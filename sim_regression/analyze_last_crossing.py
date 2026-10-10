"""Measure the last_crossing run in lc_out.txt against VAMS-2023 4.5.10.

The input is a 200 kHz sine, so its rising zero crossings sit at exactly 0, 5, 10,
15 and 20 us. The one at t = 0 cannot be detected -- interpolation needs a point
on each side -- so the reading should step through 5, 10, 15, 20 us and read
negative before the first of them.

Asserted:

  negative  "Before the expression crosses zero (0) for the first time, the
            last_crossing() function returns a negative value."
  accuracy  each reading matches the true crossing time. 4.5.10 does not control
            the timestep, so this is purely what linear interpolation gives across
            the step the integrator took.

  period    4.5.10's own use of the function, from the same clause: the distance
            between consecutive rising crossings. It reads `latest` inside the
            `@(cross)` handler *before* the statement that assigns it, so it only
            comes out right if analog variables keep their value between
            evaluations. That is the second thing this test pins down.

Exits non-zero on failure.
"""

import sys

FREQ = 200e3
EXPECTED = [5e-6, 10e-6, 15e-6, 20e-6]
PERIOD = 5e-6
TIME_TOL = 1e-11   # 10 ps; the measured error is ~0.2 ps

rows = []
for line in open("lc_out.txt"):
    p = line.split()
    if len(p) < 6:
        continue
    try:
        rows.append((float(p[0]), float(p[1]), float(p[3]), float(p[5])))
    except ValueError:
        continue

if not rows:
    sys.exit("lc_out.txt has no data -- did ngspice run?")

print(f"rows: {len(rows)}   t: {rows[0][0]:.3g} .. {rows[-1][0]:.3g} s")
print(f"input: {FREQ/1e3:.0f} kHz sine, rising zero crossings at 0, 5, 10, 15, 20 us")
print()

# Before the first detectable crossing at 5 us.
early = [r for r in rows if r[0] < 5e-6 - 1e-9]
if not early:
    sys.exit("no samples before the first crossing")
worst_early = max(r[1] for r in early)
print(f"reading before the first crossing : max {worst_early:.4g}  (must be negative)")
if worst_early >= 0.0:
    sys.exit(f"FAIL: reading reached {worst_early:.6g} before any crossing; 4.5.10 requires a negative value")

# The reading settles to each crossing time; sample it midway to the next one.
print()
print("reading after each crossing (sampled midway to the next):")
worst = 0.0
for k, want in enumerate(EXPECTED):
    probe = want + 2.5e-6
    seg = [r for r in rows if want + 1e-7 <= r[0] <= probe]
    if not seg:
        continue
    got = seg[-1][1]
    err = abs(got - want)
    worst = max(worst, err)
    print(f"  crossing {k+1} at {want*1e6:5.2f}us : read {got*1e6:.9f}us   err {err:.2e} s")

print()
print(f"worst crossing-time error : {worst:.2e} s  (tol {TIME_TOL:.0e})")

# The period, measurable only from the second crossing on -- which is why 4.5.10's
# example guards it with `if (crossings < 2)`.
late = [r for r in rows if r[0] > 12e-6]
if not late:
    sys.exit("no samples after the second crossing")
per_err = max(abs(r[2] - PERIOD) for r in late)
print(f"period after 2 crossings  : {late[-1][2]*1e6:.6f}us, want {PERIOD*1e6:.6f}us"
      f"   err {per_err:.2e} s")

print()
if worst > TIME_TOL:
    sys.exit(f"FAIL: crossing time off by {worst:.2e} s")
if per_err > TIME_TOL:
    sys.exit(
        f"FAIL: period off by {per_err:.2e} s -- `latest` is not keeping its value "
        "between evaluations, so the handler reads it before it is assigned"
    )

print("PASS: crossing times interpolated, and the period measured across evaluations")
