#!/usr/bin/env python3
"""Check the behavioural PLL in sim_regression/pll.

Everything checked here is a closed-form consequence of the netlist rather than a
reference waveform: the reference clock's own grid, the divide ratio, the control
voltage the oscillator needs to run at ndiv times the reference, a type-II loop's
zero static phase error, and the standard deviation of a jittered clock.

Exits non-zero and prints what failed.
"""
import statistics as st
import sys

# the netlist
FREF = 1e6
NDIV = 4
F0 = 3e6
KVCO = 2e6
JITTER = 20e-9
# the window in which the loop is settled; wn ~ 2*pi*20 kHz, zeta ~ 0.87
T_LOCK = 70e-6
VTH = 0.5

NVEC = 7  # ref fb vco ctrl rdrift jclk jdrift


def read(path):
    """wrdata wraps long lines, so read the whole file as one token stream."""
    vals = []
    for tok in open(path).read().split():
        try:
            vals.append(float(tok))
        except ValueError:
            pass
    n = 2 * NVEC
    return [vals[i : i + n] for i in range(0, len(vals) - n + 1, n)]


def crossings(rows, vec, thr=VTH):
    """Rising crossings of `thr`, interpolated.

    Consecutive rows can carry the same timestamp -- ngspice writes a row per
    accepted point and several can land on one instant -- so a pair that straddles
    the threshold with no time between its ends is taken at its own instant rather
    than skipped, which would silently lose the edge.
    """
    idx = 2 * vec + 1
    out = []
    for a, b in zip(rows, rows[1:]):
        t0, v0, t1, v1 = a[idx - 1], a[idx], b[idx - 1], b[idx]
        if v0 < thr <= v1:
            if t1 > t0 and v1 > v0:
                out.append(t0 + (thr - v0) * (t1 - t0) / (v1 - v0))
            else:
                out.append(t1)
    return out


def freq(edges):
    return (len(edges) - 1) / (edges[-1] - edges[0])


def main():
    rows = read("pll_out.txt")
    if len(rows) < 1000:
        print(f"only {len(rows)} rows in pll_out.txt -- did the run abort?")
        return 1

    ref, fb, vco = (crossings(rows, v) for v in (0, 1, 2))
    t_end = rows[-1][0]
    if t_end < 119e-6:
        print(f"the run stopped at {t_end * 1e6:.3f} us, short of 120 us")
        return 1

    fail = []

    # -- the reference clock's own grid ---------------------------------------
    # Each edge is scheduled from the one before it, so an edge taken late or
    # early would show up as a drift the model measures itself.
    drift = rows[-1][2 * 4 + 1]
    print(f"reference edge drift from its own grid: {drift:.3e} s")
    if drift > 1e-15:
        fail.append(f"reference edges drifted {drift:.3e} s from the grid")

    f_ref = freq(ref)
    iv = [b - a for a, b in zip(ref, ref[1:])]
    print(
        f"reference: {len(ref)} edges, {f_ref / 1e6:.9f} MHz, "
        f"period sd {st.stdev(iv) * 1e12:.3f} ps"
    )
    if abs(f_ref - FREF) / FREF > 1e-9:
        fail.append(f"reference frequency is {f_ref:.6e}, not {FREF:.6e}")

    # -- the loop is locked ---------------------------------------------------
    r = [t for t in ref if t >= T_LOCK]
    f = [t for t in fb if t >= T_LOCK]
    w = [t for t in vco if t >= T_LOCK]
    if min(len(r), len(f), len(w)) < 10:
        print(f"too few edges after {T_LOCK * 1e6:.0f} us: {len(r)}/{len(f)}/{len(w)}")
        return 1

    ratio = freq(vco[len(vco) - len(w) :]) / f_ref
    print(f"locked {T_LOCK * 1e6:.0f}-{t_end * 1e6:.0f} us: f_vco / f_ref = {ratio:.6f}")
    if abs(ratio - NDIV) / NDIV > 2e-4:
        fail.append(f"the loop is not locked: f_vco / f_ref = {ratio:.6f}, not {NDIV}")

    # The divided oscillator has to produce exactly one edge per reference edge.
    if abs(len(f) - len(r)) > 1:
        fail.append(f"{len(f)} feedback edges against {len(r)} reference edges")

    # -- static phase error ---------------------------------------------------
    # A type-II loop integrates the error to zero, so the feedback edge arrives on
    # the reference edge rather than at some fixed offset from it.
    err = []
    for t in f:
        before = [x for x in r if x <= t + 1e-9]
        if before:
            err.append(t - before[-1])
    phase = st.fmean(err)
    print(f"static phase error: {phase * 1e9:.4f} ns, sd {st.stdev(err) * 1e9:.4f} ns")
    if abs(phase) > 5e-9:
        fail.append(f"static phase error is {phase * 1e9:.3f} ns, more than 5 ns")

    # -- the control voltage the oscillator needs ------------------------------
    want = (NDIV * FREF - F0) / KVCO
    ctrl = [row[2 * 3 + 1] for row in rows if row[2 * 3] >= T_LOCK]
    got = st.fmean(ctrl)
    ripple = max(ctrl) - min(ctrl)
    print(f"control voltage: {got:.6f} V against {want:.6f} V, ripple {ripple * 1e3:.3f} mV")
    if abs(got - want) > 2e-3:
        fail.append(f"control voltage settled at {got:.6f} V, not {want:.6f} V")
    # A tri-state detector stops pumping once the edges align, so the ripple is
    # the phase error and not the supply.
    if ripple > 5e-3:
        fail.append(f"control ripple is {ripple * 1e3:.3f} mV, more than 5 mV")

    # -- jitter ----------------------------------------------------------------
    # The second clock draws each half-period from a normal distribution, so the
    # period is a sum of two independent draws and its sd is sqrt(2) times theirs.
    jit = crossings(rows, 5)
    iv = [b - a for a, b in zip(jit, jit[1:])]
    sd = st.stdev(iv)
    want_sd = JITTER * 2 ** 0.5
    print(
        f"jittered clock: {len(iv)} periods, sd {sd * 1e9:.3f} ns "
        f"against {want_sd * 1e9:.3f} ns expected"
    )
    if abs(sd - want_sd) / want_sd > 0.25:
        fail.append(f"jitter sd is {sd * 1e9:.3f} ns, not {want_sd * 1e9:.3f} ns")

    for line in fail:
        print("FAIL:", line)
    return 1 if fail else 0


if __name__ == "__main__":
    sys.exit(main())
