"""Measure the global events in ge_out.txt against VAMS-2023 5.10.2 and 5.10.1.

The model samples a bit when either the initial step or an upward crossing of
`smpl` occurs, and sets two flags from analysis lists -- one naming the analysis
this is, one naming an analysis it is not.

  sampler  the sampled bit may change only where a clock edge is: 2 us and 8 us.
           V(in) is 1 V at the first and 4 V at the second, so out is 0 and then
           1. An edge at 5 us instead, where V(in) crosses the threshold, is the
           body running unconditionally -- what an `initial_step` that
           contributes no condition to the OR leaves behind.

  tran     v(ptran) is 1: @(initial_step("tran")) ran in a transient analysis.

  ac       v(pac) is 0: neither @(initial_step("ac")) nor @(final_step("ac")) ran
           in an analysis their list does not name.

v(pstatic) is reported and not asserted. It counts @(initial_step("static")), and
what it reads says something about the simulator rather than the compiler:
ngspice sets ANALYSIS_STATIC on the first Newton iteration of the initial step
and ANALYSIS_TRAN on the rest, so a body gated on "static" runs on one iteration
and the iterations after it write the retained value back over what it did. The
names that name an analysis -- "tran", "ac", "dc", "noise" -- are the ones held
steady for a whole run, and they are the ones to put in an analysis list.

Exits non-zero on failure.
"""

import sys

THRESH = 2.5
EDGE = 8e-6         # the clock edge the bit is expected to change on
WRONG_EDGE = 5e-6   # where V(in) crosses the threshold, and where a tracking
                    # body would put its edge instead
SETTLE = 100e-9     # transition(vout, 0, 10n) plus a step or two
TOL = 1e-3

rows = []
for line in open("ge_out.txt"):
    p = line.split()
    if len(p) < 12:
        continue
    try:
        # wrdata writes a time column per variable: t in, t out, t smpl, and then
        # the three flags
        rows.append(
            (
                float(p[0]),
                float(p[1]),
                float(p[3]),
                float(p[5]),
                float(p[7]),
                float(p[9]),
                float(p[11]),
            )
        )
    except ValueError:
        continue

if not rows:
    sys.exit("ge_out.txt has no data -- did ngspice run?")

print(f"rows: {len(rows)}   t: {rows[0][0]:.3g} .. {rows[-1][0]:.3g} s")

# The bit, read where the transition ramp is over on either side of the edge.
before = [r for r in rows if SETTLE <= r[0] <= EDGE - SETTLE]
after = [r for r in rows if r[0] >= EDGE + SETTLE]
if not before or not after:
    sys.exit("ge_out.txt does not cover both sides of the 8 us edge")

lo = max(abs(r[2]) for r in before)
hi = max(abs(r[2] - 1.0) for r in after)

# Where the bit actually moved: the first row past the half-way point.
crossed = next((r[0] for r in rows if r[2] > 0.5), None)

ptran = [r[4] for r in rows]
pac = [r[5] for r in rows]
static = [r[6] for r in rows]

print()
print(f"v(in) at 2 us / 8 us  : "
      f"{min(rows, key=lambda r: abs(r[0] - 2e-6))[1]:.3f} V / "
      f"{min(rows, key=lambda r: abs(r[0] - EDGE))[1]:.3f} V  (threshold {THRESH} V)")
print(f"v(out) before 8 us    : {lo:.2e} V from 0  (tol {TOL:.0e})")
print(f"v(out) after 8 us     : {hi:.2e} V from 1  (tol {TOL:.0e})")
print(f"bit moved at          : {crossed * 1e6 if crossed else float('nan'):.3f} us"
      f"  (clock edge 8.000 us, input crossing 5.000 us)")
print()
print(f'v(ptran) initial_step("tran"): {min(ptran):.3f} .. {max(ptran):.3f}  (want 1)')
print(f'v(pac)   ...("ac")           : {min(pac):.3f} .. {max(pac):.3f}  (want 0)')
print(f'v(pstatic) ...("static")     : {min(static):.3f} .. {max(static):.3f}'
      "  (reported, not asserted)")
print()

if crossed is not None and abs(crossed - WRONG_EDGE) < 1e-6:
    sys.exit(
        f"FAIL: the bit moved at {crossed * 1e6:.3f} us, where V(in) crosses the "
        "threshold -- the body is unconditional, so the sampler is tracking its input"
    )
if lo > TOL:
    sys.exit(f"FAIL: v(out) is {lo:.4f} V from 0 before the clock edge")
if hi > TOL:
    sys.exit(f"FAIL: v(out) is {hi:.4f} V from 1 after the clock edge")
if crossed is None or abs(crossed - EDGE) > 100e-9:
    sys.exit(f"FAIL: the bit moved at {crossed} s, not at the 8 us clock edge")
if abs(max(ptran) - 1.0) > TOL or abs(min(ptran) - 1.0) > TOL:
    sys.exit(
        f"FAIL: v(ptran) is {min(ptran):.3f} .. {max(ptran):.3f}, want 1 -- "
        'the first point of a transient analysis is what @(initial_step("tran")) names'
    )
if max(abs(v) for v in pac) > TOL:
    sys.exit(
        f"FAIL: v(pac) reaches {max(abs(v) for v in pac):.3f}, want 0 -- an "
        '"ac" analysis list fired in a transient run, so the list is being ignored'
    )

print("PASS: the OR'd initial step guards its body, and both analysis lists are honoured")
