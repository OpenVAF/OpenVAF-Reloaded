"""Measure cd_out.txt against the table and the filter the model cards carry.

`carddata.va` has two devices whose numbers are nowhere in the source: a part
whose I(V) curve is a `$table_model` data source (VAMS-2023 9.21) and a filter
whose coefficients are a `zi_nd` vector (4.5.12). Both are array parameters, so
`carddata.cir` is the only place the numbers appear, and this reads them from the
netlist and requires the waveform to be what they say.

What that pins down:

  * every sample arrives in the position the card wrote it: the lookup is
    recomputed knee by knee from the device's own voltage, so a swapped sample is
    a different curve rather than a scaled one;
  * the lookup carries its derivative, because the operating point of a nonlinear
    curve in series with a sense resistor is a Newton solve: a wrong slope does
    not converge to the right current;
  * the two interpolation ends, since the ramp runs past both of them;
  * the difference equation runs on the coefficients the card set, sample for
    sample over the grid the filter asked the simulator for;
  * two cards of one compiled model keep their own numbers.

Exits non-zero on failure.
"""

import re
import sys

RS = 100.0      # the sense resistor in series with each curve
TS = 2.0e-6     # the filter's sampling period, as `carddata.va` declares it

# `wrdata` writes nine significant digits, so on a current of 10 mA the file
# itself carries about 1e-11 A, and the Newton solve adds its own residual.
TOL_I = 2e-9
# Volts on a one volt ramp, held between two samples.
TOL_V = 1e-6

# The declared defaults, which a card that sets nothing leaves in place.
VS = [0.0, 0.25, 0.5, 0.75, 1.0]
IS_DEFAULT = [0.0, 1.0e-4, 1.0e-3, 4.0e-3, 1.0e-2]
NUM_DEFAULT = [0.25]
DEN_DEFAULT = [1.0, -0.75]


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


def lookup(xs, ys, x):
    """9.21's linear interpolation with linear extrapolation off both ends."""
    n = len(xs)
    if x < xs[0]:
        i = 0
    else:
        i = n - 2
        for k in range(n - 1):
            if x <= xs[k + 1]:
                i = k
                break
    return ys[i] + (x - xs[i]) * (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i])


def recurrence(num, den, xs):
    """4.5.12's difference equation, solved for the newest output."""
    out = []
    hist_x = [0.0] * len(num)
    hist_y = [0.0] * max(len(den) - 1, 1)
    for x in xs:
        hist_x = [x] + hist_x[:-1] if len(hist_x) > 1 else [x]
        acc = sum(n * hist_x[k] for k, n in enumerate(num))
        acc -= sum(d * hist_y[k - 1] for k, d in enumerate(den) if k >= 1)
        y = acc / den[0]
        hist_y = [y] + hist_y[:-1] if len(hist_y) > 1 else [y]
        out.append(y)
    return out


cards = model_cards("carddata.cir")
is_def = cards.get("rdef", {}).get("is", IS_DEFAULT)
is_meas = cards.get("rmeas", {}).get("is", IS_DEFAULT)
num_def = cards.get("fdef", {}).get("num", NUM_DEFAULT)
den_def = cards.get("fdef", {}).get("den", DEN_DEFAULT)
num_slow = cards.get("fslow", {}).get("num", NUM_DEFAULT)
den_slow = cards.get("fslow", {}).get("den", DEN_DEFAULT)

rows = []
for line in open("cd_out.txt"):
    parts = line.split()
    if len(parts) < 12:
        continue
    try:
        v = [float(x) for x in parts]
    except ValueError:
        continue
    # wrdata writes a time column per variable: t v(rp), t v(rs1), t v(rs2),
    # t v(fi), t v(fo1), t v(fo2)
    rows.append((v[0], v[1], v[3], v[5], v[7], v[9], v[11]))

if len(rows) < 100:
    sys.exit("cd_out.txt has %d rows, expected a transient run" % len(rows))

# The measured curve, point by point against the voltage the device itself sees.
worst_i = []
for samples, sense_col in ((is_def, 2), (is_meas, 3)):
    worst = 0.0
    for row in rows:
        v_dev = row[1] - row[sense_col]
        i_meas = row[sense_col] / RS
        worst = max(worst, abs(i_meas - lookup(VS, samples, v_dev)))
    worst_i.append(worst)

# The filter, against the recurrence over the samples the waveform carries. The
# filter asks the simulator for a timepoint on each sample instant, so the input
# at `m*TS` is in the file; the output is read half a period later, clear of the
# corner it transitions over.
def at(t):
    return min(range(len(rows)), key=lambda i: abs(rows[i][0] - t))


m = 0
sampled = []
while (m + 1) * TS <= rows[-1][0]:
    sampled.append(rows[at(m * TS)][4])
    m += 1
worst_v = []
for num, den, out_col in ((num_def, den_def, 5), (num_slow, den_slow, 6)):
    want = recurrence(num, den, sampled)
    worst = 0.0
    for m, y in enumerate(want):
        got = rows[at(m * TS + 0.5 * TS)][out_col]
        worst = max(worst, abs(got - y))
    worst_v.append(worst)

print(
    "a measured curve from the model card, %d samples: worst current error %.3e A "
    "on the default card, %.3e A on the measured one" % (len(is_def), worst_i[0], worst_i[1])
)
print(
    "a filter from the model card, over %d samples of the grid it asked for: worst "
    "error %.3e V and %.3e V" % (len(sampled), worst_v[0], worst_v[1])
)

bad = []
for worst, name in zip(worst_i, ("default card", "measured card")):
    if worst > TOL_I:
        bad.append("%s: the curve is off by %.3e A" % (name, worst))
for worst, name in zip(worst_v, ("default card", "slow card")):
    if worst > TOL_V:
        bad.append("%s: the filter is off by %.3e V" % (name, worst))
if bad:
    sys.exit("; ".join(bad))
