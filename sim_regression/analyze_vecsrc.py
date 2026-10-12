#!/usr/bin/env python3
"""Check sim_regression/vecsrc, where the file is the reference.

`vectors.txt` is read here the way the model read it, and the waveform has to be
what it says: each value applied at its own time, and nothing before the first.

Exits non-zero and prints what failed.
"""
import sys

TR = 1e-9
NVEC = 2  # v(out) v(nrec)


def main():
    records = []
    for line in open("vectors.txt"):
        f = line.split()
        if len(f) == 3:
            records.append((float(f[0]), float(f[1]), f[2]))
    if not records:
        print("vectors.txt has no records")
        return 1

    vals = []
    for tok in open("vs_out.txt").read().split():
        try:
            vals.append(float(tok))
        except ValueError:
            pass
    n = 2 * NVEC
    rows = [vals[i : i + n] for i in range(0, len(vals) - n + 1, n)]
    if len(rows) < 10:
        print(f"only {len(rows)} rows in vs_out.txt -- did the run abort?")
        return 1

    fail = []

    # How many records the model says it loaded, which is the first thing that has
    # to agree: a `$fgets` that read nothing, or a `$sscanf` that converted less
    # than the three fields, would show up here.
    loaded = rows[-1][3]
    print(f"records in the file: {len(records)}, loaded by the model: {loaded:.0f}")
    if abs(loaded - len(records)) > 1e-9:
        fail.append(f"the model loaded {loaded:.0f} records, not {len(records)}")

    def sample(t):
        """v(out) at the last timepoint at or before t."""
        best = None
        for row in rows:
            if row[0] <= t + 1e-18:
                best = row[1]
            else:
                break
        return best

    # Before the first record nothing has been applied.
    first_t = records[0][0]
    before = sample(first_t - 10 * TR)
    print(f"before the first record: {before:+.6f} V")
    if abs(before) > 1e-9:
        fail.append(f"the output was {before:+.6f} V before the first record, not 0")

    # And each record's value is in force once its transition has arrived. Reading
    # at the end of the ramp rather than at the instant is the whole of 4.5.8: the
    # value is reached there, exactly.
    worst = 0.0
    worst_at = 0.0
    for t, v, tag in records:
        got = sample(t + 2 * TR)
        err = abs(got - v)
        if err > worst:
            worst, worst_at = err, t
        print(f"  {t * 1e6:6.3f} us  {tag:<5s} want {v:+.6f} V  got {got:+.6f} V")
    print(f"worst value error: {worst:.3e} V")
    if worst > 1e-9:
        fail.append(f"the output is off by {worst:.3e} V at {worst_at * 1e6:.3f} us")

    for line in fail:
        print("FAIL:", line)
    return 1 if fail else 0


if __name__ == "__main__":
    sys.exit(main())
