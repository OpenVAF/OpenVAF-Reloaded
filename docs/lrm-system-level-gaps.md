# VAMS-2023 conformance gaps for system-level modeling

What stands between OpenVAF-Reloaded today and writing system-level behavioral
models (PLLs, data converters, samplers, DSP chains, jitter and noise budgets) in
Verilog-A, measured against the Accellera VAMS-2023 LRM.

Every status below was established by compiling a probe model with
`target/release/openvaf-r`, not by reading a support list. Clause numbers are
VAMS-2023.

The LRM has no clause called "system-level modeling". The features that flow
needs are spread across 4.5 (analog operators), 5.10 (analog events), 9.5, 9.13,
9.17 and 9.21 (file I/O, random distributions, kernel control, table lookup) and
10 (compiler directives). Chapter 7 and the `$driver_*` functions of 9.22/9.23
are mixed-signal: they need a digital engine and are out of scope for an
analog-only OSDI flow.

## What OSDI does not expose, and how much of it matters

OSDI 0.4 gives a model no way to say anything to the *integrator*. The descriptor
(`openvaf/osdi/header/osdi_0_4.h:204-236`) has `eval`, the loads, setup, given
flags and noise, and nothing else. ngspice has all three of the missing pieces
internally; none of them is reachable across the ABI.

| What a model needs | ngspice has it as | OSDI exposes | Workable substitute |
| --- | --- | --- | --- |
| Know a timestep was accepted | `DEVaccept` (e.g. `vsrcacct.c`) | nothing | `$abstime` movement — what retained state uses |
| Land a point at an exact time | `CKTsetBreak(ckt, t)` | nothing | **`bound_step`, and it works** |
| Announce a discontinuity | breakpoint + `CKTorder = 1` | `EVAL_RET_FLAG_LIM` only, which is `CKTnoncon++` | none |

The middle row is the one that decides how much of this matters, so it was
measured rather than assumed. A model that caps `$bound_step` at the distance
remaining to a chosen instant makes ngspice land on it exactly:

```
event at 3.7us          WITH bound_step        WITHOUT
                          3.456000000 us        3.456000000 us
                          3.500000000 us
                          3.588000000 us
                          3.700000000 us   <--  3.656000000 us
                          3.900000000 us        3.856000000 us
                        61 points total       59 points total
```

So `bound_step` is a working stand-in for `CKTsetBreak` as far as *placing* a
point goes, at a cost of a couple of extra timepoints. `timer` (5.10.3.3) needs
no ABI change, and neither does tightening `cross` into its `time_tol` box.

What `bound_step` cannot do is the other half of a breakpoint: ngspice cuts the
integration order to 1 at one (`dctran.c:493`), and capping a step does not. That
is why `$discontinuity` has no substitute — the integrator keeps extrapolating
across a jump with a history that no longer describes the waveform. It still
converges, by rejecting steps and shrinking until it gets through; it is a cost
and an accuracy risk at the jump, not a wall.

For system-level modelling specifically, that residue is small, because the
idiomatic style avoids it. 9.17.1 itself says discontinuity "created by switch
branches and filters, such as `transition()` and `slew()`, does not need to be
announced" — and smoothing edges with `transition()` is how behavioural models
are meant to be written. Clocks are the other case, and in a SPICE flow they
usually come from a real source: `vsrc` calls `CKTsetBreak` for its own corners,
so `@(cross)` on a clock node already gets breakpoint-placed edges for free.

An OSDI proposal for an accept callback plus a breakpoint/discontinuity request
is still worth making — it would retire the `$abstime` workaround, let
`$discontinuity` mean something, and cost fewer timepoints than capping. But it
gates far less than it first appears: it is a cleanup, not a prerequisite. The
precedent for how it should look is `absdelay`: a descriptor-level protocol where
the simulator owns the time-dependent part.

## Tier 0 — compiles, and then no simulator can run it

One item, found while probing this page's own claims: `absdelay` (4.5.7).

It was listed under "confirmed working", and the compiler's half of it is. The
other half is not the compiler's: the delayed output is emitted as an unknown
whose row the model deliberately leaves empty, and the descriptor hands the input's
history to the simulator (`OsdiAbsDelayInfo`, `osdi/src/metadata.rs:403`), because
history is the one thing a model evaluation does not have. No released simulator
implements that protocol. An unstamped row is a singular matrix:

```
Warning: singular matrix:  check node nad#implicit_equation_2
Error: Transient op failed, timestep too small
```

so in ngspice the model does not misbehave, it does not run. The in-tree mock
simulator refused it outright rather than implementing it, which is why no test
caught this.

Both halves now exist:

| | Realization | Where |
| --- | --- | --- |
| default | the descriptor protocol; the simulator owns the history | `patches/ngspice-absdelay-history.patch` implements it for ngspice-46 (SPARSE and KLU, plus the AC phase shift), and `openvaf/tests/mock_sim` for the in-tree tests |
| `--absdelay in-model` | the model keeps the history in retained state | needs nothing of the simulator; runs on a stock ngspice |

The in-model realization is a shift register of (time, value) pairs with linear
interpolation and `$bound_step` capped at their spacing, which is also its cost:
a delay is resolved to `window / (depth - 2)` and the run takes at least that many
steps per delay window (measured: 187 timepoints where the protocol takes 59), and
in a small-signal analysis it has no phase shift to offer, where 4.5.7 asks for
`exp(-j w td)`. That is why the protocol stays the default. Both are measured
against the clause's closed form in `sim_regression/absdelay`, Figure 4-4's
time-varying delay included.

Also fixed on the way: a delay argument that was neither a parameter nor a literal
(`absdelay(x, 2 * td)`) crashed the backend with "attempted to read undefined
value". The delay is read by the simulator out of the instance data, so it is an
output of the evaluation, but nothing said so and the pass that moves
op-independent work into instance setup left the eval function holding a value no
longer computed there.

## Tier 1 — accepted but not honoured

The worst category is a model that compiles, behaves differently from what it
describes, and says nothing about it. One item was in it; it now warns.

| Feature | Clause | Status |
| --- | --- | --- |
| `$discontinuity(n)`, n >= 0 | 9.17.1 | Still dropped, but **no longer silent**: warns as `ignored_discontinuity` (L019). |
| Analog variable persistence | 4.5.10, 5.10.2 | Fixed. Retention is granted wherever a read can precede its write. |
| `@(final_step)` | 5.10.2 | **Silently unconditional.** The body runs at every evaluation, not at the last point. |

### `@(final_step)`

Found while implementing 9.5, where it is the natural place to put `$fclose`, and
where being unconditional is not a harmless approximation: closing the file on the
first evaluation throws away everything the rest of the run would have written. It
cost an hour of looking at an empty log.

`@(initial_step)` was made conditional with the variable-persistence work;
`final_step` was not, and it cannot be in the same way, because nothing tells a
model which point is the last one. It is the mirror image of the accept callback
in the table above: the simulator knows, the ABI does not say.

The honest interim is to warn, the way `$discontinuity` now does — `L018`
(`unscheduled_event`) exists for exactly this shape of problem but only covers the
event *functions*, not the global events. Until then: a file does not need closing
for its contents to arrive, because the process exits through libc and libc flushes
what is open. Measured — the `filelog` case closes nothing and loses nothing.

`hir_lower/src/expr.rs` handles only `$discontinuity(-1)`, the form that belongs
with `$limit` (9.17.3); every other degree lowers to nothing, because OSDI has no
channel to announce a discontinuity. It now says so, points at `$bound_step` as
the thing that does reach the simulator, and notes that the `-1` form is
supported — so a model can no longer claim a discontinuity that never arrives.

Actually announcing it needs the OSDI facility described above, and it is the one
item on this page with no substitute: `bound_step` can cap a step but cannot cut
the integration order, which is the half of a breakpoint that matters at a jump.
Capping across the jump anyway would be a partial answer, but the degree argument
carries no time, so the cap would have to be invented — a policy decision worth
taking deliberately rather than silently. A model that smooths its edges with
`transition()` or `slew()` needs none of this; 9.17.1 says so itself.

### Analog variable persistence

Found while implementing `last_crossing`, and fixed with it. A variable assigned
inside an event handler already retained across timesteps — that is the machinery
behind `@(cross)` latches — but a variable **read before it was assigned** within
one evaluation read its initialization value instead of what it held last time.
That is the shape of 4.5.10's own period example, which copies `previous = latest`
inside the `@(cross)` handler before `latest = last_crossing(...)` runs further
down the block.

Retention is now decided by a conservative definite-assignment walk over the
analog block: a write settles a variable for what follows only if it is certain to
have happened, so anything that may not run — one arm of an `if`, a loop body, an
event handler — leaves it unsettled, and any read that can come first grants
retention. That single rule subsumes the old "assigned inside `@(cross)`" rule,
since a handler may not fire.

Fixing it required making `@(initial_step)` actually conditional. It had been
lowered unconditionally, with assignments to retained variables skipped so they
were not re-applied every evaluation — which silently meant an initializer only
ever worked if the value it wrote was zero. It is now guarded on a retained
"first evaluation" flag, so an initializer applies once and retention carries it.

The blast radius is real and worth knowing. Compact models wrap their parameter
precomputation in `@(initial_step)`: BSIM4 has ~4000 lines in one, and now
retains the 319 variables it defines rather than recomputing them on every Newton
iteration. Verified byte-identical DC sweep and transient output against the
previous compiler, so the numbers do not move — but instance data grows (~5 kB for
BSIM4) and the work moves from every evaluation to once per setup. `OSDItemp`
re-invokes `setup_instance`, so a temperature change re-runs the precomputation.

Of ten real compact models compiled before and after, seven were byte-identical;
BSIM4 grew 5.1%, PSP103 3.1%, HICUML2 1.9%.

## Tier 2 — monitored events

| Feature | Clause | Status |
| --- | --- | --- |
| `cross` | 5.10.3.1 | Works. Detects the crossing and guards the body. |
| `above` | 5.10.3.2 | Works. Rising-only, plus the initialization event. |
| `timer` | 5.10.3.3 | Works. Fires at `start_time` and every `period` after it. |
| `absdelta` | 5.10.3.4 | Parses and type-checks, **body runs every evaluation** (warns `L018`) — but see the note below. |

`above` shares `cross`'s crossing detection, with two differences the LRM is
explicit about: no `dir` argument, so it triggers only from below, and it also
fires when the expression is already positive before time has moved — "if the
expression is positive at the conclusion of the initial condition analysis that
precedes a transient analysis, the above() function shall generate an event".
Note the argument layout differs too: `enable` is the fourth argument, not the
fifth.

That initialization event is keyed on `$abstime`, **not** on the analysis flags.
ngspice reports `ANALYSIS_STATIC` only on the first Newton iteration of the
initial step and `ANALYSIS_TRAN` on the rest, so a flag-gated event fires on one
iteration and is then overwritten by the others — which is exactly how the first
attempt failed. The mock simulator cannot catch that, because the test supplies
the flags itself; `sim_regression/above_init` does.

One known imprecision: in a dc sweep `above` fires at every point where the
expression is positive rather than only where it crosses, because nothing commits
retained state while time stands still. 5.10.3.2 asks for crossings there and
does not control the sweep step to resolve them, so over-firing samples the same
value a crossing would.

`timer` keeps its next event time in a retained slot and caps `$bound_step` at
the distance remaining, which is the one place a model gets to *ask* for a
timepoint rather than wait for one. `sim_regression/timer` samples a ramp with no
clock node anywhere in the netlist: all five event instants land exactly, the
held values are exact to 0.00e+00 V, and exactly five events fire — no
double-firing across the Newton iterations of the step an event lands on.

### A start_time that changes

5.10.3.3: "If the start_time or period expressions change value during the
evaluation of the analog block, the next event will be scheduled based on the
latest value of the start_time and period." `period` was re-read every evaluation
and so already followed that; `start_time` was read once and kept, which was
recorded here as a deviation. It is the deviation that matters most of the three
tiers below, because that sentence is what lets an event schedule its own
successor:

```verilog
@(timer(next_edge)) begin
    state = !state;
    next_edge = $abstime + half + $rdist_normal(seed, 0.0, jitter);
end
```

That is a clock source with a per-cycle period — a DCO, a spread-spectrum source,
a divider that stretches one cycle, anything with jitter on it — and it is how a
PLL gets a reference and an oscillator without a clock node in the netlist. With
`start_time` read once it fired at t = 0 and then stood still for ever: measured,
0 edges in 10 us where there should have been 10. A second retained slot now holds
the value the live schedule was built from, and a different one reschedules.

`sim_regression/pll` is the system built on it, and the measurement of the
placement is the model's own: the k-th reference edge lands within 3e-19 s of k
half-periods.

### time_tol, which is no longer accepted and ignored

5.10.3.3 asks the simulator to place a point "within time_tol of an event", and
the note here used to be that placing it exactly on the event satisfies any
tolerance. That is true of a `start_time` that does not move. It is not true of
one the model computes, where the instant asked for is a running sum of intervals
and the simulator's time a running sum of steps: the two can disagree in their
last bits, and an instant missed by its last bit is an instant still ahead, so
the event does not fire and the cap asks for the attosecond in between — and then
for the one in between that, until the solver gives up with "timestep too small".

`time_tol` now opens the firing window on the early side and floors the step
asked for, and with none given the tool picks one, as the clause allows: a part in
1e12 of the instant itself. The same hole was in `transition`, which asked for a
timepoint on each corner of a ramp until it got one and could ask for a
femtosecond for ever, and in `cross`, whose default tolerance is a thousandth of
the step being taken and so followed that step down by a factor of a thousand per
evaluation. Both now have a floor that does not depend on the step it is
bounding.

None of the three is exotic. All of them were found in one netlist, by two
clocks and a divider: a divider output caught half way up its own transition ramp
is sitting exactly on a phase detector's switching point, and an oscillator edge
that coincides with a reference edge is what a locked PLL does every cycle.

`absdelta` is worth a scoping decision rather than an implementation. 5.10.3.4
says it "is only allowed in an initial or always block of a Verilog-AMS module":
it is a digital-side sampler for converting analog signals into real-typed
digital variables. Accepting it in an analog block, as today, is not something
the LRM sanctions. Consider rejecting it in Verilog-A instead of implementing it.

### Accuracy

`cross` and `above` now steer the timestep onto the crossing, which is what
5.10.3.1 means by "in addition, cross() controls the timestep to accurately
resolve the crossing". Detection alone could not: by the time two accepted points
straddle zero the crossing is behind us, and a model cannot ask for a step to be
rejected. So it predicts instead — the expression's rate over the last accepted
step extrapolates to the time it reaches zero, and `$bound_step` caps the next
step there. Each capped step lands closer, so the estimate sharpens as it
approaches.

`time_tol` is the floor that stops the refinement, which is also what "within
time_tol of the crossing" buys. With none given the tool picks one, as the clause
allows: a thousandth of the step already being taken, relative to whatever scale
the solver is working at and unable to collapse towards zero on its own.

The residual in `sim_regression/sample_hold` went from 7.5e-05 V to **0.00e+00**,
and `last_crossing`, which reads a `@(cross)`-driven sampler, from 2e-13 s to
0.00e+00 as a side effect. The tolerance there is now 1e-06 V, a thousand times
tighter than before, and the previous compiler fails it at 7.50e-05 V.

## Tier 3 — hard errors that block whole model classes

All of these produce `function 'x' is currently not supported by OpenVAF`, from
the `UNSUPPORTED` list at `sourcegen/src/hir_builtins.rs:29`.

Four entries have left this tier since it was written: the Z-transform filters
(4.5.12), `$table_model` (9.21), and both sides of 9.5. What is left of it is two
tasks, and there are four odds and ends besides.

| Feature | Clause | What it blocks |
| --- | --- | --- |
| `$ferror` | 9.5.7 | Reporting *which* error. The codes are implementation-defined and this runtime has nothing to tell them apart with; `$feof` covers the case a model can act on. |
| `$fmonitor` | 9.5.2 | Writing when an argument changes, which needs the change detected per argument. |
| `$simprobe` | 9.16 | Probing another instance's signals. |
| `$analog_node_alias`, `$analog_port_alias` | 9.20 | Node aliasing. |
| `$test$plusargs`, `$value$plusargs` | 9.12 | Command-line configuration of a model. |

Two of these interacted in a way worth calling out, and both halves now work. 5.10.3.3's own PRBS generator
example is:

```verilog
analog begin
    @(timer(0, period))
        x = $random + 0.5;
    V(out) <+ transition( x, 0.0, period/100.0 );
end
```

That example could not be written at all: `timer` did not schedule and `$random`
did not exist. Both are now implemented, so it runs.

The convergence concern turned out to be handled by the retained-state machinery
rather than needing anything new. The seed is an inout integer the function
mutates, so calling it in straight-line analog code would re-randomize on every
Newton iteration. But a seed variable is read before it is written, which earns
it retention, and a retained slot only commits when time advances — so every
iteration of one timestep reads the same committed seed and draws the same
number. A seed that is a parameter, a constant, or absent gets a retained slot of
its own, seeded from the expression on the first evaluation, per 9.13.2's "an
internal seed is created which is assigned the initial value".

9.13.3 does not spell the algorithm out; it defers to IEEE 1364 17.9.3, and
9.13.1 requires that `$random` "shall always return the same stream of values
given the same initial random_seed". Matching other simulators is therefore part
of being correct, so the implementation is checked against one: all eight
functions, 34 draws, values and advanced seeds alike, in
`openvaf/test_data/osdi/rng_stream.va`.

### Found while testing: `case` arms and analog operators

Fixed. An analog operator in the `default` arm of a `case` crashed the compiler:

```verilog
case (kind)
    0: val = transition(V(in) > 0.5, 0, 1n);
    default: val = transition(V(in) > 0.2, 0, 2n);
endcase
```

```
internal error: entered unreachable code: attempted to read undefined value
```

The same pair written as `if`/`else` always compiled, and so did two numbered
arms; it was the `default` arm specifically, because that one is lowered into the
block the last condition falls through to and that block was still open. An open
block answers a variable read with a placeholder phi to be filled in when it is
sealed, so a body that branches — any analog operator with a select in it — read
through the placeholder from a successor block, which is no longer the one that
gets filled, and the value reached codegen undefined. Each arm above it is lowered
into a block that is sealed first, deliberately, for exactly this reason.

A multi-modulus divider or a mode-switched buffer is the natural way to hit it.
`openvaf/test_data/osdi/case_default.va` builds the same choice twice, once each
way, and the integration test requires the two to agree rather than merely to
compile.

## Tier 4 — language and grammar

| Feature | Clause | Status |
| --- | --- | --- |
| `` `default_discipline `` | 10.2 | Not implemented; fails as ``macro '`default_discipline' has not been declared``. |
| `` `default_transition `` | 10.3 | Same. Directly relevant here: it sets the default rise/fall for every bare `transition()` in a file, which is exactly how system-level models get written. |
| Multi-dimensional arrays | 3.x, 4.2.14 | Parse error on the second subscript (`real g[0:1][0:1]`). 1-D arrays work. |
| `paramset` | 6.x | Parse error: `expected 'discipline', 'nature' or 'module'`. Matters more for device libraries than for system-level work. |

## A PLL, part by part

The page is organized by clause, which is the wrong shape for answering "can I
write a PLL in this". So, by part, with the state of each measured in
`sim_regression/pll` unless noted:

| Part | Written with | State |
| --- | --- | --- |
| Reference clock | `@(timer(next_edge))` rescheduling itself, `transition` for the edge | works; edges on their grid to 3e-19 s |
| Jitter | `$rdist_normal` on each half-period | works; sd within sampling error of `sqrt(2)` times the spec |
| Phase/frequency detector | `@(cross(V(ref) - vth, +1))`, arms cleared when both are up | works; `cross` steers the step onto each edge |
| Phase error as a number | `last_crossing` (4.5.10) | works; crossing times exact to 0.00e+00 s in `sim_regression/last_crossing` |
| Charge pump | `I(ctrl) <+ -transition(icp * (up - dn), 0, tr)` | works |
| Loop filter | RC in the netlist, or `laplace_nd`/`laplace_zp` (4.5.11) | works |
| Oscillator, event-driven | `@(timer)` with the period recomputed per edge | works; period exact, and the control voltage settles where the arithmetic says |
| Oscillator, phase-domain | `idtmod(freq, 0, 1, 0)` and a threshold | works, but nothing steers the timestep onto the wrap, so the edge lands where the solver stepped |
| Divider | `@(cross)` counting, `transition` on the output | works; exactly one output edge per `ndiv` input edges |
| Multi-modulus divider | an analog operator per `case` arm | works — it crashed the compiler until the `default`-arm fix above |
| Phase noise | `noise_table` / `flicker_noise` / `white_noise` (4.6) | works; the table has to be an array literal or a file, not a parameter array |
| Delay line, DLL | `absdelay` (4.5.7) | works, both realizations — see Tier 0 |
| Jitter or period logging | `$fstrobe` / `$fdisplay` to a file (9.5) | works; `sim_regression/filelog` checks a jitter sequence the model wrote itself |
| Stimulus from a file of vectors | `$fgets` and `$sscanf` (9.5.4) | works; `sim_regression/vecsrc` drives a source from a file and checks the waveform against it |
| Sigma-delta state for fractional-N | multi-dimensional arrays | **missing** for 2-D — Tier 4; a MASH needs only scalar accumulators, so this is a convenience |
| A bare `transition(x)` with a file-wide edge rate | `` `default_transition `` (10.3) | **missing** — Tier 4 |

The two realizations of the oscillator are worth the distinction. The
phase-domain one is the textbook form and it is exact in the sense that matters
for a large-signal sweep, but its output edge is a threshold crossing of a
sawtooth that nothing asks the solver to resolve, so the edge carries the solver's
step as jitter. The event-driven one places its own edges and therefore has none
of that, at the cost of sampling the control voltage once per edge rather than
continuously — which is what a real oscillator does anyway.

So nothing on this page now blocks writing a PLL, measuring one, or driving one
from a file. What is left in the table above is a convenience
(`` `default_transition ``), a shape of array nothing here needs, and the two 9.5
tasks a testbench does not reach for.

## 9.5 — files

Implemented: `$fopen`, `$fclose`, `$fdisplay`, `$fwrite`, `$fstrobe`, `$fdebug`,
`$fflush`, `$ftell`, `$fseek`, `$rewind`, `$feof`, `$fgets`, `$swrite`,
`$sformat`, `$sscanf`, `$fscanf`. The front end already knew all
of them — signatures, format-string checking and diagnostics were in place — so
what was missing was the lowering, a runtime in `openvaf/osdi/stdlib.c`, and three
decisions the clause does not make for you.

OSDI has nothing to say about files, so the model does it itself: the generated
library already calls `snprintf` through libc, and `fopen` and friends resolve the
same way. The descriptor tables live in the library, which is what 9.5.1 describes
— a descriptor is an integer, so passing one between instances has to mean the
same file to both.

**A descriptor's kind lives in its value.** 9.5.1 has two of them: `$fopen(name)`
returns a multichannel descriptor with a single bit set, `$fopen(name, mode)` a
file descriptor with the top bit set. That is why one integer argument serves every
output task, and why `$fdisplay(mcd_a | mcd_b, ...)` writes to both files at once.
Channel 0 of an mcd and the three reserved descriptors are standard input, output
and error; a model inside a simulator has no business writing to the process's
stdout, so those route to `osdi_log`, where `$display` already goes.

**`$strobe` and `$fstrobe` now write once per timepoint**, which is 9.4.1's "at the
end of the current simulation time" read as closely as an analog model can read it.
Before, `$strobe` was lowered identically to `$display` — once per Newton iteration,
which for a measurement log is 2x to 20x the lines wanted. The guard is the
uncommitted half of a retained slot, which is per instance and is written as soon as
it is assigned, so it distinguishes the first evaluation at a timepoint from the
iterations that settle it.

It cannot distinguish an *accepted* timepoint from an attempted one, because
nothing tells it: a step the solver goes back on keeps the line already written for
it, and the time column goes backwards there. `sim_regression/filelog` measures how
much of that there is (1325 lines of 9114) and the monotone subsequence is the
accepted run. This is the best argument on this page for the accept callback at the
top of it.

**`$fopen` of a file this library already has open returns the same descriptor.**
5.10.2 puts `@(initial_step)` in force "during the solution of the first point",
which is every Newton iteration of it, so the one place a model can open a file
once is a place it is asked to open it several times. Opening it again truncates
what the earlier iterations wrote and hands out a descriptor nothing holds, until
the thirty available run out — measured, three opens and two truncations on the
first timepoint of a three-iteration step. The clause says nothing either way, and
this is the reading that lets the idiom work. The cost is that a model wanting two
independent handles on one file gets one, which 9.5 offers no way to ask for
anyway.

### Writing back through an argument

9.5.3 and 9.5.4 put their result in an argument, which is the only way Verilog-A
has of expressing it -- there are no pointers, so there is nothing for a runtime
to write through. `$fgets` returns its line and `$sscanf` leaves its conversions
in the runtime for the lowering to read back one at a time; the lowering does the
assigning. The same shape as 9.13's seed, which has worked this way since
`$random`.

Which conversion produces what is settled at compile time from the format, so the
format of a scan has to be a literal -- the type checker requires one. A target
that is a variable, or one element of an array named by a constant index, is
assigned; anything else is accepted by the type checker and then not assigned,
which is a hole worth closing.

`$fscanf` scans a line rather than the file, so a conversion cannot span a line
break. For a file with one record per line, which is what a vector file is, that
is the same thing.

### Reading advances the file, and evaluation repeats

The trap that cost the most here, and it belongs to the language rather than to
9.5: **an analog block is evaluated several times for one timepoint, and a read
advances the file every time.** A `$fgets` in an event handler consumes a record
per Newton iteration rather than per event -- measured, three of four records
swallowed before the simulation had left t = 0.

A model cannot ask whether this is the first evaluation of a timepoint. `$strobe`
can, because the guard is built into it; nothing else can reach it. So the way to
read a file is to make the reading idempotent: `$rewind` and then read, so that
repeating it reads the same thing. `sim_regression/vecsrc` loads its records that
way and steps through them with `timer`.

That is also what `$fopen` does for writing, by handing back a descriptor it has
already opened rather than opening the file again, and it is one more argument for
the accept callback at the top of this page.

### A retained string

A string variable that needed retention crashed the compiler
(`unknown cast found Real -> String`): a retained slot is eight bytes meant for a
double, and a string is not a number. 4.5.10 makes no exception for strings, so
neither does retention now -- what goes in the slot is the pointer, which is what
a string value is here, and `commit_retained` copies those eight bytes rather
than assigning them as a number.

The slot starts as zero, which reads back as the empty string: the value a string
variable has before anything assigns one. Nothing frees what the pointer points
at, which is already true of every string this compiler builds at run time.

## Confirmed working

Recorded so it is not re-litigated. All probed: `ddt`, `idt`, `idtmod`,
`transition`, `slew`, `ddx`, `limexp`, `last_crossing`, all four
`laplace_*` forms
(4.5.11), `white_noise` / `flicker_noise` / `ac_stim` / `analysis` (4.6),
`$limit` (9.17.3), `$bound_step` (9.17.2), named events and `->` (5.10.4),
`initial_step` / `final_step` (5.10.2), `analog initial` (5.2.1), indirect
contributions (5.6.7), analog user-defined functions (4.7), 1-D arrays, strings
and string parameters, `aliasparam`, `$param_given`, `$port_connected`,
`$simparam`, `$temperature`, `$vt`, `$abstime`, `$finish` / `$stop` / `$error` /
`$info`, bus ports with `genvar` loops, the display tasks (9.4), and the file
tasks (9.5, see above).

## Suggested order

1. ~~**`$discontinuity`**~~ — done: warns as `ignored_discontinuity` (L019).
   Announcing it for real needs step 5.
2. ~~**`above`**~~ — done. Rising-only crossing plus the initialization event,
   verified against ngspice in `sim_regression/above_init`.
3. ~~**`last_crossing`**~~ — done. Retained state plus linear interpolation;
   4.5.10 does not control the timestep, so it needed no new infrastructure.
   Measures crossings to ~0.2 ps in `sim_regression/last_crossing`.
4. ~~**Analog variable persistence**~~ — done. Retention wherever a read can
   precede its write, plus a conditional `@(initial_step)`. 4.5.10's period
   example now measures the period.
5. ~~**`timer`**~~ — done. Retained next-event time plus a capped `bound_step`;
   verified against ngspice with no clock node in the netlist.
6. ~~**Close the `cross` tolerance box**~~ — done. Predictive `bound_step` from
   the extrapolated crossing; the sample-and-hold residual is now 0.00e+00 V.
7. ~~**`$random` / `$dist_*`**~~ — done. All eight functions, verified against
   the stream IEEE 1364 17.9.3 specifies.
8. ~~**Z-transform filters**~~ — done. Retained state plus T-periodic sampling
   plus `transition`, so largely a composition of 3, 5 and what already existed.
9. ~~**`absdelay` without a simulator**~~ — done, both realizations; see Tier 0.
10. ~~**A `start_time` that changes**~~ — done, with the tolerance floors the
    three step-controlling operators turned out to be missing. A PLL is the
    system that needs it, and `sim_regression/pll` is it.
11. ~~**File I/O, the output side**~~ — done. `$fopen` through `$feof`, with
    `$strobe` and `$fstrobe` writing once per timepoint instead of once per
    iteration.
12. ~~**A retained string variable**~~ — done. The slot holds the pointer.
13. ~~**`$fscanf` / `$sscanf`**~~ — done, with `$fgets`, `$swrite` and
    `$sformat`: the way *in* for a file of vectors.
14. **Warn on `@(final_step)`** — Tier 1, and a trap rather than a curiosity now
    that there are files to close. `L018` already exists; it needs to cover the
    global events.
15. **Reject a scan target that is not a place** — accepted and then not
    assigned, which is the silent kind of wrong this page is about.
16. **`` `default_transition `` / `` `default_discipline ``** — independent,
    small, and immediately visible to model writers.
17. **Multi-dimensional arrays** — still a parse error on the second subscript.
18. **OSDI proposal** — accept callback plus breakpoint/discontinuity request.
    No longer last on merit: it is what `$fstrobe` needs in order to mean what
    9.4.1 says, what would let a model read one record per timepoint, and what
    would retire the `$abstime` workaround and give `$discontinuity` something to
    say.
