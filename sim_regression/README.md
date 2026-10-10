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
transition     PASS
absdelay       PASS
pll            PASS
```

`pll` takes longer than all the others together, because it is a closed loop that
has to settle rather than one operator being measured.

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

cd ../transition
../../target/release/openvaf-r trfilter.va -o trfilter.osdi
ngspice -b trfilter.cir
python3 ../analyze_transition.py

cd ../absdelay
../../target/release/openvaf-r addelay.va -o addelay.osdi
../../target/release/openvaf-r --absdelay in-model addelay.va -o addelay_in_model.osdi
ngspice -b addelay_in_model.cir
python3 ../analyze_absdelay.py
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

## transition — passing

`transition` used to be a first-order lag with the rise time as its time
constant. It never arrived, and it discarded `td` (issue #42). VAMS-2023 4.5.8
asks for a piecewise linear waveform, delayed by `td`, with "time-points at both
corners of a transition" — and corners are a claim about a waveform, so the test
lives here.

Both channels are driven from `timer` events, so every corner sits at an instant
arithmetic predicts. With `td = 2n`, `tr = 3n`, `tf = 1n`:

```
corners placed as exact timepoints:
   12.00 ns : yes    15.00 ns : yes    32.00 ns : yes    33.00 ns : yes
value at each corner:
   12.00 ns : 0.000000000  want 0.0  ok
   15.00 ns : 1.000000000  want 1.0  ok
samples inside each ramp sit on the straight line:
       12 -> 15 ns :  13 samples, worst |err| 1.233e-08 (tol 3.333e-08)  ok
a pulse shorter than td and tr survives as a partial swing:
  peak              : 0.333333333  want 0.333333333  ok
  returns to 0      : 53.333333 ns  want 53.333333 ns  ok
```

The interesting line is the last block. A 1 ns pulse is shorter than the 2 ns
delay, so an inertial reading of `td` would swallow it; 4.5.8 says `td` "models
transport delay" and allows "an arbitrary number of transitions pending", so both
edges have to come back out. It is also shorter than `tr`, so it only climbs a
third of the way, and the interrupted fall is computed from the old destination as
origin — which makes it take `tf/3`, landing at 53.333 ns rather than the 54 ns a
fall from the current value over `tf` would give.

The linearity tolerance is derived from the printed time resolution rather than
fixed: `wrdata` gives the time column 9 significant digits, and on the 1 ns fall
that uncertainty already shows up in the value at the 1e-7 level.

## absdelay — passing

VAMS-2023 4.5.7 is the one operator with two realizations to compare, so this
case runs the same model twice.

```
absdelay       PASS
  in-model fixed delay: worst error 8.882e-16 V at 4.006 us
  in-model figure 4-4:  worst error 8.882e-16 V at 5.066 us
  protocol fixed delay: worst error 8.882e-16 V at 4.006 us
  protocol figure 4-4:  worst error 8.882e-16 V at 5.066 us
  the two realizations differ by at most 0.000e+00 V (at 0.000 us)
```

The clause gives the operator in closed form — `Output(t) = Input(max(t - td, 0))`
— and the input is a 1 V/us ramp, so every expected value is arithmetic rather
than a reference waveform.

The second channel is Figure 4-4: a delay of 2 us until t = 3 us, then 4 us until
t = 5 us, then 1 us, under a `maxdelay` of 5 us, which is the only way the clause
allows `td` to vary at all. The figure's own narration is the test: the output
holds `input(0)` until t reaches the delay, tracks `input(t - 2)` from 2 us,
*returns* to `input(0)` at 3 us when the delay grows past the elapsed time, picks
the ramp up again at 4 us, and jumps when the delay shortens at 5 us.

**The protocol half needs a patched simulator.** `absdelay` is realized through
the OSDI descriptor by default (`OsdiAbsDelayInfo`): the compiler emits the
delayed output as an unknown and leaves its row empty, because the input's history
is not something a model evaluation has. ngspice has never implemented that
protocol, and an unstamped row is a singular matrix:

```
Warning: singular matrix:  check node nad#implicit_equation_2
Error: Transient op failed, timestep too small
```

`patches/ngspice-absdelay-history.patch` implements it against ngspice-46, for
both the SPARSE and KLU solvers plus AC, where the delay is a phase shift. Point
`NGSPICE_ABSDELAY` at the patched binary and `run_all.sh` checks that half too;
without it the protocol run is skipped with a note and the in-model half still
runs.

```sh
cd sim_regression/absdelay
../../target/release/openvaf-r addelay.va -o addelay.osdi
../../target/release/openvaf-r --absdelay in-model addelay.va -o addelay_in_model.osdi
ngspice -b addelay_in_model.cir          # any ngspice
$NGSPICE_ABSDELAY -b addelay.cir         # needs the patch
python3 ../analyze_absdelay.py
```

Measured alongside, on a netlist that would otherwise step in microseconds: the
protocol takes 59 timepoints over 6 us, the in-model realization 187, because it
caps the step at the spacing of its own history grid. In AC at 100 kHz with
`td = 1 us` the protocol gives exactly −0.628 rad and the in-model realization
gives none at all; it has no phase to offer, which is the other half of why the
protocol is the default.

## pll — passing

The only case here that is a system rather than an operator: a 1 MHz reference, a
tri-state phase detector and charge pump, an RC loop filter in the netlist, a
divide-by-four and an oscillator, closed into a loop that has to lock at 4 MHz.
Nothing in it is a reference waveform; every number is a closed-form consequence
of the netlist.

```
reference edge drift from its own grid: 2.711e-19 s
reference: 119 edges, 1.000000000 MHz, period sd 0.002 ps
locked 70-120 us: f_vco / f_ref = 3.999905
static phase error: 0.2141 ns, sd 0.3257 ns
control voltage: 0.499951 V against 0.500000 V, ripple 0.454 mV
jittered clock: 118 periods, sd 26.441 ns against 28.284 ns expected
```

Both clocks and the oscillator are event-driven: each edge's handler names the
time of the next one, so there is no clock node in the netlist and no period
written anywhere but in the model. That is what 5.10.3.3's sentence about a
changing `start_time` is for, and the first line is the measurement of it — the
k-th reference edge lands within a zeptosecond of k half-periods, measured by the
model against its own grid rather than by reading the waveform back.

The control voltage is the loop's own arithmetic: the oscillator runs at
`f0 + kvco * V(ctrl)`, so locking at four times the reference pins `V(ctrl)` at
`(4 MHz - 3 MHz) / 2 MHz/V` with nothing to tune. The static phase error is what
makes it a type-II loop: the charge pump drives a capacitor, so the error
integrates to zero rather than settling at an offset.

The jittered clock drives nothing. It draws each half-period from
`$rdist_normal`, so its period is the sum of two independent draws and its
standard deviation is `sqrt(2)` times theirs — which also checks that the seed
survives across timesteps rather than being re-drawn every Newton iteration.

Two things about this netlist are worth knowing before writing another like it.

**A grid-locked clock and a variable oscillator accumulate differently.** The
reference computes its next instant from the one it just served
(`next_edge = next_edge + period/2`) so that an edge taken a tolerance late
cannot move the grid; the oscillator computes it from the time the edge was
actually taken (`next_edge = $abstime + 0.5/freq`) so that a late edge cannot put
the next one in the past, where it would be served immediately and two
half-periods would cancel into no edge at all. Swapping them is the kind of
mistake that shows up as a clock that runs slightly fast, or one that stops.

**A detector that pumps all the time has a ripple it cannot get rid of.** The
first version of this netlist used a detector that held the pump in one direction
or the other at all times, and its control ripple works out to `zeta * N * wn` in
frequency however the filter is scaled — 6% of the carrier here, which the
oscillator then samples at its own edges, and the loop hunted instead of locking.
The tri-state detector stops pumping once the edges line up, which is what takes
the ripple to 0.45 mV and what the reset path in a real PFD is for.

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
