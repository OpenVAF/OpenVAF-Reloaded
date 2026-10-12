"""Measure the default transition time in dir_out.txt against VAMS-2023 10.3.

`directives.va` puts the same bare filter, `transition(x)`, on both sides of a
`` `default_transition 1u `` directive, and a filter that asks for a zero rise
time next to one that names 1 us outright. 4.5.8 decides all four:

  bare, zero  below the directive, "unspecified or equal to zero (0.0)", so both
              take the 1 us the directive names
  named       the same filter with the time written out, which the other two have
              to agree with sample for sample
  step        above the directive, where no default is in force, so it gets "a
              negligible, but non-zero, transition time" and steps

The measurement that distinguishes the two is the 10% to 90% rise time, which is
0.8 us for a 1 us ramp and one timestep for a step. It does not depend on when the
ramp started, which is a question about the input and the solver's grid rather than
about the directive.

Exits non-zero on failure.
"""

import sys

RISE = 1e-6        # the time `default_transition names in the model
TOL = 1e-9         # the three filters below the directive are the same code
RISE_TOL = 0.02    # of the rise time, so 16 ns of 800 ns
STEP_MAX = 50e-9   # a "negligible" transition resolved on a 10 ns grid

rows = []
for line in open("dir_out.txt"):
    p = line.split()
    if len(p) < 10:
        continue
    try:
        # wrdata writes a time column per variable: t bare, t zero, t named,
        # t step, t in
        rows.append(
            (float(p[0]), float(p[1]), float(p[3]), float(p[5]), float(p[7]), float(p[9]))
        )
    except ValueError:
        continue

if len(rows) < 100:
    print(f"only {len(rows)} rows in dir_out.txt")
    sys.exit(1)

fails = []


def crossing(col, level):
    """The first time the column reaches `level`, interpolated.

    The waveform is piecewise linear and the corners are time-points the filter
    asked for, so interpolation between two samples is exact rather than an
    approximation of the curve between them.
    """
    prev = None
    for row in rows:
        t, val = row[0], row[col]
        if val >= level:
            if prev is None or val == prev[1]:
                return t
            t0, v0 = prev
            return t0 + (level - v0) * (t - t0) / (val - v0)
        prev = (t, val)
    return None


def rise_time(col):
    lo = crossing(col, 0.1)
    hi = crossing(col, 0.9)
    if lo is None or hi is None:
        return None
    return hi - lo


# -- the three filters below the directive are one filter -------------------
for name, col in (("bare", 1), ("zero", 2)):
    worst = max(abs(row[col] - row[3]) for row in rows)
    print(f"max |v({name}) - v(named)|     : {worst:.2e} V  (tol {TOL:.0e})")
    if worst > TOL:
        fails.append(f"v({name}) differs from v(named) by {worst:.3e} V")

# -- and they ramp over the time the directive names ------------------------
want = 0.8 * RISE
for name, col in (("bare", 1), ("zero", 2), ("named", 3)):
    tr = rise_time(col)
    if tr is None:
        fails.append(f"v({name}) never reached 90%")
        continue
    print(f"v({name}) rise time 10-90%    : {tr:.4e} s  (want {want:.4e})")
    if abs(tr - want) > RISE_TOL * want:
        fails.append(f"v({name}) rise time is {tr:.4e} s, not {want:.4e} s")

# -- while the module above the directive still steps -----------------------
tr = rise_time(4)
if tr is None:
    fails.append("v(step) never reached 90%")
else:
    print(f"v(step) rise time 10-90%     : {tr:.4e} s  (want < {STEP_MAX:.0e})")
    if tr > STEP_MAX:
        fails.append(f"v(step) took {tr:.4e} s, so the directive reached above itself")

# -- and all four arrive --------------------------------------------------
settled = rows[-1]
for name, col in (("bare", 1), ("zero", 2), ("named", 3), ("step", 4)):
    print(f"v({name}) at {settled[0]:.2e} s      : {settled[col]:.6f} V")
    if abs(settled[col] - 1.0) > 1e-6:
        fails.append(f"v({name}) ended at {settled[col]:.6f} V, not 1 V")

if fails:
    print()
    for fail in fails:
        print(f"FAIL: {fail}")
    sys.exit(1)
