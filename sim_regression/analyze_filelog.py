#!/usr/bin/env python3
"""Check sim_regression/filelog, where the measurement is the file the model
wrote rather than anything read back from the waveform.

Nothing here opens an ngspice output file. `periods.txt`, `evals.txt`,
`both_a.txt` and `both_b.txt` were all written by the model with VAMS-2023 9.5's
tasks, and every claim is checked against the file's own contents.

Exits non-zero and prints what failed.
"""
import statistics as st
import sys

# the netlist
PERIOD = 1e-6
JITTER = 20e-9
TSTOP = 100e-6


def read(path):
    rows = []
    for line in open(path):
        f = line.split()
        if not f:
            continue
        try:
            rows.append([float(x) for x in f])
        except ValueError:
            pass
    return rows


def main():
    fail = []

    periods = read("periods.txt")
    evals = read("evals.txt")
    if not periods or not evals:
        print("the model wrote nothing: periods.txt and evals.txt are empty")
        return 1

    # -- one line per timepoint against one line per evaluation ----------------
    # 9.4.1 says `$strobe` writes "at the end of the current simulation time" and
    # `$display` whenever it is reached, which in an analog block is once per
    # Newton iteration. So the second file has to be the longer one.
    print(f"$fstrobe wrote {len(periods)} lines, $fdisplay {len(evals)}")
    if len(evals) <= len(periods):
        fail.append(
            f"$fdisplay wrote {len(evals)} lines and $fstrobe {len(periods)}: "
            "the per-iteration task should write more"
        )

    # A timepoint the solver goes back on keeps the line already written for it, so
    # the time column is not monotone. Reported rather than required: what a reader
    # wants is the monotone subsequence, and this says how much is being dropped.
    times = [r[1] for r in periods]
    retries = sum(1 for a, b in zip(times, times[1:]) if b <= a)
    print(f"timepoints attempted and then gone back on: {retries}")

    # -- the periods, checked against each other -------------------------------
    # One (edge, period) pair per period index, however many timepoints carried it.
    logged = {}
    for n, _t, edge, p in periods:
        logged[int(n)] = (edge, p)
    counts = sorted(logged)
    print(f"periods logged: {len(counts)} (n = {counts[0]} .. {counts[-1]})")

    want = round(TSTOP / PERIOD)
    if abs(len(counts) - want) > 2:
        fail.append(f"{len(counts)} periods logged over {TSTOP * 1e6:.0f} us, want ~{want}")

    # Each period is the gap between consecutive edges, which makes the file
    # internally checkable: the numbers the model wrote have to agree with each
    # other to the last bit, or something between computing and writing them lost
    # precision.
    worst = 0.0
    for n in counts[1:]:
        if n - 1 not in logged:
            continue
        gap = logged[n][0] - logged[n - 1][0]
        worst = max(worst, abs(gap - logged[n][1]))
    print(f"worst period against the gap between its edges: {worst:.3e} s")
    if worst > 1e-15:
        fail.append(f"the logged periods disagree with the logged edge times by {worst:.3e} s")

    # -- the distribution the model drew from ----------------------------------
    # Each half-period is an independent draw, so the period's standard deviation
    # is sqrt(2) times the one asked for. The first period is the gap from t = 0 to
    # the first edge, which is not a period at all.
    vals = [logged[n][1] for n in counts[1:]]
    mean = st.fmean(vals)
    sd = st.stdev(vals)
    want_sd = JITTER * 2 ** 0.5
    print(
        f"period: mean {mean * 1e6:.6f} us, sd {sd * 1e9:.3f} ns "
        f"against {PERIOD * 1e6:.6f} us and {want_sd * 1e9:.3f} ns, over {len(vals)} periods"
    )
    if abs(mean - PERIOD) / PERIOD > 0.02:
        fail.append(f"mean period is {mean * 1e6:.6f} us, not {PERIOD * 1e6:.6f} us")
    if abs(sd - want_sd) / want_sd > 0.25:
        fail.append(f"period sd is {sd * 1e9:.3f} ns, not {want_sd * 1e9:.3f} ns")

    # -- 9.5.1's multichannel descriptor ---------------------------------------
    # One task, two files: `$fstrobe(mcd_a | mcd_b, ...)` wrote to both, and the
    # pair has to be identical and to agree with the descriptor channel.
    a = open("both_a.txt").read()
    b = open("both_b.txt").read()
    print(f"both channels of the mcd: {len(a)} and {len(b)} bytes, identical: {a == b}")
    if a != b or not a:
        fail.append("the two channels of the multichannel descriptor differ")
    mcd = {}
    for row in read("both_a.txt"):
        mcd[int(row[0])] = row[1]
    if mcd != {n: logged[n][1] for n in logged}:
        fail.append("the mcd channel and the file descriptor logged different periods")

    for line in fail:
        print("FAIL:", line)
    return 1 if fail else 0


if __name__ == "__main__":
    sys.exit(main())
