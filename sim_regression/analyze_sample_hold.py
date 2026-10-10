"""Measure the sample-and-hold run in sh_out.txt against VAMS-2023 5.10.3.1.

Two independent claims, both of which a working `@(cross)` has to satisfy:

  hold      `out` must not move between sample edges. This is the one that fails
            outright when `cross` takes no part in scheduling: the body runs on
            every evaluation, so `out` tracks the input.
  accuracy  the held value must be `v(in)` at the crossing *instant*, found by
            interpolating the clock through the threshold. Detecting the crossing
            on the first accepted step after it puts a floor under this: the event
            lands up to one timestep late, and the input has moved on by then.

Exits non-zero if either check fails.
"""

import sys

THRESH = 2.5
# `transition(state, 0, 10n)` is a continuous lag, so it approaches the sampled
# value asymptotically rather than arriving in exactly 10 ns. 100 ns leaves ~2e-05 V
# of tail, which was invisible while the event itself was landing ~150 ns late and
# swamping it. Wait long enough for the filter to be done, so what is left measures
# the event timing and not the filter.
SETTLE = 500e-9
# v(in) ramps at 0.5 V/us, and `cross` now steers the timestep onto the crossing
# (VAMS-2023 5.10.3.1), so what is left is the curvature error of that prediction.
SAMPLE_TOL = 1e-6
DRIFT_TOL = 5e-3

rows = []
for line in open("sh_out.txt"):
    p = line.split()
    if len(p) < 6:
        continue
    try:
        rows.append((float(p[0]), float(p[1]), float(p[3]), float(p[5])))
    except ValueError:
        continue

if not rows:
    sys.exit("sh_out.txt has no data -- did ngspice run?")

print(f"rows: {len(rows)}   t: {rows[0][0]:.3g} .. {rows[-1][0]:.3g} s")

# Rising crossings of the threshold, interpolated to the instant the LRM samples
# at, together with v(in) there -- the value `out` should hold until the next one.
edges = []
for i in range(1, len(rows)):
    (t0, vin0, _, vs0), (t1, vin1, _, vs1) = rows[i - 1], rows[i]
    if vs0 < THRESH <= vs1:
        f = (THRESH - vs0) / (vs1 - vs0)
        edges.append((t0 + f * (t1 - t0), vin0 + f * (vin1 - vin0)))

print(f"rising sample edges at: {', '.join(f'{t*1e6:.3f}us' for t, _ in edges)}")

track_err = max(abs(vout - vin) for _, vin, vout, _ in rows)
print(f"\nmax |v(out) - v(in)| over the run : {track_err:.4f} V")

print("\nper hold interval (settling excluded):")
worst_drift = 0.0
worst_sample = 0.0
for k, (start, sampled) in enumerate(edges):
    end = edges[k + 1][0] if k + 1 < len(edges) else rows[-1][0]
    seg = [r for r in rows if start + SETTLE <= r[0] <= end - 1e-9]
    if len(seg) < 3:
        continue
    outs = [r[2] for r in seg]
    drift = max(outs) - min(outs)
    err = max(abs(v - sampled) for v in outs)
    worst_drift = max(worst_drift, drift)
    worst_sample = max(worst_sample, err)
    print(
        f"  {start*1e6:6.3f}us -> {end*1e6:6.3f}us : "
        f"v(out) {min(outs):.4f}..{max(outs):.4f} (drift {drift:.4f} V), "
        f"sampled v(in) = {sampled:.4f} (err {err:.1e} V)"
    )

print(f"\nworst drift within a hold interval : {worst_drift:.2e} V  (tol {DRIFT_TOL:.0e})")
print(f"worst held-vs-sampled error       : {worst_sample:.2e} V  (tol {SAMPLE_TOL:.0e})")
print()

# The three ways this goes wrong have different causes, so name them apart.
outs_all = [r[2] for r in rows]
never_moved = max(outs_all) - min(outs_all) <= SAMPLE_TOL

failures = []
if not edges:
    failures.append("no rising sample edges found on v(smpl)")
elif never_moved:
    failures.append(
        f"v(out) never moves (stays at {outs_all[-1]:.4f} V) -- the crossing is "
        "never detected, so the body never runs"
    )
elif worst_drift > DRIFT_TOL:
    failures.append(
        f"out TRACKS the input (drift {worst_drift:.4f} V) -- the event takes no "
        "part in scheduling, so the body runs on every evaluation"
    )
elif worst_sample > SAMPLE_TOL:
    failures.append(
        f"held value is off by {worst_sample:.2e} V -- the event fires, but lands "
        "too long after the crossing"
    )

if failures:
    for f in failures:
        print(f"FAIL: {f}")
    sys.exit(1)

print("PASS: out holds each sampled value between edges -- LRM behaviour")
