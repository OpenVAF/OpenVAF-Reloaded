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

OSDI 0.4 gave a model no way to say anything to the *integrator*, and no way to
be told anything by it. ngspice has all three of the missing pieces internally,
and all three are reachable across the ABI now, through the descriptor tail that
`docs/osdi-timepoint-protocol.md` specifies and
`patches/ngspice-osdi-timepoints.patch` implements.

| What a model needs | ngspice has it as | OSDI exposes | State |
| --- | --- | --- | --- |
| Know a timestep was accepted | `DEVaccept` (e.g. `vsrcacct.c`) | `accept` | implemented; the line, the records read and the retained values of a point are all final when it is. `$fstrobe` writes **7789 lines instead of 9114**, none from a point the solver went back on |
| Land a point at an exact time | `CKTsetBreak(ckt, t)` | `breakpoint_offset` | implemented, and `bound_step` remains the cheaper way |
| Announce a discontinuity | breakpoint + `CKTorder = 1` | `discontinuity_offset` | implemented; **14.8x** more accurate after a jump |

The middle row is the one that decided how much of this mattered, and it was
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

It turned out to be the cheaper way as well as the portable one. A model asks for
an instant now (`breakpoint_offset`), and honouring the request in ngspice costs
**64% more timepoints** than the cap for the same exact placement
(`sim_regression/timer`: 835 against 509), because a breakpoint there also cuts
the next step to a tenth of the last one. The request is worth having for
robustness, since an instant cannot be missed by a last-bit rounding the way a
cap can. But the expectation this page used to carry, that it would cost fewer
points than capping, was wrong, and the patch leaves that half off unless asked.

What `bound_step` cannot do is the other half of a breakpoint: ngspice cuts the
integration order to 1 at one (`dctran.c:493`), and capping a step does not. That
is what `discontinuity_offset` is for, and it is the half that paid: with Gear
integration, announcing a jump leaves the twenty timepoints after it **14.8x**
more accurate (`sim_regression/timepoints`), because Gear carries several past
points and a history that straddles a jump is wrong. With trapezoidal it changes
nothing and costs nothing: one past derivative, already the post-jump one.

The error *at* the jump is the same announced or not, in both methods. A step
that straddles a discontinuity is not accurately integrable, which is the
clause's own reason for telling models to smooth their edges.

For system-level modelling specifically, that residue is small, because the
idiomatic style avoids it. 9.17.1 itself says discontinuity "created by switch
branches and filters, such as `transition()` and `slew()`, does not need to be
announced" — and smoothing edges with `transition()` is how behavioural models
are meant to be written. Clocks are the other case, and in a SPICE flow they
usually come from a real source: `vsrc` calls `CKTsetBreak` for its own corners,
so `@(cross)` on a clock node already gets breakpoint-placed edges for free.

The proposal is written and implemented: `docs/osdi-timepoint-protocol.md`. It
followed `absdelay`'s precedent, a descriptor-level protocol where the simulator
owns the time-dependent part, and it gates far less than it first appeared: a
cleanup, not a prerequisite.

What the callback carries is everything that is final exactly when a timepoint
is: the `$fstrobe` line composed at it (9.4.1), the records its reads consumed
(9.5.4), and the values it retained for the next timestep (4.5.10). The descriptor
points at a function generated per module, because where a module keeps its state
is known only to the compiler, and the runtime does the rest. Without the call all
three fall back to `$abstime` moving forward, which is the same thing for every
point a solver does not go back on.

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
describes, and says nothing about it. Six items have been in it; four are fixed,
and the two that cannot be now warn.

| Feature | Clause | Status |
| --- | --- | --- |
| `$discontinuity(n)`, n >= 0 | 9.17.1 | Announced through the timepoint protocol. Honoured by a simulator that reads it; L019 says so. |
| Analog variable persistence | 4.5.10, 5.10.2 | Fixed. Retention is granted wherever a read can precede its write. |
| `@(final_step)` | 5.10.2 | Still unconditional, but **no longer silent**: warns as `unscheduled_event` (L018). Its analysis list is honoured. |
| `initial_step` ORed with another event | 5.10.1 | Fixed. It contributes its first-evaluation flag instead of leaving the body unconditional. |
| An analysis list on a global event | 5.10.2 | Fixed. One `analysis()` test per name; the list used to parse and then mean nothing. |
| A scan target that is not a place | 9.5.4 | Fixed. Rejected now, and an array element at an index the model computes is assigned. |

### The global events

Three separate things, all of them in the two sentences 5.10.2 spends on
`initial_step` and `final_step`. The first two are fixed; the third is the one
that cannot be.

**An `initial_step` that cancelled what it was ORed with.** 5.10.1's own one-bit
sampler is `@(initial_step or cross(V(smpl) - 2.5, +1))`, and it did not sample.
The body of an event control is guarded only if *every* element of the event
expression carries a runtime condition, and `initial_step` carried none: it was
special-cased ahead of the loop that collects them, for the case where it is the
whole expression, and was an element with nothing to offer in every other case.
So the body ran unconditionally and the sampler tracked its input, and the
warning the compiler did emit blamed the other half, "'cross' does not schedule an
event yet", which by then was no longer true of `cross`. It contributes its
first-evaluation flag from inside the loop now, like everything else.

**An analysis list that was accepted and ignored.** Either global event may name
the analyses it belongs to, and `@(initial_step("ac"))` is the first point of an ac
analysis and of nothing else. The list was collected during HIR lowering and then
read by nobody, which made it exactly as strong as no list at all: an initializer
meant for one analysis ran in every analysis, and a `final_step("tran")` writing a
summary file wrote one during the operating point of a dc sweep. In this tree,
HiSIM2's degradation log is written from a `final_step("tran")`. It is one
`analysis()` test per name, ORed, ANDed with whatever the event itself contributes.

Which names are worth putting in a list is a separate matter, and the same one
that cost `above` its first implementation: the flags are not stable across the
Newton iterations of one step. ngspice reports `ANALYSIS_STATIC` only on the first
iteration of the initial step and `ANALYSIS_TRAN` on the rest, so a body gated on
`"static"` runs on one iteration and the iterations after it write the retained
value back over what it did -- measured, `sim_regression/global_events` counts
zero. The names that name an *analysis*, `"tran"`, `"ac"`, `"dc"`, `"noise"`, are
held steady for a whole run and are the ones to use.

**A `final_step` that is every step.** Found while implementing 9.5, where it is
the natural place to put `$fclose` and where being unconditional is not a harmless
approximation: closing the file on the first evaluation throws away everything the
rest of the run would have written. It cost an hour of looking at an empty log.

`@(initial_step)` was made conditional with the variable-persistence work;
`final_step` cannot be, because nothing tells a model which point is the last one.
It is the mirror image of the accept callback in the table above: the simulator
knows, the ABI does not say. So it warns, under `L018` (`unscheduled_event`), which
exists for exactly this shape of problem and now covers the global events and not
only the event functions. Its analysis list does narrow it to the analyses the
list names, which is as far as the clause can be followed.

Nothing needs closing for its contents to arrive, as it happens: the process exits
through libc and libc flushes what is open. Measured: the `filelog` case closes
nothing and loses nothing.

Every degree is announced now. `$discontinuity(-1)` still belongs to `$limit`
(9.17.3) and is unchanged; every other degree leaves the lowest degree announced
during the evaluation in the instance data, where a simulator that reads the
timepoint protocol finds it. L019 still warns, because whether the announcement
arrives is the simulator's half of the contract and no released simulator reads
it yet; the warning now says that rather than that the compiler drops it.

What it is worth is measured in `sim_regression/timepoints`, and the answer
depends on the integration method: 14.8x more accurate after a jump with Gear,
and nothing either way with trapezoidal, which has no history to discard. A model
that smooths its edges with `transition()` or `slew()` needs none of it; 9.17.1
says so itself.

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
| `` `default_discipline `` | 10.2 | Implemented. A net declared without a discipline gets the one the directive names, which is what 10.2's own example relies on. |
| `` `default_transition `` | 10.3 | Implemented. The default rise and fall time of every transition filter that specifies neither, which is exactly how system-level models get written. |
| Multi-dimensional arrays | 3.2, 3.3 | Implemented, for as many dimensions as a declaration gives. One row of one is readable but not assignable. |
| Array parameters | 3.4.4 | Implemented, of one dimension or several: one parameter of as many elements as it was declared with, which a model card sets in one go. The size is fixed when the model is compiled. |
| `noise_table` from a model card | 4.6.3 | Implemented. A table whose values are not constants is read at run time, so a measured spectrum is a model card rather than a rebuild. A table of constants still folds into the model. |
| `paramset` | 6.x | Parse error: `expected 'discipline', 'nature' or 'module'`. Matters more for device libraries than for system-level work. |

### The directives that supply a default

Neither was accepted and then ignored: an unknown directive is an unknown
macro, so a file using either failed to compile and nothing was silently
different. What they needed was a way to reach the code that wants them,
because a directive is gone before there is a syntax tree. The state one sets
now travels with the tokens, the way `` `begin_keywords ``'s keyword set
already did, and is turned back into positions when the tree is built.

`` `default_transition `` is the rise and fall time of a transition filter that
specifies neither, and 4.5.8 adds "or are equal to zero (0.0)". An unspecified
time arrives at the lowering as a zero, so both halves of that sentence are one
test, made in the one place every filter passes through: the Z-transform filters
(4.5.12) say the same thing about their own transition time and get it from
there too.

Two readings had to be chosen, and both are worth recording.

**Where the directive may appear.** 10.3 says it "can be used only outside of
module definitions" and then, one sentence later, that "there are no scope
restrictions for this directive". The restriction is the sentence that can be
followed exactly: outside a module, the value in force where a module begins is
the value in force for every filter the module contains, so there is nothing to
approximate. Inside one it is an error that says so and says where to move it,
rather than a value that reaches some filters and not others.

**Which nets a default discipline reaches.** 10.2's qualifiers are the digital
net and variable types, and Verilog-A has nets of none of them: `wire` itself is
rejected as an unsupported net type. What it does have is nets declared with no
net type at all, which are of the default net type, `wire`, so an unqualified
directive reaches them and so does a `wire`-qualified one, in preference to the
unqualified one because 10.2 ends with "the more specific directives have higher
precedence over general directives". Any other qualifier would be in force and
apply to nothing, and warns instead.

What a directive names still has to be a discipline, and that cannot be checked
where the directive is read: a directive is processed long before a `discipline`
declaration is seen, and may name one that an included file declares later. So
it is checked at the nets that take it, which is also where a model writer can
see which net the complaint is about.

`` `resetall `` resets both, per 10.2, and is still unimplemented: it warns as
an unsupported directive, which it did before.

### Multi-dimensional arrays

3.2 declares `integer flag_array[0:8][0:3];` as one variable, and 3.4.8 spends
its whole example on a 3x3 coupling matrix, so this is a shape the LRM expects a
model to be written in. It was a parse error on the second subscript, in a
declaration and in every use.

Most of what it needed was already there, which is why this is smaller than it
looks: `Type` has always been an array of arrays, the OSDI layer has always
flattened a nested type to its base type and total length, and an array
variable has always been one MIR place per element. What was missing was the
middle: a declaration that collects more than one range, an expression that
chains more than one subscript, and the arithmetic from a chain of indices to
the one place it names.

That arithmetic is the row-major position, with the declared lower bounds kept
on the other side of the comparison rather than subtracted from the index, so
that a one-dimensional array still compares the index itself against a constant
and lowers exactly as it did before: `openvaf/test_data/osdi/arrays.va` produces
byte-identical MIR.

Three things turned up next to it.

**A string array crashed the compiler**, and had nothing to do with the number
of dimensions: a variable with no initializer gets a placeholder of its own type
to be checked against, and an array's was always a real. For a `real` or
`integer` array that is harmless, for `string names[0:2];` it is a type mismatch
on an expression that has no source location, which the diagnostic renderer
unwraps. The placeholder follows the array's base type now.

**`dim()` and `to_dim()` disagreed about which end a dimension list starts at**,
which was invisible while every array had one dimension and would have
transposed the shape of any array that reached `Type::union` with another.

**A row is readable but not assignable.** `v = m[1]` copies the row, because one
row of a multi-dimensional array is an array in its own right and the places
that back it are a contiguous run. `m[1] = v` is rejected instead: an
assignment destination here is one place, and a row is a run of them. The
diagnostic says so and says what to do.

What is still not implemented next to this is an array *variable's*
declaration initializer (`real g[0:1] = '{1, 2};`), which is rejected as a scalar
type mismatch: an array variable is initialized by an assignment pattern in the
analog block, which does work, nested and all.

### Array parameters

The other half of 3.4.8's example: the coupling matrix there is a `parameter real
c[0:2][0:2]`, not a variable, because the numbers a particular device was fitted
with belong to the netlist rather than to the source. The declaration was a parse
error on the subscript, for one dimension as much as for several.

What the two halves need is not the same thing, which is why this is a separate
item from the row above. An array variable is one MIR place per element and
nothing outside the model ever sees it. An array parameter has to be two things at
once: **one** parameter to the simulator, which sets it as a whole and of the
exact declared size (3.4.4), and **one value per element** to the model, which
reads them one at a time.

OSDI already had the simulator's half. `OsdiParamOpvar` carries a `len`, the
access function hands out one pointer per parameter whatever its type, and
ngspice's `osdiparam.c` has always written a vector parameter as a `memcpy` of
`len * sizeof(double)` and refused an override of the wrong length. That is
3.4.4's exact-size rule, implemented by a simulator for a declaration the compiler
could not parse. So no ABI change was needed, and `.model mfit polyres
c=[1e-6 2e-3 5e-4 1e-4]` works the day the front end accepts the declaration.

The model's half is a new input kind: `ParamKind::ParamElement(param, i)`, one per
element in row-major order, alongside one `ParamGiven` flag for the array, since
an override replaces all of it. Each element takes its default from its own
position in the assignment pattern, and each is checked against the declared range.
A `from [0:100]` applies to every element, so an array with one element outside it
is an invalid parameter rather than a silently accepted one.

Two things followed from being able to declare one.

**An array parameter is also an array argument.** 4.5.1 says an analog operator
that takes an array takes "an `array_identifier` (e.g. an array parameter or an
array variable)", so a filter's coefficients can come from the model card:
`laplace_nd(V(in), num, den)` with `den` a parameter array is what
`sim_regression/arrayparam` measures, and the pole lands where the card says.
The operators that build their table or their coefficients at *compile* time
cannot take one, and now say so: `$table_model` and the `zi_*` filters reject an
array parameter with a diagnostic. `noise_table` would otherwise have accepted
one (a parameter is a constant expression, which is all its check asked for) and
compiled an empty spectrum, so it was made to reject one too, which is where the
next section starts.

**Indexing something that is not an array crashed the compiler**, and had nothing
to do with parameters: `x[0]` on a scalar typed as `Err`, and the cast inserted
into an `Err` hit an `unreachable!`. It is a diagnostic now, for a variable as
well as for a parameter, which is the first mistake anyone makes with a feature
whose whole point is the subscript.

What 3.4.4 allows and this does not is resizing: the declared range may be sized
by a parameter, and the clause lets an override of that parameter change the array
size. The size here is fixed when the model is compiled, because the simulator's
slot has one. `parameter real s[0:n-1]` works and is sized by `n`'s default; a
model card that sets `n` does not resize `s`.

### A noise table at run time

4.6.3's `noise_table` takes a vector of frequency-power pairs, and until now
those pairs had to be known while the model was being compiled: an array of
constants, or a two-column data file read at compile time. The interpolation was
emitted as a chain of selects with every segment's slope folded in, which is as
cheap as a table of constants deserves. What it is not is something a netlist can
set, and a measured noise spectrum is the clearest case on this page of a number
that belongs to the part rather than to the source: one compiled model, one card
per part number.

So a table whose values are not constants is read at run time instead. The pairs
travel as the arguments of the noise callback, two to a pair, and arrive as
`NoiseSourceKind::NoiseTableDyn` with a MIR value each. Each value gets an eval
output, which for an array parameter's element is not a slot at all but a read of
the model data where the simulator wrote it, and `load_noise` interpolates over
those values rather than over constants: a division per segment instead of a
folded slope, and a `log10` per frequency for a linear table, since
`noise_table_log`'s own column is already in log10.

Which table is which is decided by what the values are, not by where they came
from. An array of literal constants and a data file still build the table into the
model, unchanged; anything else (an array parameter, an array the module filled
in, an assignment pattern with an expression in it) is read at run time.
`sim_regression/noisetab` puts both forms of the same numbers in one model and
requires them to agree, which they do exactly, at all 91 frequencies of a sweep.

Three things come with it.

**Sorting cannot happen at run time.** A table of constants is sorted and
deduplicated while it is built, so its pairs may be written in any order. A
run-time table is interpolated in the order the array declares them, so its
frequencies have to ascend. A repeated frequency is harmless, because the segment
it spans is empty and the infinity its slope divides to is never the one
selected, but a table out of order is a table read out of order.

**An empty table used to compile.** `noise_table({})` contributed nothing and
said nothing about it; nor did a table with an odd number of values, minus the
last one. A pair is two numbers, so both are diagnostics now: "the table is a
vector of (frequency, power) pairs, and 3 values is not a whole number of them".

**A bank of them is a matrix.** One row of a two-dimensional array is an array in
its own right, so `noise_table(bank[1], "...")` is the second of as many spectra
as the card declares, read with the row machinery 3.2's arrays already use.

What this does not extend to is the two other operators that build their data in.
`$table_model` reduces its samples to a piecewise polynomial so that the existing
autodiff can differentiate the lookup, and 9.21.1 captures the data source on the
first call in any case; the `zi_*` filters fold their coefficients into the
recurrence. Both still reject an array parameter, and both say so in terms of a
model card.

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
| Loop filter | RC in the netlist, or `laplace_nd`/`laplace_zp` (4.5.11) | works, and its coefficients can be an array parameter the model card sets (`sim_regression/arrayparam`) |
| Oscillator, event-driven | `@(timer)` with the period recomputed per edge | works; period exact, and the control voltage settles where the arithmetic says |
| Oscillator, phase-domain | `idtmod(freq, 0, 1, 0)` and a threshold | works, but nothing steers the timestep onto the wrap, so the edge lands where the solver stepped |
| Divider | `@(cross)` counting, `transition` on the output | works; exactly one output edge per `ndiv` input edges |
| Multi-modulus divider | an analog operator per `case` arm | works — it crashed the compiler until the `default`-arm fix above |
| Phase noise | `noise_table` / `flicker_noise` / `white_noise` (4.6) | works, and the table can be an array parameter the model card sets (`sim_regression/noisetab`), or an array literal or a data file built into the model |
| Delay line, DLL | `absdelay` (4.5.7) | works, both realizations — see Tier 0 |
| Jitter or period logging | `$fstrobe` / `$fdisplay` to a file (9.5) | works; `sim_regression/filelog` checks a jitter sequence the model wrote itself |
| Stimulus from a file of vectors | `$fgets` and `$sscanf` (9.5.4) | works, loaded into arrays (`sim_regression/vecsrc`) or streamed a record at a time (`sim_regression/vecstream`), both checked against the file |
| Sigma-delta state for fractional-N | multi-dimensional arrays | works; `sim_regression/multidim` measures a matrix of them against its own arithmetic |
| A bare `transition(x)` with a file-wide edge rate | `` `default_transition `` (10.3) | works; `sim_regression/directives` measures the ramp it sets |

The two realizations of the oscillator are worth the distinction. The
phase-domain one is the textbook form and it is exact in the sense that matters
for a large-signal sweep, but its output edge is a threshold crossing of a
sawtooth that nothing asks the solver to resolve, so the edge carries the solver's
step as jitter. The event-driven one places its own edges and therefore has none
of that, at the cost of sampling the control voltage once per edge rather than
continuously — which is what a real oscillator does anyway.

So nothing on this page now blocks writing a PLL, measuring one, driving one
from a file, or shaping one from its model card, and nothing is left in the table
above: what remains in Tier 4 is `paramset`, which matters for device libraries
rather than for system-level work, and the two 9.5 tasks a testbench does not
reach for.

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

**`$strobe` and `$fstrobe` write once per timepoint**, which is 9.4.1's "at the end
of the current simulation time" read as closely as the simulator allows. Before,
`$strobe` was lowered identically to `$display`, once per Newton iteration, which
for a measurement log is 2x to 20x the lines wanted.

Which timepoint, and which evaluation of it, depends on what the simulator can
say. A `$fstrobe` line is composed and held, and written when the accept callback
says the point was accepted: the values of the evaluation that converged, once per
accepted point, and nothing at all for a point the solver went back on. Measured
in `sim_regression/filelog`: 7789 lines where a simulator without the callback
gets 9114, the 1325 difference being exactly the points that were gone back on.

Without the callback the line is written as soon as it is composed at a new time,
which is one line per *attempted* point from its first evaluation: the time column
goes backwards where the solver did, and the monotone subsequence is the accepted
run. A line to the simulator's own log (`$strobe` rather than `$fstrobe`) is always
written that way, because the handle it is written through is only valid during
the call it came with and a held line has no handle left.

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
format of a scan has to be a literal -- the type checker requires one. It also
requires each target to be something that can be written to, and to be able to
hold what its conversion produces: `$sscanf(line, "%e", p)` on a parameter, or a
`%s` into a real variable, used to be accepted and then quietly dropped. The
conversions and the type check read the format through the same function the
lowering uses, so the two cannot disagree about which argument receives which
conversion.

A target is a variable or one element of an array, at any index -- including one
the model computes, which is written by choosing among the elements exactly as
`arr[i] = x` is. `sim_regression/vecsrc` scans straight into its arrays because of
that; before, it counted four records and stored none of them.

The cast between a conversion and its target is the one an assignment would do:
`$sscanf("8.75", "%d", r)` with a real `r` leaves 8.0 in it, because `%d` says
what to read out of the text and `r` says what holds it.

`$fscanf` scans a line rather than the file, so a conversion cannot span a line
break. For a file with one record per line, which is what a vector file is, that
is the same thing.

### Reading advances the file once per timepoint

The trap that cost the most here, and it belonged to the language rather than to
9.5: **an analog block is evaluated several times for one timepoint, and a read
advanced the file every time.** A `$fgets` in an event handler consumed a record
per Newton iteration rather than per event, measured as three of four records
swallowed before the simulation had left t = 0.

9.5's reads are sequential, and the rate that a sequence of records can be read at
is per timepoint: a model has no way to be evaluated once. So a read-only
descriptor now remembers where it stood when the point began, every evaluation of
that point starts its reads there, and the point advances the file by what the
model read once. With the accept callback at the top of this page, "the point"
means the accepted point, so one the solver goes back on consumes nothing.

`sim_regression/vecstream` is what that makes possible: a vector source written
the way the clause reads, a record at a time, with no arrays and no rewinding.
Before, it took 3 of the 30 records that were due and held the right level for 4
of 30; after, 30 and 30. `sim_regression/vecsrc` does the same job the other way,
loading the whole file into arrays inside `@(initial_step)` with a `$rewind` to
make the repeated loading harmless, which is now a choice rather than the only
way to read a file.

A model still cannot ask whether this is the first evaluation of a timepoint;
`$strobe` can, because the guard is built into it. What it no longer needs to ask
is whether a read has already happened.

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
`initial_step` (5.10.2, `final_step` with the caveat in Tier 1), `analysis` lists
on both, `analog initial` (5.2.1), indirect
contributions (5.6.7), analog user-defined functions (4.7), arrays of any number
of dimensions, strings
and string parameters, `aliasparam`, `$param_given`, `$port_connected`,
`$simparam`, `$temperature`, `$vt`, `$abstime`, `$finish` / `$stop` / `$error` /
`$info`, bus ports with `genvar` loops, the display tasks (9.4), the file
tasks (9.5, see above), and the compiler directives of 10.2 through 10.6.

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
14. ~~**Warn on `@(final_step)`**~~ — done, and with it the two deviations
    probing it turned up: an `initial_step` ORed with another event contributed no
    condition, and an analysis list on either global event was accepted and
    ignored. `sim_regression/global_events` measures all three.
15. ~~**Reject a scan target that is not a place**~~ — done. Rejected if it is
    not one, assigned if it is: an array element at a computed index now arrives,
    and the cast to the target's own type is the one an assignment would do.
16. ~~**`` `default_transition `` / `` `default_discipline ``**~~ — done. The
    state a directive sets travels with the tokens, which is what both clauses
    mean by the text stream that follows the directive.
17. ~~**Multi-dimensional arrays**~~ — done, for as many dimensions as a
    declaration gives. The row-major position of an index chain, with the
    one-dimensional case lowering byte-identically to what it did before.
18. ~~**OSDI proposal**~~ — done, all three facilities, written as
    `docs/osdi-timepoint-protocol.md` and patched into ngspice: a discontinuity
    can be announced (14.8x more accurate after a jump, with Gear), an instant can
    be asked for (which ngspice charges 64% more timepoints for than the cap it
    would replace), and a timepoint can be confirmed, which is what `$fstrobe`
    needed in order to mean what 9.4.1 says (7789 lines instead of 9114, none from
    a point the solver went back on).
19. ~~**A generated accept function**~~ — done. The descriptor points at a
    function generated per module, which hands the runtime the state only the
    compiler can locate: so 9.5's reads advance once per accepted timepoint
    (`sim_regression/vecstream`, 3 of 30 records before and 30 of 30 after) and
    retained state commits because a point was accepted rather than because
    `$abstime` moved. Probing the proxy it replaces did not find it wrong, which
    was worth knowing: the lowering rewrites every retained slot at the end of
    every evaluation, so a rejected point leaves nothing stale behind. What the
    callback adds is that reads and retained state cannot disagree about what a
    timepoint was.
20. ~~**Array parameters**~~ — done, of one dimension or several. One parameter of
    as many elements as it was declared with, which a model card sets in one go
    (`.model mfit polyres c=[1e-6 2e-3 5e-4 1e-4]`) and the model reads an element
    at a time, including as an operator's array argument (4.5.1), so a filter's
    pole can come from the card. ngspice already had its half, `len` and the
    exact-size check included, for a declaration the compiler could not parse.
    `sim_regression/arrayparam` measures a fitted polynomial and a pole against
    the numbers in the netlist, and against the same two devices written with one
    parameter per element: 6019 of 6019 rows identical.
21. ~~**A noise table from a model card**~~ — done. A table whose values are
    not constants is read at run time, so a measured spectrum is a card rather
    than a rebuild; one of constants still folds into the model, unchanged.
    `sim_regression/noisetab` has both forms of the same numbers in one model and
    they agree exactly at all 91 frequencies of a sweep, while a second card moves
    the one and leaves the other where it was. What came with it: an empty or
    odd-length table is a diagnostic instead of a spectrum of nothing, one row of
    a two-dimensional array is a table (so a bank of them is a matrix), and a
    run-time table has to ascend in frequency, because nothing can sort it there.
22. **`$table_model` and the `zi_*` filters from a model card** — the two
    operators whose data is still built in. `$table_model` reduces its samples to
    a piecewise polynomial, which is what lets the existing autodiff
    differentiate the lookup, so a run-time table has to emit that same
    arithmetic over values: the comparisons stay selects, whose derivative is a
    select, so it should carry its Jacobian the way the compiled-in form does.
    The `zi_*` coefficients are the same shape of problem one step further in,
    since they multiply a retained recurrence. A measured I(V) curve or a
    measured Kvco per part number is the thing this buys, the way a measured
    spectrum was for item 21.
