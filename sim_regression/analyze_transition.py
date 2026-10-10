"""Measure the transition run in tr_out.txt against VAMS-2023 4.5.8.

The model drives every input change from a `timer` event, so each corner of the
output sits at an instant arithmetic can predict. With td = 2 ns, tr = 3 ns,
tf = 1 ns and a 0 -> 1 -> 0 input at 10 ns and 30 ns, `v(ramp)` must be:

    flat at 0          up to 12 ns     (the transport delay)
    linear 0 -> 1      12 .. 15 ns     (rise_time, reaching 1 exactly)
    flat at 1          15 .. 32 ns
    linear 1 -> 0      32 .. 33 ns     (fall_time)
    flat at 0          after 33 ns

Four claims:

  corners   each corner is an exact timepoint in the output, which is the clause
            asking for them: "causes the simulator to place time-points at both
            corners of a transition". Nothing else in the netlist marks these
            instants, so a point can only land there via `$bound_step`.
  values    the output at each corner is the value the ramp should have reached.
            This is what the first-order lag could not do: it was 1 - 1/e at the
            trailing corner instead of 1.
  linear    the samples strictly inside a ramp sit on the straight line between
            its corners, not on an exponential.
  glitch    a 1 ns pulse, shorter than both td and tr, still comes out -- `td` is
            a transport delay, not an inertial one -- and comes out as a partial
            swing. It reaches tr/3 of full scale, then falls from the old
            destination as origin, so the fall takes tf/3.

Exits non-zero on failure.
"""

import sys

TD, TR, TF = 2e-9, 3e-9, 1e-9

# (time, expected v(ramp)) at every corner of the ramp channel.
RAMP_CORNERS = [
    (10e-9 + TD, 0.0),
    (10e-9 + TD + TR, 1.0),
    (30e-9 + TD, 1.0),
    (30e-9 + TD + TF, 0.0),
]

# The glitch channel: a pulse from 50 ns to 51 ns, delayed by td.
GLITCH_START = 50e-9 + TD                      # 52 ns, starts rising
GLITCH_PEAK_T = 51e-9 + TD                     # 53 ns, the rise is interrupted
GLITCH_PEAK_V = (51e-9 - 50e-9) / TR           # 1/3: how far 1 ns of a 3 ns rise got
# 4.5.8 computes the interrupted fall from the old destination (1.0) as origin, so
# the slope is the full -1/tf and covers only the third that was climbed.
GLITCH_END = GLITCH_PEAK_T + GLITCH_PEAK_V * TF

TIME_TOL = 1e-18        # corners are requested exactly, not within a tolerance
VALUE_TOL = 1e-9

# `wrdata` prints the time column with 9 significant digits, so a sample's time is
# known to about half of its last digit. On a ramp that uncertainty shows up in the
# value as |slope| times it, which near 30 ns on the 1 ns fall is already ~5e-8 --
# larger than any tolerance worth setting on the model. So the linearity check
# derives its tolerance from the printed resolution instead of fixing one.
def line_tol(t, slope):
    from math import floor, log10
    resolution = 10 ** (floor(log10(abs(t))) - 8) if t else 0.0
    return abs(slope) * resolution + 1e-12

rows = []
for line in open("tr_out.txt"):
    p = line.split()
    if len(p) < 6:
        continue
    try:
        rows.append((float(p[0]), float(p[1]), float(p[3]), float(p[5])))
    except ValueError:
        continue

if not rows:
    sys.exit("tr_out.txt has no data -- did ngspice run?")

times = [r[0] for r in rows]
ramp = {r[0]: r[1] for r in rows}
glitch = {r[0]: r[2] for r in rows}

print(f"rows: {len(rows)}   t: {times[0]:.3g} .. {times[-1]:.3g} s")
print()

bad = []


def nearest(t):
    return min(times, key=lambda x: abs(x - t))


print("corners placed as exact timepoints:")
for t, _ in RAMP_CORNERS:
    hit = any(abs(x - t) <= TIME_TOL for x in times)
    print(f"  {t*1e9:6.2f} ns : {'yes' if hit else 'NO (nearest %.6f ns)' % (nearest(t)*1e9)}")
    if not hit:
        bad.append(f"no timepoint on the corner at {t*1e9:.3f} ns")
print()

print("value at each corner:")
for t, want in RAMP_CORNERS:
    got = ramp[nearest(t)]
    ok = abs(got - want) <= VALUE_TOL
    print(f"  {t*1e9:6.2f} ns : {got:.9f}  want {want:.1f}  {'ok' if ok else 'FAIL'}")
    if not ok:
        bad.append(f"v(ramp) at {t*1e9:.3f} ns is {got:.9f}, want {want:.1f}")
print()

print("samples inside each ramp sit on the straight line:")
for (t_a, v_a), (t_b, v_b) in [(RAMP_CORNERS[0], RAMP_CORNERS[1]),
                               (RAMP_CORNERS[2], RAMP_CORNERS[3])]:
    slope = (v_b - v_a) / (t_b - t_a)
    inside = [t for t in times if t_a < t < t_b]
    # Rank by how far each sample exceeds its own tolerance, so the one reported is
    # the one that actually decides the verdict.
    scored = [
        (abs(ramp[t] - (v_a + slope * (t - t_a))) - line_tol(t, slope), t)
        for t in inside
    ]
    worst_t = max(scored)[1] if scored else None
    if worst_t is not None:
        worst = abs(ramp[worst_t] - (v_a + slope * (worst_t - t_a)))
        worst_tol = line_tol(worst_t, slope)
    label = f"{t_a*1e9:.0f} -> {t_b*1e9:.0f} ns"
    if worst_t is None:
        print(f"  {label:>16} : no interior samples")
        bad.append(f"the ramp {label} has no interior samples to check")
        continue
    ok = worst <= worst_tol
    print(f"  {label:>16} : {len(inside):3d} samples, worst |err| {worst:.3e} "
          f"at {worst_t*1e9:.3f} ns (tol {worst_tol:.3e})  {'ok' if ok else 'FAIL'}")
    if not ok:
        bad.append(f"the ramp {label} is off its line by {worst:.3e}, tol {worst_tol:.3e}")
print()

print("a pulse shorter than td and tr survives as a partial swing:")
peak = max(glitch.values())
ok_peak = abs(peak - GLITCH_PEAK_V) <= VALUE_TOL
print(f"  peak              : {peak:.9f}  want {GLITCH_PEAK_V:.9f}  "
      f"{'ok' if ok_peak else 'FAIL'}")
if not ok_peak:
    bad.append(f"the glitch peaks at {peak:.9f}, want {GLITCH_PEAK_V:.9f}")

flat_before = [glitch[t] for t in times if t < GLITCH_START - TIME_TOL]
ok_quiet = max(abs(v) for v in flat_before) <= VALUE_TOL
print(f"  quiet before {GLITCH_START*1e9:.0f}ns : max |v| {max(abs(v) for v in flat_before):.3e}  "
      f"{'ok' if ok_quiet else 'FAIL'}")
if not ok_quiet:
    bad.append("the glitch channel moves before the transport delay has elapsed")

back_down = [t for t in times if t > GLITCH_PEAK_T and abs(glitch[t]) <= VALUE_TOL]
if not back_down:
    print("  returns to 0      : NO")
    bad.append("the glitch never returns to 0")
else:
    arrived = min(back_down)
    ok_end = abs(arrived - GLITCH_END) <= 1e-12
    print(f"  returns to 0      : {arrived*1e9:.6f} ns  want {GLITCH_END*1e9:.6f} ns  "
          f"{'ok' if ok_end else 'FAIL'}")
    if not ok_end:
        bad.append(f"the glitch lands at {arrived*1e9:.6f} ns, want {GLITCH_END*1e9:.6f} ns")
print()

if bad:
    for b in bad:
        print("FAIL:", b)
    sys.exit(1)

print("transition: all claims hold")
