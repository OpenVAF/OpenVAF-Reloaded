"""Measure what announcing a discontinuity is worth (VAMS-2023 9.17.1).

`jump.va` switches its output by 5 V at a threshold crossing and drives an RC of
1 us. `jump.cir` announces the jump with `$discontinuity(0)`, `jump_off.cir` does
not, and that parameter is the only difference between the two runs. The netlists
ask for Gear integration, because Gear carries several past points across the jump
and is therefore the method the announcement is about; see the note at the end.

The reference is exact: the drive is piecewise constant, so each interval of the
solver's own grid has a closed-form answer, and starting each interval from the
solver's own value makes the error reported per interval rather than accumulated.

Two measurements, and which one applies depends on the simulator:

  a simulator that does not read the protocol
      the two runs must be identical, bit for bit. That is the claim that
      announcing costs nothing where it buys nothing, and it is the only claim a
      stock simulator can support.

  a simulator that does (set NGSPICE_TIMEPOINTS to one, which writes the second
  pair of files)
      the announced run must be at least five times more accurate in the twenty
      timepoints after each jump, which is where carrying a stale history shows.

The error *at* the jump is the same either way and is reported, not asserted: no
multistep method integrates a step that straddles a discontinuity accurately,
which is why 9.17.1 tells a model to smooth its edges with `transition()` and why
this case is written without one.

Exits non-zero on failure.
"""

import math
import os
import sys

TAU = 1e-6          # the RC the netlists build
VJUMP = 1.0         # a jump of the switch output is at least this big
AFTER = 20          # timepoints after a jump that the announcement can change
GAIN = 5.0          # the accuracy the announcement has to buy, where it is read


def read(name):
    rows = []
    for line in open(name):
        p = line.split()
        if len(p) < 6:
            continue
        try:
            # wrdata writes a time column per variable: t out, t mid, t ctrl
            rows.append((float(p[0]), float(p[1]), float(p[3])))
        except ValueError:
            continue
    return rows


def errors(rows):
    """(time, error, timepoints since the last jump) for every interval."""
    out = []
    since = 1 << 30
    for (t0, drive, v0), (t1, next_drive, v1) in zip(rows, rows[1:]):
        # The drive holds until the instant it jumps, so the interval that ends at
        # a jump is driven by the old value.
        exact = drive + (v0 - drive) * math.exp(-(t1 - t0) / TAU)
        since = 0 if abs(next_drive - drive) > VJUMP else since + 1
        out.append((t1, abs(v1 - exact), since))
    return out


def summarize(label, on_file, off_file):
    """Both runs of a pair, reported and returned as (announced, silent) errors."""
    on, off = read(on_file), read(off_file)
    if len(on) < 100 or len(off) < 100:
        print(f"{label}: only {len(on)} and {len(off)} rows")
        return None
    res = []
    for name, rows in (("announced", on), ("silent", off)):
        errs = errors(rows)
        jumps = sum(1 for _, _, since in errs if since == 0)
        at_jump = max((e for _, e, since in errs if since == 0), default=0.0)
        after = max((e for _, e, since in errs if 1 <= since <= AFTER), default=0.0)
        print(
            f"{label:<22} {name:<10} rows {len(rows):5d}  jumps {jumps:2d}  "
            f"at the jump {at_jump:.3e} V  worst of the {AFTER} after {after:.3e} V"
        )
        res.append((after, jumps, rows))
    return res


fails = []

plain = summarize("cap only", "tp_on.txt", "tp_off.txt")
if plain is None:
    sys.exit(1)
(_, jumps_on, rows_on), (_, jumps_off, rows_off) = plain
if jumps_on != 8 or jumps_off != 8:
    fails.append(f"expected 8 jumps in each run, got {jumps_on} and {jumps_off}")
if rows_on != rows_off:
    fails.append(
        "the two runs differ although this simulator does not read the protocol: "
        "announcing is supposed to cost nothing"
    )

if os.path.exists("tp_proto_on.txt"):
    proto = summarize("protocol read", "tp_proto_on.txt", "tp_proto_off.txt")
    if proto is None:
        sys.exit(1)
    (after_on, _, _), (after_off, _, _) = proto
    print(
        f"{'':<22} {'gain':<10} {after_off / max(after_on, 1e-300):.1f}x "
        f"better after a jump  (want {GAIN:.0f}x)"
    )
    if after_on * GAIN > after_off:
        fails.append(
            f"the announcement bought {after_off / max(after_on, 1e-300):.2f}x, "
            f"not {GAIN:.0f}x"
        )
else:
    print("protocol run skipped: set NGSPICE_TIMEPOINTS to a simulator that reads")
    print("the timepoint protocol (patches/ngspice-osdi-timepoints.patch)")

if fails:
    print()
    for fail in fails:
        print(f"FAIL: {fail}")
    sys.exit(1)
