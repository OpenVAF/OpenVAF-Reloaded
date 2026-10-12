"""Measure the crosstalk matrix in xt_out.txt against the arithmetic it describes.

`xtalk.va` accumulates a 3x3 matrix product through a double loop whose indices
are both computed, so every timepoint goes through the flat-position arithmetic
that backs a multi-dimensional array (VAMS-2023 3.2). The matrix is asymmetric,
which is what makes the check sharp: a transposed dimension or a stride taken
from the wrong dimension gives a different waveform, not a differently scaled one.

The outputs are algebraic in the inputs, so this compares them against the matrix
product recomputed here, point for point, rather than against a stored waveform.

`pick` is `c[i][j]` with the two indices taken from the signs of two inputs: a
four-level staircase whose level says which element was read, and the only part of
the case where the index is not a loop variable.

Exits non-zero on failure.
"""

import sys

K = 0.01            # the coupling parameter the netlist sets
# `wrdata` writes nine significant digits, so the inputs this recomputes from are
# already rounded: on values of order 1 that is a few parts in 1e9. The error a
# wrong element would make is the size of a coupling term, 1e-2 of the input, so
# there are seven orders of magnitude between the two.
TOL = 1e-7

# c[i][j] as the model builds it.
C = [[1.0 if i == j else K * (10.0 * i + j) for j in range(3)] for i in range(3)]

rows = []
for line in open("xt_out.txt"):
    p = line.split()
    if len(p) < 14:
        continue
    try:
        # wrdata writes a time column per variable: t out0, t out1, t out2,
        # t pick, t in0, t in1, t in2
        rows.append(
            (
                float(p[0]),
                [float(p[1]), float(p[3]), float(p[5])],
                float(p[7]),
                [float(p[9]), float(p[11]), float(p[13])],
            )
        )
    except ValueError:
        continue

if len(rows) < 100:
    print(f"only {len(rows)} rows in xt_out.txt")
    sys.exit(1)

fails = []

worst = [0.0, 0.0, 0.0]
worst_at = [0.0, 0.0, 0.0]
for t, out, _pick, inp in rows:
    for i in range(3):
        want = sum(C[i][j] * inp[j] for j in range(3))
        err = abs(out[i] - want)
        if err > worst[i]:
            worst[i], worst_at[i] = err, t

for i in range(3):
    print(f"worst |v(out{i}) - row {i} of c| : {worst[i]:.3e} V at {worst_at[i] * 1e6:.3f} us")
    if worst[i] > TOL:
        fails.append(f"v(out{i}) is off the matrix product by {worst[i]:.3e} V")

# -- the staircase: which element the computed indices reached ---------------
worst_pick = 0.0
worst_pick_at = 0.0
levels = {}
for t, _out, pick, inp in rows:
    i = 2 if inp[0] > 0.0 else 0
    j = 1 if inp[1] > 0.0 else 0
    want = C[i][j]
    err = abs(pick - want)
    levels[(i, j)] = want
    if err > worst_pick:
        worst_pick, worst_pick_at = err, t

print(f"worst |v(pick) - c[i][j]|      : {worst_pick:.3e} V at {worst_pick_at * 1e6:.3f} us")
print(f"elements reached               : {len(levels)} of 4 ({sorted(levels.values())})")
if worst_pick > TOL:
    fails.append(f"v(pick) is off c[i][j] by {worst_pick:.3e} V")
if len(levels) < 4:
    fails.append(f"only {len(levels)} of the 4 index combinations occurred")

if fails:
    print()
    for fail in fails:
        print(f"FAIL: {fail}")
    sys.exit(1)
