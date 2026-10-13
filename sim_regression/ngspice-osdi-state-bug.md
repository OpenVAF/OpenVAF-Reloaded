# ngspice: OSDI devices read a stale state buffer on the first iteration of every timestep

Filed against ngspice (not OpenVAF). Written up here because it was found while
implementing VAMS-2023 §5.10.3 event scheduling; OpenVAF-Reloaded no longer
depends on the answer (retained state moved into the instance data), but the bug
still degrades `$limit` for every OSDI device.

Affects: ngspice-46, `src/osdi/`.

## Summary

`OSDIload` hands the model the same buffer for both the previous and the next
state, and that buffer is a member of a ring that the transient integrator
rotates once per accepted timestep. So on the first Newton iteration of each
step, an OSDI model reads back the slot it wrote several steps earlier instead of
the most recent value.

## Detail

`src/osdi/osdiload.c:141`

```c
OsdiSimInfo sim_info = {
    ...
    .prev_state = ckt->CKTstates[0],
    .next_state = ckt->CKTstates[0],
```

Both point at `CKTstates[0]`. Within one timestep that aliasing is what limiting
wants: a model reads the value the previous iteration wrote. Across timesteps it
breaks, because `src/spicelib/analysis/dctran.c:659` rotates the ring:

```c
temp = ckt->CKTstates[ckt->CKTmaxOrder+1];
for (i = ckt->CKTmaxOrder; i >= 0; i--)
    ckt->CKTstates[i+1] = ckt->CKTstates[i];
ckt->CKTstates[0] = temp;
```

`cktsetup.c:192` allocates `MAX(2, CKTmaxOrder) + 2` buffers, so with the default
`maxord = 2` the ring is 4 deep and the `CKTstates[0]` a model sees at the start
of a new step is the buffer it last wrote **four accepted steps** ago. Measured
lag: 4 steps.

Nothing copies `CKTstates[1]` into `CKTstates[0]` for OSDI devices. Built-in
SPICE devices do not need it because each one rewrites all of its state on every
load call; an OSDI model's `$limit` state is the same shape, but it is *read
before* it is written, which is where the staleness shows.

## Impact

`$limit` only steers the Newton trajectory, never the converged solution, so this
does not produce wrong DC or transient answers. It does mean every OSDI device
begins each timestep limiting against a value from four steps back, which is a
worse initial guess than the intended one — a convergence-quality regression
relative to built-in devices, silent and hard to attribute.

## Reproducing

Any OSDI model that reads `prev_state` will show it, but the sharpest probe is a
model that stores a known monotonic sequence (e.g. `$abstime`) into a state slot
and contributes the value read back from `prev_state` to an output node: the
output lags the input by `MAX(2, maxord) + 2` accepted steps rather than one.

## Suggested fix

Copy each instance's last accepted state forward on the first iteration of a new
timestep, so `CKTstates[0]` holds the previous accepted values before that
iteration overwrites them. `MODEINITPRED` marks exactly that iteration (set at
`dctran.c:710`, just after the rotation at `:659`), and `OSDIload` already walks
models and instances:

```c
if ((ckt->CKTmode & MODEINITPRED) && descr->num_states != 0) {
  for (GENmodel *m = in_model; m; m = m->GENnextModel)
    for (GENinstance *i = m->GENinstances; i; i = i->GENnextInstance)
      memcpy(ckt->CKTstates[0] + i->GENstate,
             ckt->CKTstates[1] + i->GENstate,
             descr->num_states * sizeof(double));
}
```

(The OSDI state slots occupy `[GENstate, GENstate + num_states)`; the reactive
residual slots that follow them are written every load and need no copy.)

Re-pointing `prev_state` at `CKTstates[1]` instead would also yield the previous
accepted value, but it changes `$limit` from previous-iteration to
previous-timestep semantics, which is not what limiting wants. The copy keeps
iteration semantics within a step and corrects only the first iteration.

## Related, but a separate matter

OSDI 0.4 has no state facility with *accepted-timestep* semantics at all, and one
pointer pair cannot provide both meanings. A model that needs a value from the
previous accepted step (the latch behind `@(cross)`, VAMS-2023 §5.10.3.1) cannot
use this array however ngspice behaves. The precedent for how that should be
added is `absdelay`: a descriptor-level protocol (`OsdiAbsDelayInfo`) where the
simulator owns the history. Until then OpenVAF-Reloaded keeps retained values in
its own instance data.
