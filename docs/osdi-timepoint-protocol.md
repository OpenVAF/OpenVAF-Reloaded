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
| "a timepoint was accepted" (9.4.1, 9.5) | specified below, not implemented |

## The shape of it

Nothing new in the calling convention. Both implemented halves are a number the
model leaves in its instance data, which the simulator reads after `eval`:

```c
  /* additions to the OSDI 0.4 descriptor tail */
  uint32_t breakpoint_offset;      /* double: the instant wanted, +inf for none */
  uint32_t discontinuity_offset;   /* double: the degree announced, < 0 for none */
```

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

Not implemented. The specification is small, and so is the list of things waiting
for it:

```c
  /* a further addition to the tail */
  void (*accept)(void *handle, void *inst, void *model, OsdiSimInfo *info);
```

Called once per accepted timepoint, after the solution is final, before the next
step is attempted. The arguments are `eval`'s, so an implementation has everything
it needs; `NULL` (or a descriptor that does not reach the field) means a model has
no accepted-point work and the simulator should skip it.

What it would retire, in the order the rest of this tree wants it:

1. **`$strobe` and `$fstrobe` would mean what 9.4.1 says.** "At the end of the
   current simulation time" is currently read as "the first evaluation at a new
   time", because that is the only thing an analog model can detect. Two
   consequences, both measured in `sim_regression/filelog`: a line is composed from
   the *least* converged iteration of a timepoint, and 1325 of 9114 lines belong to
   timepoints the solver went on to reject. With an accept callback the line is
   buffered and flushed when the point is accepted: the converged values, once, and
   never for a step that was taken back.
2. **A model could read one record per timepoint.** 9.5's file reads advance the
   file on every evaluation, so `sim_regression/vecsrc` has to make its reads
   idempotent by rewinding (three of four records were swallowed before t = 0
   without that). A read in an accept callback happens exactly once per point.
3. **Retained state would not need `$abstime`.** Everything this tree keeps across
   timesteps -- `@(cross)` latches, `timer`'s next event, `transition`'s queue,
   `absdelay`'s history in the in-model realization -- commits by noticing that
   `$abstime` has moved, which is a proxy for "the last point was accepted". It
   works, and it costs a retained slot and a comparison per model; an accept
   callback is what it is a proxy for.

None of the three is a correctness problem today, which is why this half is last.

## What this does not propose

**A way to reject a timepoint.** A model cannot ask for the step to be taken again,
and nothing here changes that. `$bound_step` and a requested instant are both
forward-looking; an event that is discovered late stays discovered late.

**A say in the solution itself.** The protocol is about *when* the simulator
solves, not about what it solves. `$limit` (9.17.3) already has its own channel.

**A version bump.** The two implemented fields extend the 0.4 tail, which
`OSDI_DESCRIPTOR_SIZE` makes safe, and this tree's `absdelay` fields set that
precedent. For an upstream OSDI release the three fields together are the natural
contents of an 0.5, with the accept callback's `NULL` and the offsets' `UINT32_MAX`
as the "not used" encodings.

## Checking an implementation

* `openvaf/test_data/osdi/timepoints.va` plus `test_timepoints` in
  `openvaf/openvaf/tests/integration.rs`: the mock simulator reads both slots and
  the test pins what is in them, timepoint by timepoint -- the instant asked for
  moves to the next event once the current one is consumed, and the announcement is
  about one point rather than about the state it left behind.
* `sim_regression/timepoints`: the two runs above, which also assert that a
  simulator which ignores the protocol produces the same waveform either way.
* `patches/ngspice-osdi-timepoints.patch` is the ngspice side, on top of
  `patches/ngspice-absdelay-history.patch` (which is where the per-entry descriptor
  size comes from). `OSDI_TP_TRACE=1` prints each announcement as it arrives, which
  is how the first end-to-end check was made.
