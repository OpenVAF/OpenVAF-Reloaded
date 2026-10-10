"""Measure the timer run in tm_out.txt against VAMS-2023 5.10.3.3.

`timer(1u, 2u)` schedules events at 1, 3, 5, 7 and 9 us. Nothing in the netlist
marks those instants -- there is no clock node -- so the only way a timepoint
lands on one is `$bound_step` asking for it. Three claims:

  placed    each event instant is an exact timepoint in the output. This is the
            part the mock simulator cannot test: it is the solver being steered.
  sampled   the value held after each event is `v(in)` at that instant. The input
            ramps 0.5 V/us from 0, so event k sits at 0.5, 1.5, 2.5, 3.5, 4.5 V.
  counted   exactly one event fires per period -- no double-firing across the
            Newton iterations of the step the event lands on, and none missed.

Exits non-zero on failure.
"""

import sys

EVENTS = [1e-6, 3e-6, 5e-6, 7e-6, 9e-6]
SLOPE = 0.5e6          # V/s, from PWL(0 0 10u 5)
VALUE_TOL = 1e-9

rows = []
for line in open("tm_out.txt"):
    p = line.split()
    if len(p) < 6:
        continue
    try:
        rows.append((float(p[0]), float(p[1]), float(p[3]), float(p[5])))
    except ValueError:
        continue

if not rows:
    sys.exit("tm_out.txt has no data -- did ngspice run?")

print(f"rows: {len(rows)}   t: {rows[0][0]:.3g} .. {rows[-1][0]:.3g} s")
print()

times = [r[0] for r in rows]
missing = [t for t in EVENTS if t not in times]
print("event instants placed as exact timepoints:")
for t in EVENTS:
    print(f"  {t*1e6:5.1f}us : {'yes' if t in times else 'NO'}")

print()
print("value held after each event:")
worst = 0.0
for k, t in enumerate(EVENTS):
    want = SLOPE * t
    end = EVENTS[k + 1] if k + 1 < len(EVENTS) else rows[-1][0]
    seg = [r for r in rows if t + 1e-9 <= r[0] <= end - 1e-9]
    if not seg:
        continue
    got = seg[-1][1]
    ticks = seg[-1][2]
    err = abs(got - want)
    worst = max(worst, err)
    flag = "" if abs(ticks - (k + 1)) < 0.5 else f"   TICKS={ticks:.0f}, want {k+1}"
    print(f"  after {t*1e6:5.1f}us : v(out)={got:.6f}  want {want:.6f}  "
          f"ticks={ticks:.0f}{flag}")

final_ticks = rows[-1][2]
print()
print(f"worst held-value error : {worst:.2e} V  (tol {VALUE_TOL:.0e})")
print(f"total events fired     : {final_ticks:.0f}  (want {len(EVENTS)})")
print()

if missing:
    sys.exit(
        "FAIL: no timepoint at "
        + ", ".join(f"{t*1e6:.1f}us" for t in missing)
        + " -- bound_step did not steer the solver onto the event"
    )
if worst > VALUE_TOL:
    sys.exit(f"FAIL: held value off by {worst:.2e} V")
if abs(final_ticks - len(EVENTS)) > 0.5:
    sys.exit(
        f"FAIL: {final_ticks:.0f} events fired, want {len(EVENTS)} -- the timer is "
        "double-firing or missing events"
    )

print("PASS: events placed, sampled and counted exactly -- LRM behaviour")
