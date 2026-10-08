# Transient regression harness

Local-only. These tests run a compiled model in **ngspice** and measure the
waveform, which is the one thing the in-tree harnesses cannot do:

| harness | lowers equations | steps time | judges |
| --- | --- | --- | --- |
| `test_data/mir` | no | no | MIR shape |
| `test_data/ui` | n/a | no | diagnostics |
| `openvaf/tests/integration.rs` (mock sim) | yes | state swap only, `abstime = 0` | residuals and Jacobians |
| this | yes | yes, a real integrator | behaviour over time |

`slew`'s rate limit, a filter's corner frequency and — above all — whether an
event actually fires are claims about waveforms, so they belong here.

## Running

All of them at once:

```sh
PATH=$HOME/ngspice-install/bin:$PATH ./sim_regression/run_all.sh
```

```
sample_hold    PASS
above_init     PASS
last_crossing  PASS
timer          PASS
```

Or one at a time:

```sh
export PATH=$HOME/ngspice-install/bin:$PATH

cd sim_regression/sample_hold
../../target/release/openvaf-r sample_hold.va -o sample_hold.osdi
ngspice -b sample_hold.cir
python3 ../analyze_sample_hold.py

cd ../above_init
../../target/release/openvaf-r above_init.va -o above_init.osdi
ngspice -b above_init.cir
python3 ../analyze_above_init.py

cd ../last_crossing
../../target/release/openvaf-r last_crossing.va -o last_crossing.osdi
ngspice -b last_crossing.cir
python3 ../analyze_last_crossing.py

cd ../timer
../../target/release/openvaf-r timer.va -o timer.osdi
ngspice -b timer.cir
python3 ../analyze_timer.py
```

Each analyzer exits non-zero on failure.

## sample_hold — passing

`sample_hold.va` is VAMS-2023 §5.10.3.1's own sample-and-hold, written verbatim.
Per the LRM `state` updates only when `V(smpl)` crosses the threshold upward, so
`out` is a staircase holding each sampled value until the next crossing.

The analyzer checks two independent claims and exits non-zero if either fails:

| check | what it catches |
| --- | --- |
| hold | `out` must not move between sample edges |
| accuracy | the held value must be `v(in)` at the interpolated crossing instant |

Current result:

```
worst drift within a hold interval : 0.00e+00 V  (tol 5e-03)
worst held-vs-sampled error        : 0.00e+00 V  (tol 1e-06)
PASS: out holds each sampled value between edges -- LRM behaviour
```

Exact, and the tolerance is a thousand times tighter than it was. `cross` now
steers the timestep onto the crossing (5.10.3.1: "in addition, cross() controls
the timestep to accurately resolve the crossing") rather than letting the event
land on whatever step the solver took next. The previous compiler fails this same
check at 7.50e-05 V.

One measurement note that cost some confusion: `transition(state, 0, 10n)` is a
continuous lag, so it approaches the sampled value asymptotically rather than
arriving in exactly 10 ns. At 100 ns after an edge it is still ~2e-05 V short,
which was invisible while the event was landing ~150 ns late and swamping it.
`SETTLE` is 500 ns so that what this test measures is the event timing and not
the filter.

For the record, the two earlier states of this test, both of which it now
distinguishes by name:

```
local-all, before any crossing detection:
  worst drift 0.9430 V   -> out TRACKS the input, the body runs every step

with crossing detection but retained state in the OSDI state array:
  v(out) == 0 throughout -> the crossing is never detected at all
```

## above_init — passing

`above_init.va` is VAMS-2023 §5.10.3.2's own sample-and-hold: 5.10.3.1's module
with `above` in place of `cross`. The netlist drives the scenario the clause was
written for — the clock sits at 5 V for the entire run and never crosses the
2.5 V threshold in either direction, so `cross` has nothing to trigger on and
would leave `out` at 0 forever. The LRM's words: `cross` "would never trigger,
even if the voltage on the smpl port is always above 2.5V".

```
v(smpl)                 : 5.000 .. 5.000 V  (never crosses 2.5 V)
v(in) over the run      : 1.056 .. 6.000 V
v(out) after settling   : 1.0000 .. 1.0000 V
PASS: above() sampled at initialization and held -- LRM behaviour
```

This one earns its place twice over. `above`'s initialization event was first
written gated on the `static` analysis flag, which the mock simulator was happy
with because the test handed it that flag — but ngspice sets ANALYSIS_STATIC only
on the *first Newton iteration* of the initial step and ANALYSIS_TRAN on the
rest, so the event fired on one iteration and was overwritten by the others, and
`v(out)` stayed at 0 for the whole run. The flags are not stable within a
timestep; `$abstime` is. Nothing but a real integrator would have caught that.

## last_crossing — passing

A 200 kHz sine, so the rising zero crossings sit at exactly 0, 5, 10, 15 and
20 us. The one at t = 0 cannot be detected — interpolation needs a point on each
side — so the reading should stay negative until 5 us and then step through the
rest.

```
reading before the first crossing : max -1  (must be negative)
  crossing 1 at  5.00us : read  5.000000000us  err 0.00e+00 s
  crossing 4 at 20.00us : read 20.000000000us  err 0.00e+00 s
PASS
```

Exact. §4.5.10 is explicit that `last_crossing` itself "does not control the
timestep to get accurate results" -- on its own it managed 0.2 ps here, from
linear interpolation alone. What closed the last of it is the `@(cross)` in the
same model, which now steers the timestep onto the crossing, so the two points
the interpolation runs between straddle it tightly. The clause says as much:
last_crossing "can be used with the cross() or above() function for improved
accuracy".

The model also measures `period` the way 4.5.10's own example does, and that is
asserted too: 5.000000 us, exactly. It reads `latest` inside the `@(cross)`
handler *before* the statement that assigns it, so it only comes out right
because analog variables now keep their value between evaluations.

## timer — passing

`timer(1u, 2u)` sampling a ramp, with **no clock node anywhere in the netlist**.
Every other test here has an edge for the solver to find; this one has nothing to
find, so the only way a timepoint lands on an event is `$bound_step` asking for
it.

```
event instants placed as exact timepoints:
    1.0us : yes      3.0us : yes      5.0us : yes      7.0us : yes      9.0us : yes
value held after each event:
  after   1.0us : v(out)=0.500000  want 0.500000  ticks=1
  after   9.0us : v(out)=4.500000  want 4.500000  ticks=5
worst held-value error : 0.00e+00 V
total events fired     : 5  (want 5)
```

The tick count is the part that would catch a subtler bug than "no event": one
event per period, not one per Newton iteration of the step it lands on.

## Why retained state does not live in the OSDI state array

Worth writing down, because it is the reason the middle state above looked like a
compiler bug and was not one.

`@(cross)` needs the value its expression had at the previous *accepted* timestep.
The obvious home is OSDI's `prev_state` / `next_state`, and that is where it
started. It cannot work there:

* `OSDIload` passes `ckt->CKTstates[0]` as **both** `prev_state` and `next_state`
  (ngspice-46 `src/osdi/osdiload.c:141`), so the two alias.
* `dctran` rotates that ring once per accepted step
  (`src/spicelib/analysis/dctran.c:659`) over `MAX(2,maxord)+2` buffers
  (`cktsetup.c:192`), so with the default `maxord = 2` a model reads back the
  slot it wrote **four steps** ago.

Neither is a conformance violation: the OSDI header documents no semantics for
those pointers, and the array's real purpose is `$limit`, where aliasing gives
exactly the previous-Newton-iteration value that limiting wants. It is also why
nobody noticed — a limit state only steers the Newton path and never the
converged answer. A retained value *is* the answer.

So retained slots live in the instance data instead, which no simulator rotates,
committed on `$abstime` movement. See the retained-state note at the top of
`openvaf/osdi/stdlib.c` for the mechanism and its rejected-timestep handling.

Two things are still worth fixing upstream, neither of them here:

1. **ngspice**: even read as "previous iteration", the ring rotation is wrong.
   The first Newton iteration of every step reads a four-step-old buffer rather
   than the last accepted value, because nothing copies `CKTstates[1]` into
   `[0]` for OSDI devices (classic SPICE devices rewrite all their state each
   load, so they never needed it). That is a convergence-quality bug in `$limit`
   that stands on its own.
2. **OSDI**: there is no facility with accepted-timestep semantics, and one
   pointer cannot serve both meanings. The precedent for how it should look is
   `absdelay`, the other genuinely time-dependent operator: it is a
   descriptor-level protocol (`OsdiAbsDelayInfo`) where the simulator owns the
   history, not something a model fakes through the state array.
