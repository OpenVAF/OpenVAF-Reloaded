# A timepoint protocol for OSDI

What a Verilog-A model has to be able to say to the integrator, what OSDI 0.4 gives
it no way to say, and what it costs to add. This is the proposal that
`docs/lrm-system-level-gaps.md` has been pointing at, written after implementing
two thirds of it and measuring the result.

Three things are missing, in the order they were measured rather than the order
they were expected to matter:

| What a model needs to say | Status here |
| --- | --- |
| "the solution is discontinuous at the point you just solved" (VAMS-2023 9.17.1) | implemented and measured: **14.8x** more accurate after a jump |
| "place a timepoint at this instant" (5.10.3.1, 5.10.3.3, 4.5.8) | implemented and measured: it works, and in ngspice it costs 64% more timepoints than the `$bound_step` cap it would replace |
| "a timepoint was accepted" (9.4.1, 9.5) | implemented and measured: `$fstrobe` writes **7789 lines instead of 9114**, and none of them for a point the solver went back on |

## The shape of it

Nothing new in the calling convention. Both implemented halves are a number the
model leaves in its instance data, which the simulator reads after `eval`:

```c
  /* additions to the OSDI 0.4 descriptor tail */
  uint32_t breakpoint_offset;      /* double: the instant wanted, +inf for none */
  uint32_t discontinuity_offset;   /* double: the degree announced, < 0 for none */
  void (*accept)(void *handle, void *inst, void *model, OsdiSimInfo *info);
```

The first two are what a model says *during* an evaluation; the third is the
simulator's turn to speak, and the next section is what it is for.

That is the same mechanism `$bound_step` has always used (`bound_step_offset`) and
the same one `absdelay` uses for the delay it hands over, so a simulator that
implements either of those already has the code that reads these. Both are
`UINT32_MAX` when the module never asks, so a simulator can tell from the
descriptor alone whether a model speaks the protocol, before it runs anything.

Being in the tail matters: a descriptor is read by offset, so new fields at the end
change none of the old ones, and `OSDI_DESCRIPTOR_SIZE` already tells a simulator
how far the file's own descriptor reaches. ngspice's reader keeps that size per
registry entry (`patches/ngspice-absdelay-history.patch`), so an older model and a
newer one load side by side.

### Combining requests

An evaluation can reach several `$discontinuity` calls and several operators that
want a point. One slot each is enough, because both requests are re-made at every
evaluation:

* the **breakpoint** slot keeps the earliest instant asked for, exactly as
  `$bound_step` keeps the smallest cap. An operator that wants a series of points
  asks for its next one every time it is evaluated, so a request that loses is
  made again once the winner has passed.
* the **discontinuity** slot keeps the *lowest* degree announced, because a jump in
  the value is worse news than a jump in a derivative, and a simulator that can
  only restart its integration should hear the worse one.

### What a simulator does with them

The protocol carries information; the policy is the simulator's, and the two halves
are deliberately separate signals because the right policy is not the same.

**A discontinuity** is about the point that has just been solved, so there is
nothing to place: the useful response is to not integrate across it, i.e. to
restart the integration there. In ngspice that is `CKTorder = 1`, which is what
`dctran.c` already does on the first point after a breakpoint.

That response is worth something only to a method that carries history across the
point. Measured in `sim_regression/timepoints`, the same model one parameter apart:

```
                                            worst error in the 20 timepoints after a jump
Gear, maxord 4   announced                   9.539e-04 V
                 silent                      1.414e-02 V      14.8x
trapezoidal      announced == silent         3.225e-06 V
```

Gear keeps several past points, so a history that straddles the jump is wrong and
discarding it is a 14.8x win. Trapezoidal is a one-step method whose only past
value is the derivative at the point just solved -- which is the post-jump one, and
is what the next step wants -- so cutting its order would spend an order of
accuracy for nothing. The patch therefore cuts the order for Gear only, and the
trapezoidal runs are bit-identical with and without the announcement. A model that
announces a discontinuity never pays for announcing it.

The error *at* the jump (3.6e-02 V with Gear, 2.9e-02 V with trapezoidal) is the
same either way and no announcement can help it: a step that straddles a
discontinuity is not accurately integrable by any multistep method. That is why
9.17.1 tells models to smooth their edges with `transition()` or `slew()` in the
first place, and why the case that measures this is written without one.

**A requested instant** is about the future, so the response is to place a point
there. In ngspice that is `CKTsetBreak`, which also cuts the following step to a
tenth of the last one -- a breakpoint there means "something happens here", and
ngspice protects itself accordingly. Measured on `sim_regression/timer`:

```
cap only (stock ngspice)                rows  509   held values exact to 0.00e+00 V
request present, ignored (patched)      rows  509   identical, bit for bit
request honoured                        rows  835   held values exact to 0.00e+00 V
```

So in ngspice the request is a 64% cost for no accuracy: `$bound_step` had already
placed the instants exactly, which is what PR #47's tolerance floors were for. The
patch leaves this half off unless `OSDI_TIMEPOINT_BREAKPOINTS` is set.

That is not an argument against the facility, but it is an argument against the
expectation this page used to carry (that a breakpoint would cost *fewer*
timepoints than capping). What the request buys is robustness rather than cost: an
instant asked for cannot be missed by a last-bit rounding, which is exactly the
failure mode that capping had before those floors ("timestep too small", from a
cap that halved an attosecond at a time). A simulator whose breakpoints do not
reprice the next step would get the placement for nothing.

## The third facility: an accept callback

```c
  void (*accept)(void *handle, void *inst, void *model, OsdiSimInfo *info);
```

Called once per accepted timepoint, after the solution is final and before the next
step is attempted, with `eval`'s own arguments except that `info` may be null: a
simulator that has no `OsdiSimInfo` to hand at that moment passes none, and nothing
an accepted-point task does needs one. A null field means the model has no
accepted-point work, so a simulator can skip the call and most models never offer
it.

### What it buys, measured

`$strobe` and `$fstrobe` are what 9.4.1 gives "at the end of the current simulation
time", which is a moment an analog block cannot reach by itself: it is evaluated
many times while the solver settles one point, and nothing in it knows which
evaluation was the last. Until now the closest reading was "the first evaluation at
a new time", with two consequences, both visible in `sim_regression/filelog`: the
line carries the values of the *least* converged iteration of its point, and a
point the solver goes back on leaves a line behind that the next accepted point
contradicts.

With the callback the line is composed as before but held, and written when the
point is accepted:

```
                                   lines   from a timepoint the solver went back on
without the callback                9114   1325
with it                             7789      0
```

The 1325 are exactly the difference. What the model measured does not change (the
same 100 periods, agreeing with their own edges to 9e-18 s); what changes is which
evaluation of each point the line comes from, and that the points that never
happened say nothing.

Nothing changes for a simulator that does not make the call: the runtime writes the
line as soon as it is composed at a new time, which is what this task did before
the callback existed, down to the iteration it picks. The first accepted point of a
run is written that way too, because its line is composed before the first `accept`
arrives.

### What else it carries

The two other things this page listed as waiting for the callback are done with it
too, and what the descriptor points at is now a function generated per module,
because where a module keeps its state is known only to the compiler. The ABI above
did not change for that.

**Reads that advance once per timepoint.** 9.5's reads are sequential: each takes
the next record. Taken literally in a block that is evaluated many times for one
point, that is a record per Newton iteration, which is a rate nothing in the clause
suggests and which no model can work with: a file of one record per timepoint is
consumed two to three times too fast. So a read-only descriptor remembers where it
stood when the point began, every evaluation of that point starts its reads there,
and the point advances the file by what the model read once. With the callback,
"the point" means the accepted point, so a point the solver goes back on consumes
nothing. `sim_regression/vecstream` is a vector source written the way the clause
reads, a record at a time with no arrays and no rewinding, which could not be
written at all before:

```
                              records taken    records whose level the
                              of 30 due        waveform holds
before                         3               4 of 30
after                         30              30 of 30
```

**Retained state that commits because a point was accepted.** Everything this tree
keeps across timesteps used to commit by noticing that `$abstime` had moved
forward. Probing that proxy did not find it wrong: a rejected step is retried at an
earlier time, which the comparison sees, and the lowering rewrites every retained
slot at the end of every evaluation, so a rejected point cannot leave a stale value
behind either. Measured on 300 rejections in a model whose state is written only
under a condition, and again on one written only inside an `@(cross)` handler: no
committed value came from a point that was not accepted.

What made it worth wiring anyway is that retained state and file positions are one
state. Rolling a rejected point's reads back while its retained counter commits
leaves a model holding a value from a record the file has moved past, which is a
source that has lost its place. So both now commit at the accepted point when the
simulator says there was one, and both fall back to `$abstime` when it does not.

## What this does not propose

**A way to reject a timepoint.** A model cannot ask for the step to be taken again,
and nothing here changes that. `$bound_step` and a requested instant are both
forward-looking; an event that is discovered late stays discovered late.

**A say in the solution itself.** The protocol is about *when* the simulator
solves, not about what it solves. `$limit` (9.17.3) already has its own channel.

**A version bump.** The three fields extend the 0.4 tail, which
`OSDI_DESCRIPTOR_SIZE` makes safe, and this tree's `absdelay` fields set that
precedent. For an upstream OSDI release they are the natural contents of an 0.5,
with the accept callback's `NULL` and the offsets' `UINT32_MAX` as the "not used"
encodings.

One implementation note that is not in the ABI but bit once: a simulator's handle
is only valid for the duration of the call it came with (ngspice builds one on
the stack per evaluation), so a line held for later cannot be written through it.
That is why the accept callback is given one of its own, and why a line going to
the simulator's own log rather than to a file is not held at all: `$strobe` keeps
the first-evaluation reading that `$fstrobe` has now left behind.

## Checking an implementation

* `openvaf/test_data/osdi/timepoints.va` plus `test_timepoints` in
  `openvaf/openvaf/tests/integration.rs`: the mock simulator reads both slots and
  the test pins what is in them, timepoint by timepoint -- the instant asked for
  moves to the next event once the current one is consumed, and the announcement is
  about one point rather than about the state it left behind.
* `sim_regression/timepoints`: the two runs above, which also assert that a
  simulator which ignores the protocol produces the same waveform either way.
* `openvaf/test_data/osdi/accept_log.va` plus `test_accept`: six timepoints, the
  first three unaccepted, the next two accepted and the last evaluated and left,
  with a value that changes from one evaluation of a point to the next -- so each
  line says which evaluation it came from, and the point that was never accepted
  says nothing at all.
* `sim_regression/filelog` runs the same model twice when `NGSPICE_TIMEPOINTS` is
  set, and requires that the accepting run write fewer lines and none from a
  timepoint the solver went back on.
* `openvaf/test_data/osdi/read_point.va` plus `test_read_point`: one record per
  point, with every evaluation of a point seeing the same record, and a point that
  is evaluated and never accepted leaving the file and the record count where it
  found them. That last part is the one case `$abstime` cannot recognize on its
  own, because the next attempt is at a later time either way.
* `sim_regression/vecstream`: the streaming vector source above, run under both
  binaries, with the file as the reference.
* `patches/ngspice-osdi-timepoints.patch` is the ngspice side, on top of
  `patches/ngspice-absdelay-history.patch` (which is where the per-entry descriptor
  size comes from). `OSDI_TP_TRACE=1` prints each announcement as it arrives, which
  is how the first end-to-end check was made.
