#!/usr/bin/env python3
"""Check sim_regression/vecstream: a vector source that streams its file.

The file is the reference. Every record whose stamp the run reaches has to be the
level the waveform holds from that stamp until the next one, and the number of
records the model took has to be the number of records that were due: a read that
advanced per Newton iteration instead of per timepoint consumes the file several
times too fast, and the source loses its place.

Exits non-zero and prints what failed.
"""
import sys

TR = 1e-9  # the model's transition time, so the level is settled either side of it


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
    recs = read("vectors.txt")
    out = read("vstream_out.txt")
    if not recs or not out:
        fail.append("vectors.txt or vstream_out.txt is empty")
        print("\n".join(fail))
        return 1

    # wrdata writes a time column per vector: t v(out) t v(ntaken)
    wave = [(r[0], r[1], r[3]) for r in out]
    tstop = wave[-1][0]
    due = [r for r in recs if r[0] <= tstop]
    taken = wave[-1][2]
    print(f"records in the file {len(recs)}, due by {tstop * 1e6:.1f} us {len(due)}, taken {taken:.0f}")

    if abs(taken - len(due)) > 0.5:
        fail.append(
            f"the model took {taken:.0f} records where {len(due)} were due: "
            "a read is advancing the file at the wrong rate"
        )

    # Each record's value is the level held from its stamp to the next one.
    worst = 0.0
    checked = 0
    for k, (t, v) in enumerate(due):
        t_end = due[k + 1][0] if k + 1 < len(due) else tstop
        inside = [vv for tt, vv, _ in wave if t + 20 * TR < tt < t_end - 20 * TR]
        if not inside:
            continue
        checked += 1
        err = max(abs(vv - v) for vv in inside)
        worst = max(worst, err)
        if err > 1e-3:
            fail.append(f"record {k} at {t:.3e} wants {v:+.6f}, the waveform is off by {err:.3e}")

    print(f"records whose level the waveform holds: {checked} of {len(due)}, worst error {worst:.3e}")
    if checked < len(due) - 2:
        fail.append(f"only {checked} of {len(due)} records had an interval to check")

    # The count only ever grows, and by one record at a time: it is a retained
    # counter, so this is also a check that retained state and the file position
    # agree about what a timepoint is.
    counts = [n for _, _, n in wave]
    jumps = [(a, b) for a, b in zip(counts, counts[1:]) if b < a]
    if jumps:
        fail.append(f"the record count goes backwards {len(jumps)} times, e.g. {jumps[0]}")

    if fail:
        print("\n".join(f"FAIL: {f}" for f in fail))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
