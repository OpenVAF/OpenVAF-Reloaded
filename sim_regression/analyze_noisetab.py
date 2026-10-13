"""Measure nt_def.txt and nt_set.txt against the noise tables the cards carry.

`noisetab.va` contributes two noise sources from the same numbers (VAMS-2023
4.6.3): one whose table is an array parameter, which `noisetab.cir` can set, and
one whose table is an array literal, which is compiled into the model. The card's
vector lives in the netlist and nowhere else, so this reads it from there and
requires the spectrum to be what it says.

What that pins down:

  * the pairs arrive in the position the card wrote them, since the reference is
    recomputed knee by knee rather than compared against a stored spectrum;
  * a table read at run time interpolates identically to the same table compiled
    in, which is the one thing a new code path for an old operator has to prove;
  * a card that sets the array moves the run-time spectrum and leaves the
    compiled-in one alone, so the two are not sharing anything;
  * two cards of one compiled model keep their own tables.

The reference is the interpolation 4.6.3's table defines: power linear in
log10(frequency) between two pairs, and the nearest endpoint's power outside
them. The circuit turns a noise current into a voltage density through a known
impedance (1 kohm in parallel with the device's 1 mS), so

    onoise(f) = sqrt(S(f)) * 500

with nothing in it that the simulator has to integrate.

Exits non-zero on failure.
"""

import math
import re
import sys

# The netlist's load and the declared conductance: a noise current at the output
# node sees the two in parallel.
R_LOAD = 1000.0
G_DEV = 1e-3
Z_OUT = 1.0 / (G_DEV + 1.0 / R_LOAD)

# `wrdata` writes nine significant digits, so two numbers that agree to a
# relative 1e-8 agree as far as the file can say.
TOL = 1e-8

# The declared default, which a card that sets nothing leaves in place.
SN_DEFAULT = [1.0, 1e-20, 1.0e2, 1e-22, 1.0e4, 1e-24, 1.0e6, 1e-24]


def model_cards(path):
    """{card: {parameter: [values]}} for every `.model` line that sets a vector."""
    cards = {}
    for line in open(path):
        m = re.match(r"\.model\s+(\S+)\s+(\S+)\s*(.*)", line.strip(), re.I)
        if not m:
            continue
        name, _module, rest = m.groups()
        cards[name.lower()] = {
            pm.group(1).lower(): [float(x) for x in pm.group(2).split()]
            for pm in re.finditer(r"(\w+)\s*=\s*\[([^\]]*)\]", rest)
        }
    return cards


def spectrum(vals, freq):
    """The power 4.6.3's table gives at `freq`, from its flat (f, power) vector."""
    pairs = sorted(zip(vals[0::2], vals[1::2]))
    x = [math.log10(f) for f, _ in pairs]
    y = [p for _, p in pairs]
    lx = math.log10(freq)
    if lx <= x[0]:
        return y[0]
    for i in range(len(x) - 1):
        if lx < x[i + 1]:
            return y[i] + (y[i + 1] - y[i]) / (x[i + 1] - x[i]) * (lx - x[i])
    return y[-1]


def read(path):
    """[(frequency, run-time table, compiled-in table)] from a `wrdata` file."""
    rows = []
    for line in open(path):
        parts = line.split()
        # wrdata writes a frequency column per vector: f card, f fixed.
        if len(parts) < 4:
            continue
        try:
            v = [float(x) for x in parts]
        except ValueError:
            continue
        rows.append((v[0], v[1], v[3]))
    return rows


cards = model_cards("noisetab.cir")
sn_def = cards.get("mdef", {}).get("sn", SN_DEFAULT)
sn_set = cards.get("mset", {}).get("sn", SN_DEFAULT)

default = read("nt_def.txt")
carded = read("nt_set.txt")
if len(default) < 50 or len(carded) != len(default):
    sys.exit(
        "expected two noise sweeps of the same length, got %d and %d rows"
        % (len(default), len(carded))
    )

# The table a card can set, against the pairs that card carries.
worst_card = 0.0
worst_fixed = 0.0
for vals, rows in ((sn_def, default), (sn_set, carded)):
    for freq, card, _fixed in rows:
        want = math.sqrt(spectrum(vals, freq)) * Z_OUT
        worst_card = max(worst_card, abs(card - want) / want)
for rows in (default, carded):
    for freq, _card, fixed in rows:
        want = math.sqrt(spectrum(SN_DEFAULT, freq)) * Z_OUT
        worst_fixed = max(worst_fixed, abs(fixed - want) / want)

# On the default card the two tables are the same numbers, so the two sources
# have to agree exactly: the run-time interpolation is the compiled-in one.
same = sum(1 for _f, card, fixed in default if card == fixed)
worst_same = max(abs(card - fixed) for _f, card, fixed in default)

# The card's table is four times the power, so twice the density, and the
# compiled-in source has to be unmoved by it.
worst_ratio = max(abs(card / fixed - 2.0) for _f, card, fixed in carded)
worst_immune = max(
    abs(c[2] - d[2]) for c, d in zip(carded, default)
)

print(
    "a noise table from the model card, %d pairs over %d frequencies: worst "
    "error %.3e relative" % (len(sn_def) // 2, len(default), worst_card)
)
print(
    "the same table compiled in: worst error %.3e relative, and %d of %d "
    "frequencies identical to the one read at run time (worst difference %.3e)"
    % (worst_fixed, same, len(default), worst_same)
)
print(
    "the card's own table: %.3gx the density of the compiled-in source "
    "(worst error %.3e), which the card leaves where it was (%.3e)"
    % (2.0, worst_ratio, worst_immune)
)

bad = []
if worst_card > TOL:
    bad.append("the table from the card is off by %.3e relative" % worst_card)
if worst_fixed > TOL:
    bad.append("the compiled-in table is off by %.3e relative" % worst_fixed)
if same != len(default):
    bad.append(
        "%d of %d frequencies differ between the two tables (worst %.3e)"
        % (len(default) - same, len(default), worst_same)
    )
if worst_ratio > TOL:
    bad.append("the card's table is off by %.3e of the expected ratio" % worst_ratio)
if worst_immune != 0.0:
    bad.append("the card moved the compiled-in table by %.3e" % worst_immune)
if bad:
    sys.exit("; ".join(bad))
