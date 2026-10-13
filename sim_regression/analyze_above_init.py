"""Measure the above() run in ai_out.txt against VAMS-2023 5.10.3.2.

The clock sits at 5 V for the whole run, so it never crosses the 2.5 V threshold
in any direction. That makes one claim and one anti-claim:

  sampled   `above` fires while the initial state is solved, because the
            expression is already positive there, so `out` holds v(in) at t = 0.
  held      `out` must then stay flat, because no crossing ever follows.

`cross` in the same circuit leaves `out` at 0 forever, which is the LRM's stated
reason for `above` existing. Exits non-zero on failure.
"""

import sys

SAMPLED = 1.0       # v(in) at t = 0
SAMPLE_TOL = 1e-3
DRIFT_TOL = 5e-3
SETTLE = 100e-9     # transition(state, 0, 10n) plus a step or two

rows = []
for line in open("ai_out.txt"):
    p = line.split()
    if len(p) < 6:
        continue
    try:
        rows.append((float(p[0]), float(p[1]), float(p[3]), float(p[5])))
    except ValueError:
        continue

if not rows:
    sys.exit("ai_out.txt has no data -- did ngspice run?")

print(f"rows: {len(rows)}   t: {rows[0][0]:.3g} .. {rows[-1][0]:.3g} s")

seg = [r for r in rows if r[0] >= SETTLE]
outs = [r[2] for r in seg]
ins = [r[1] for r in seg]
drift = max(outs) - min(outs)
err = max(abs(v - SAMPLED) for v in outs)

print(f"v(smpl)                 : {min(r[3] for r in rows):.3f} .. {max(r[3] for r in rows):.3f} V"
      "  (never crosses 2.5 V)")
print(f"v(in) over the run      : {min(ins):.3f} .. {max(ins):.3f} V")
print(f"v(out) after settling   : {min(outs):.4f} .. {max(outs):.4f} V")
print()
print(f"held-vs-sampled error   : {err:.2e} V  (tol {SAMPLE_TOL:.0e}, expected {SAMPLED} V)")
print(f"drift over the whole run: {drift:.2e} V  (tol {DRIFT_TOL:.0e})")
print()

if err > SAMPLE_TOL and max(abs(v) for v in outs) < SAMPLE_TOL:
    sys.exit(
        "FAIL: v(out) never leaves 0 -- above() did not fire at initialization, "
        "which is the one thing it is for"
    )
if drift > DRIFT_TOL:
    sys.exit(f"FAIL: v(out) drifts {drift:.4f} V -- it is tracking the input, not holding")
if err > SAMPLE_TOL:
    sys.exit(f"FAIL: held value is {outs[0]:.4f} V, expected {SAMPLED} V")

print("PASS: above() sampled at initialization and held -- LRM behaviour")
