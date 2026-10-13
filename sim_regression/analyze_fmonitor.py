#!/usr/bin/env python3
"""Check sim_regression/fmonitor: what VAMS-2023 9.4.1's four display tasks wrote
over one run, and whether the monitor's lines follow from the waveform.

The model is a level detector with hysteresis. Nothing here restates what it
reports: the thresholds come out of the netlist card, the voltage the detector saw
and the state it reported come out of ngspice's `wrdata` file, and the detector is
recomputed from that voltage. The monitor's lines then have to be exactly the
changes of that recomputed state, with the first point in front of them.

Exits non-zero and prints what failed.
"""
import re
import sys

# wrdata writes nine significant digits, so a time in the model's own log and the
# same time in the waveform agree to about that.
TOL_T = 1e-8


def number(tok):
    """A SPICE number with an optional scale factor suffix."""
    scale = {
        "t": 1e12, "g": 1e9, "meg": 1e6, "k": 1e3, "m": 1e-3,
        "u": 1e-6, "n": 1e-9, "p": 1e-12, "f": 1e-15,
    }
    m = re.match(r"^([-+0-9.eE]+)([a-zA-Z]*)$", tok.strip())
    if not m:
        raise ValueError(tok)
    val = float(m.group(1))
    suffix = m.group(2).lower()
    for name in ("meg", "t", "g", "k", "m", "u", "n", "p", "f"):
        if suffix.startswith(name):
            return val * scale[name]
    return val


def thresholds():
    """The detector's window, as the netlist cards it, with the module's own
    defaults for anything the card leaves out."""
    want = {}
    src = open("fmonitor.va").read()
    for name, val in re.findall(r"parameter\s+real\s+(\w+)\s*=\s*([^\s;]+)", src):
        want[name] = number(val)
    for line in open("fmonitor.cir"):
        if line.lower().startswith(".model"):
            for name, val in re.findall(r"(\w+)\s*=\s*([^\s()]+)", line):
                if name in want:
                    want[name] = number(val)
    return want["vlo"], want["vhi"]


def rows(path):
    """The (time, v(in), v(st)) rows of a wrdata file. `wrdata` writes an x column
    per vector, and the times are the same in each."""
    out = []
    for line in open(path):
        f = line.split()
        if len(f) < 4:
            continue
        try:
            vals = [float(x) for x in f]
        except ValueError:
            continue
        out.append((vals[0], vals[1], vals[3]))
    return out


def lines(path, tag):
    """The numbers of every line of the model's log that starts with `tag`."""
    out = []
    for line in open(path):
        f = line.split()
        if f and f[0] == tag:
            out.append([float(x) for x in f[1:]])
    return out


def detector(vin, vlo, vhi):
    """The model's own rule, recomputed: a Schmitt trigger has to be walked in
    order, which is what makes its state something a single timepoint cannot
    give."""
    state = 0
    out = []
    for v in vin:
        if state == 0 and v > vhi:
            state = 1
        elif state == 1 and v < vlo:
            state = 0
        out.append(state)
    return out


def match(t, times):
    """The index of the timepoint a logged time belongs to."""
    i = min(range(len(times)), key=lambda k: abs(times[k] - t))
    err = abs(times[i] - t) / max(abs(t), 1e-12)
    return i, err


def check(log, out, label, accepting, fail):
    """Everything one run has to satisfy, and the states its monitor reported."""
    data = rows(out)
    if not data:
        fail.append(f"{label}: {out} has no rows")
        return []
    times = [r[0] for r in data]
    vin = [r[1] for r in data]
    vlo, vhi = thresholds()

    want = detector(vin, vlo, vhi)
    reported = [round(r[2]) for r in data]
    agree = sum(1 for a, b in zip(want, reported) if a == b)
    print(
        f"{label}: {len(data)} timepoints, the state the model contributed and the "
        f"detector recomputed from v(in) agree on {agree} of {len(data)}"
    )
    if agree != len(data):
        fail.append(f"{label}: the reported state differs from the recomputed one on "
                    f"{len(data) - agree} timepoints")

    monitor = lines(log, "monitor")
    strobe = lines(log, "strobe")
    display = lines(log, "display")
    clock = lines(log, "clock")
    print(
        f"{label}: $fdisplay wrote {len(display)} lines, $fstrobe {len(strobe)}, "
        f"$fmonitor {len(monitor)}, and the monitor of $abstime alone {len(clock)}"
    )

    # 9.4.1: the per-evaluation task writes most, the per-timepoint one writes one
    # line per timepoint, and the monitor writes when something changed.
    if len(display) <= len(strobe):
        fail.append(f"{label}: $fdisplay wrote {len(display)} lines and $fstrobe "
                    f"{len(strobe)}: the per-iteration task should write more")
    if len(strobe) != len(data):
        fail.append(f"{label}: $fstrobe wrote {len(strobe)} lines over {len(data)} "
                    "timepoints, and it writes one per timepoint")
    # The exception, which is the whole difference between a monitor and a strobe
    # for anything that reports the time: one line in the run, not one per point.
    if len(clock) != 1:
        fail.append(f"{label}: the monitor of $abstime alone wrote {len(clock)} lines, "
                    "and 9.4.1 excepts $abstime from the comparison")

    # What the monitor should have written: the first point, which has nothing to
    # differ from, and every point at which the recomputed state changed.
    changes = [0] + [i for i in range(1, len(want)) if want[i] != want[i - 1]]
    print(f"{label}: the recomputed detector changes at {len(changes) - 1} of "
          f"{len(data)} timepoints, and the monitor wrote {len(monitor)} lines")
    if len(monitor) != len(changes):
        fail.append(f"{label}: the monitor wrote {len(monitor)} lines where the "
                    f"waveform asks for {len(changes)}")
        return [round(m[1]) for m in monitor]

    worst = 0.0
    for (t, state), i in zip([(m[0], round(m[1])) for m in monitor], changes):
        j, err = match(t, times)
        worst = max(worst, err)
        if j != i or err > TOL_T:
            fail.append(f"{label}: a monitor line at {t:.9e} s belongs to timepoint "
                        f"{j} and the change it should report is at timepoint {i}")
        elif state != want[i]:
            fail.append(f"{label}: the monitor reports state {state} at {t:.9e} s "
                        f"where the waveform has {want[i]}")
    print(f"{label}: worst disagreement between a monitor's time and the "
          f"timepoint it belongs to: {worst:.2e} relative")

    # Whether the strobe beside it reported the same thing at the same timepoint.
    # On a simulator that confirms its points both lines come from the evaluation
    # that converged, so every monitor line is a strobe line as well. On one that
    # does not, a strobe's line is its point's *first* evaluation (9.4.1 has no
    # other moment to offer), and the first evaluation of the point where a state
    # turns over is the one that has not turned over yet: so the two disagree at
    # exactly the points the monitor has something to say about. That is the
    # difference the accept callback buys, measured.
    strobed = {(round(s[0], 15), round(s[1])) for s in strobe}
    missing = [m for m in monitor if (round(m[0], 15), round(m[1])) not in strobed]
    print(f"{label}: {len(monitor) - len(missing)} of {len(monitor)} monitor lines "
          "are a line the strobe beside them wrote too")
    if accepting and missing:
        fail.append(f"{label}: {len(missing)} monitor lines are at timepoints the "
                    "strobe beside them reported differently, although every line "
                    "of this run comes from the evaluation that converged")

    return [round(m[1]) for m in monitor]


def main():
    fail = []
    states = check("fm_log.txt", "fm_out.txt", "plain", False, fail)

    # 9.4.1 puts the comparison at the accepted step, which is a moment a simulator
    # has to say. One that does is pointed at by NGSPICE_TIMEPOINTS
    # (patches/ngspice-osdi-timepoints.patch); one that does not leaves the model to
    # infer it from `$abstime`. The same changes have to come out either way.
    try:
        proto = check("fm_log_proto.txt", "fm_out_proto.txt", "accepting", True, fail)
    except OSError:
        proto = None
    if proto is None:
        print("accept callback run skipped: set NGSPICE_TIMEPOINTS to a simulator")
        print("that makes one (patches/ngspice-osdi-timepoints.patch)")
    else:
        print(f"the states reported, either way: {states} and {proto}")
        if states != proto:
            fail.append("the two runs report different sequences of states")

    for line in fail:
        print("FAIL:", line)
    return 1 if fail else 0


if __name__ == "__main__":
    sys.exit(main())
