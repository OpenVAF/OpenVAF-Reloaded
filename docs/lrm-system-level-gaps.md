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

That is also why `time_tol` is accepted and then ignored. 5.10.3.3 asks the
simulator to place a point "within time_tol of an event"; placing it exactly on
the event satisfies any tolerance. One deviation worth recording: the LRM says a
`start_time` that changes mid-simulation reschedules the next event, and this
reads `start_time` only while nothing is scheduled yet. `period` is re-read every
evaluation, so a changing period does follow the clause.

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

| Feature | Clause | What it blocks |
| --- | --- | --- |
| `zi_nd`, `zi_np`, `zi_zd`, `zi_zp` | 4.5.12 | Linear discrete-time filters. Sampled-data systems, digital filter models, sigma-delta modulators, any DSP chain. A unity Z-filter is a sample-and-hold with period T. |
| `$table_model` | 9.21 | Data-driven behavioral models from swept or measured data. Not merely unsupported: the name is commented out of the sysfun list (`hir_builtins.rs:207`), so it does not resolve at all. |
| `$fopen`, `$fclose`, `$fdisplay`, `$fwrite`, `$fstrobe`, `$fmonitor`, `$fscanf`, `$fgets`, `$sformat`, `$swrite`, `$sscanf`, `$fseek`, `$ftell`, `$feof`, … | 9.5 | File-driven stimulus and result logging — the normal way a system-level testbench gets vectors in and measurements out. |
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

Two analog operators in different arms of a `case` statement crash the compiler.
`transition()` trips it as readily as the distributions do, and predates them:

```verilog
case (kind)
    0: val = transition(V(in) > 0.5, 0, 1n);
    default: val = transition(V(in) > 0.2, 0, 2n);
endcase
```

The same pair written as `if`/`else` compiles and runs. Not in Tier 1 because it
is a crash rather than a wrong answer, but it is the kind of thing a model writer
hits without warning.

## Tier 4 — language and grammar

| Feature | Clause | Status |
| --- | --- | --- |
| `` `default_discipline `` | 10.2 | Not implemented; fails as ``macro '`default_discipline' has not been declared``. |
| `` `default_transition `` | 10.3 | Same. Directly relevant here: it sets the default rise/fall for every bare `transition()` in a file, which is exactly how system-level models get written. |
| Multi-dimensional arrays | 3.x, 4.2.14 | Parse error on the second subscript (`real g[0:1][0:1]`). 1-D arrays work. |
| `paramset` | 6.x | Parse error: `expected 'discipline', 'nature' or 'module'`. Matters more for device libraries than for system-level work. |

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
`$info`, bus ports with `genvar` loops, and the display tasks (9.4).

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
8. **Z-transform filters** — retained state plus T-periodic sampling plus
   `transition`, so largely a composition of 3, 5 and what already exists.
9. **`` `default_transition `` / `` `default_discipline ``** — independent,
   small, and immediately visible to model writers.
10. **`$table_model`, then file I/O** — the two largest self-contained items.
11. **OSDI proposal** — accept callback plus breakpoint/discontinuity request.
    Deliberately last: it would retire the `$abstime` workaround, give
    `$discontinuity` something to say, and cost fewer timepoints than capping,
    but nothing above is waiting on it.
