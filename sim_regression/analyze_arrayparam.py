"""Measure ap_out.txt against the numbers the model cards carry.

`arrayparam.va` has two devices whose behaviour is a parameter array (VAMS-2023
3.4.4): a resistor whose I(V) curve is a fitted polynomial, and a one-pole filter
whose coefficients are handed to `laplace_nd`. The vectors live in
`arrayparam.cir` and nowhere else, so this reads them from the netlist and
requires the waveform to be what they say.

What that pins down:

  * every element of the array arrives, and in the position the card wrote it:
    the polynomial is recomputed term by term from the device's own voltage, so a
    swapped coefficient is a different curve rather than a scaled one;
  * two cards of one compiled model keep their own arrays, since the default card
    and the fitted one are in the circuit at the same time;
  * an array parameter works as an analog operator's array argument (4.5.1),
    which is what lets the filter's pole come from the card.

Both devices are checked against arithmetic recomputed here rather than against a
stored waveform. The resistor is algebraic, so that is a polynomial. The filter is
not, so the reference is the exact response of one pole to the input the waveform
itself carries: between two of the simulator's own timepoints the source is a
straight line, and

    y(h) = y0*e + u0*(1-e) + m*(h - tau*(1-e)),   e = exp(-h/tau), m = (u1-u0)/h

is what the pole does to a straight line. The error that leaves is the
simulator's own integration error.

Exits non-zero on failure.
"""

import math
import re
import sys

RS = 100.0      # the sense resistor the netlist puts in series with each device

# The currents are of order 1 mA and `wrdata` writes nine significant digits, so
# the voltages this recomputes from are already rounded to about a nanoamp's
# worth. A coefficient read from the wrong element would miss by ~1e-4 A.
TOL_I = 2e-9
# Volts on a one volt step. The pole that the fast card places differs from the
# default one by half a volt over the first few hundred nanoseconds.
TOL_V = 1e-3

# The declared defaults, which a card that sets nothing leaves in place.
C_DEFAULT = [0.0, 1.0e-3, 0.0, 0.0]
DEN_DEFAULT = [1.0, 1.0e-6]


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


cards = model_cards("arrayparam.cir")
c_def = cards.get("rdef", {}).get("c", C_DEFAULT)
c_fit = cards.get("rfit", {}).get("c", C_DEFAULT)
den_def = cards.get("fdef", {}).get("den", DEN_DEFAULT)
den_fast = cards.get("ffast", {}).get("den", DEN_DEFAULT)

rows = []
for line in open("ap_out.txt"):
    parts = line.split()
    if len(parts) < 14:
        continue
    try:
        v = [float(x) for x in parts]
    except ValueError:
        continue
    # wrdata writes a time column per variable: t v(rp), t v(rs1), t v(fp),
    # t v(rs2), t v(si), t v(od), t v(of)
    rows.append((v[0], v[1], v[3], v[5], v[7], v[9], v[11], v[13]))

if len(rows) < 1000:
    sys.exit("ap_out.txt has %d rows, expected a transient run" % len(rows))

# The polynomial, term by term, against the voltage the device itself sees.
worst_i = []
for coeffs, src_col, sense_col in ((c_def, 1, 2), (c_fit, 3, 4)):
    worst = 0.0
    for row in rows:
        v = row[src_col] - row[sense_col]
        i_meas = row[sense_col] / RS
        i_want = sum(ck * v**k for k, ck in enumerate(coeffs))
        worst = max(worst, abs(i_meas - i_want))
    worst_i.append(worst)

# The pole, against what one pole does to the input the waveform carries.
worst_v = []
for den, out_col in ((den_def, 6), (den_fast, 7)):
    tau = den[1] / den[0]
    y = 0.0
    worst = 0.0
    for prev, cur in zip(rows, rows[1:]):
        h = cur[0] - prev[0]
        if h <= 0.0:
            continue
        u0, u1 = prev[5], cur[5]
        e = math.exp(-h / tau)
        y = y * e + u0 * (1.0 - e) + (u1 - u0) / h * (h - tau * (1.0 - e))
        worst = max(worst, abs(cur[out_col] - y))
    worst_v.append(worst)

print(
    "fitted polynomial, %d coefficients: worst current error %.3e A on the "
    "default card, %.3e A on the fit" % (len(c_fit), worst_i[0], worst_i[1])
)
print(
    "pole from the card: tau %.3g s and %.3g s, worst error %.3e V and %.3e V"
    % (den_def[1] / den_def[0], den_fast[1] / den_fast[0], worst_v[0], worst_v[1])
)

bad = []
for worst, name in zip(worst_i, ("default card", "fitted card")):
    if worst > TOL_I:
        bad.append("%s: polynomial off by %.3e A" % (name, worst))
for worst, name in zip(worst_v, ("default card", "fast card")):
    if worst > TOL_V:
        bad.append("%s: pole off by %.3e V" % (name, worst))
if bad:
    sys.exit("; ".join(bad))
