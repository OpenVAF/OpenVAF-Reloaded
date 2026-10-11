"""Measure the absdelay runs against VAMS-2023 4.5.7.

The clause gives the operator in closed form,

    Output(t) = Input(max(t - td, 0))

and the input here is a 1 V/us ramp, so every expected value is arithmetic. Two
realizations are checked, against that formula and against each other:

  ad_out.txt        `--absdelay in-model`: the history is retained state inside
                    the model, so this runs on any OSDI simulator.
  ad_proto_out.txt  the descriptor protocol (`OsdiAbsDelayInfo`), where the
                    simulator owns the history. Only present when the simulator
                    implements it; skipped with a note otherwise.

Four claims:

  fixed    a constant 1 us delay reproduces the ramp shifted by exactly 1 us,
           and holds input(0) until t reaches the delay.
  figure   Figure 4-4's time-varying delay, which is what `maxdelay` is for:
           td of 2 us until t = 3 us, then 4 us until t = 5 us, then 1 us. The
           clause walks through the result -- "From time 0 until 2s, the output
           remains at input(0) [...] At 3s, the transport delay changes from 2s
           to 4s, switching the output back to input(0) [...] At 5s the transport
           delay goes to 1s and the output correspondingly jumps".
  causal   the output never runs ahead of the input.
  agree    where both realizations ran, they agree.

Exits non-zero on failure.
"""

import os
import sys

US = 1e-6
TD = 1.0 * US
# Figure 4-4's schedule, as the model writes it.
SCHEDULE = [(3.0 * US, 2.0 * US), (5.0 * US, 4.0 * US), (float("inf"), 1.0 * US)]
# A ramp of 1 V/us from 0.
RAMP = 1.0 / US

# The output jumps where the delay does, so expectations are probed between the
# jumps rather than across them.
JUMPS = [3.0 * US, 5.0 * US]
GUARD = 60e-9

IN_MODEL = "ad_out.txt"
PROTOCOL = "ad_proto_out.txt"


def read(path):
    """wrdata writes (time, value) pairs per column; take the first time column."""
    rows = []
    with open(path) as fh:
        for line in fh:
            parts = line.split()
            if len(parts) < 6:
                continue
            try:
                vals = [float(p) for p in parts]
            except ValueError:
                continue
            rows.append((vals[0], vals[1], vals[3], vals[5]))
    return rows


def delay_at(t):
    for until, delay in SCHEDULE:
        if t < until:
            return delay
    raise AssertionError


def want_fixed(t):
    return max(t - TD, 0.0) * RAMP


def want_vary(t):
    return max(t - delay_at(t), 0.0) * RAMP


def near_jump(t):
    return any(abs(t - jump) < GUARD for jump in JUMPS)


def check(name, rows, problems):
    if not rows:
        problems.append("%s: no data rows" % name)
        return

    # fixed: the whole waveform, which for a ramp is exact arithmetic.
    worst, worst_t = 0.0, 0.0
    for t, fixed, _vary, _raw in rows:
        err = abs(fixed - want_fixed(t))
        if err > worst:
            worst, worst_t = err, t
    print("  %-8s fixed delay: worst error %.3e V at %.3f us" % (name, worst, worst_t / US))
    if worst > 2e-3:
        problems.append("%s: fixed delay is off by %.3e V at %.3f us" % (name, worst, worst_t / US))

    # figure: the varying delay, away from its own jumps.
    worst, worst_t = 0.0, 0.0
    for t, _fixed, vary, _raw in rows:
        if near_jump(t):
            continue
        err = abs(vary - want_vary(t))
        if err > worst:
            worst, worst_t = err, t
    print("  %-8s figure 4-4:  worst error %.3e V at %.3f us" % (name, worst, worst_t / US))
    if worst > 0.2:
        problems.append("%s: figure 4-4 is off by %.3e V at %.3f us" % (name, worst, worst_t / US))

    # the four phases the clause narrates, sampled in the middle of each.
    for t, expect in [
        (1.0 * US, 0.0),
        (2.5 * US, 0.5),
        (3.5 * US, 0.0),
        (4.5 * US, 0.5),
        (5.5 * US, 4.5),
    ]:
        got = min(rows, key=lambda row: abs(row[0] - t))
        if abs(got[2] - expect) > 0.2:
            problems.append(
                "%s: at %.1f us the varying delay reads %.3f V, expected %.3f V"
                % (name, t / US, got[2], expect)
            )

    # causal: a delay cannot show the future.
    for t, fixed, vary, raw in rows:
        if fixed > raw + 1e-6 or vary > raw + 1e-6:
            problems.append("%s: output leads the input at %.3f us" % (name, t / US))
            break


def main():
    problems = []
    runs = {}

    if not os.path.exists(IN_MODEL):
        problems.append("%s is missing: the in-model run did not produce data" % IN_MODEL)
    else:
        runs["in-model"] = read(IN_MODEL)
        check("in-model", runs["in-model"], problems)

    if os.path.exists(PROTOCOL) and os.path.getsize(PROTOCOL) > 0:
        runs["protocol"] = read(PROTOCOL)
        check("protocol", runs["protocol"], problems)
    else:
        print("  protocol run skipped: this simulator does not implement")
        print("  OsdiAbsDelayInfo (see patches/ngspice-absdelay-history.patch)")

    if len(runs) == 2:
        worst, worst_t = 0.0, 0.0
        proto = runs["protocol"]
        for t, fixed, _vary, _raw in runs["in-model"]:
            if near_jump(t):
                continue
            other = min(proto, key=lambda row: abs(row[0] - t))
            if abs(other[0] - t) > 40e-9:
                continue
            err = abs(fixed - other[1])
            if err > worst:
                worst, worst_t = err, t
        print("  the two realizations differ by at most %.3e V (at %.3f us)" % (worst, worst_t / US))
        if worst > 5e-3:
            problems.append("the two realizations disagree by %.3e V" % worst)

    for problem in problems:
        print("FAIL: %s" % problem)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
