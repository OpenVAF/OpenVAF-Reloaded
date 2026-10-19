# ngspice: an OSDI device with an extra internal node aborts the process when its operating point fails

Filed against ngspice (not OpenVAF). Found while transcribing a third-party
PLL's loop filter to Verilog-A: an ill-posed deck, which should have produced a
singular-matrix failure, killed the process instead.

Affects: ngspice-46, reproduced on two independently built binaries here,
including an unpatched one.

## Summary

Given a deck whose operating point cannot be solved, ngspice reports `singular
matrix`, tries dynamic gmin stepping, true gmin stepping and source stepping,
falls back to a transient operating point, and then aborts:

```
Warning: singular matrix:  check node n1#mid
Note: Starting dynamic gmin stepping
...
Note: Transient op finished successfully
double free or corruption
Aborted                                   (exit 134)
```

Exit 134 is SIGABRT, raised by glibc's heap checker, so memory has been
corrupted by then. The same ill-posed topology built from ngspice's own `R` and
`C` primitives reports its singular matrix and exits 0.

An ill-posed circuit is the deck's fault. Aborting rather than reporting the
failure is not, and a heap error means the damage is not confined to the
analysis that failed.

## Reproducing

`gnd.va`, two modules, the same circuit twice. Both are a 300 ohm resistor to an
internal node and 1 nF from there to the reference. They differ only in whether
the reference is a net of the module's own:

```verilog
`include "disciplines.vams"

module iso_a(p);
    inout p; electrical p;
    electrical mid;
    analog begin
        I(p, mid) <+ V(p, mid) / 300.0;
        I(mid)    <+ ddt(1.0e-9 * V(mid));
    end
endmodule

module iso_b(p);
    inout p; electrical p;
    electrical mid;
    electrical gnd;
    ground gnd;
    analog begin
        I(p, mid)   <+ V(p, mid) / 300.0;
        I(mid, gnd) <+ ddt(1.0e-9 * V(mid, gnd));
    end
endmodule
```

`gnd.cir`, with `iso_a` or `iso_b`. The current source is what makes it
ill-posed: with the capacitor open at dc nothing in the device reaches ground,
so the operating point is undefined.

```
* osdi ground node
.model m_iso iso_b
N1 p m_iso
Idrv 0 p DC 0
.control
pre_osdi gnd.osdi
op
quit
.endc
.end
```

```sh
openvaf-r gnd.va -o gnd.osdi
ngspice -b gnd.cir
```

| module | result |
| --- | --- |
| `iso_a` | exit 0, 6 `singular matrix` warnings, no crash |
| `iso_b` | **exit 134**, `double free or corruption` |

Replace `Idrv 0 p DC 0` with `Vdrv p 0 DC 1` and both exit 0 with no warnings:
it takes an operating point that fails.

`.options gminsteps=0`, `.options noopiter`, `.options itl6=0` and
`.options gmin=1e-9` each still abort, so it is not specific to the gmin or
source-stepping fallbacks. The glibc message alternates between `double free or
corruption` and `free(): invalid size` depending on the deck, which is the usual
signature of a write past the end of a block rather than of a genuine double
free.

No debugger is installed in the environment this was found in, so there is no
backtrace here. The repro above is three files and a second of runtime under
one.

## What the two modules differ by, concretely

A net declared `ground` inside a module is not collapsed onto the simulator's
reference by OpenVAF-Reloaded; it becomes an internal node like any other,
pinned at 0 V. `print all` after the operating point, on the well-posed deck:

```
  iso_a                       iso_b
  n1#mid = 1.000000e+00       n1#gnd = 0.000000e+00
  p      = 1.000000e+00       n1#mid = 1.000000e+00
                              p      = 1.000000e+00
```

So `iso_b` presents one more internal node than `iso_a`, and that is the whole
difference between exit 0 and exit 134. A wrong node or matrix size somewhere on
the failure path is the obvious suspect, since the success path is fine with the
same device.

## A note on the OpenVAF side

Whether a `ground` net should become a node at all is a separate question, and
one for this tree rather than for ngspice: a ground declaration names the
discipline's global reference, which has a potential of zero by definition and
no KCL row of its own, so collapsing it would be the expected lowering. Doing
that here would remove the trigger, and it would not fix the ngspice defect,
which any device with an unused internal node can presumably still reach.

Found from the other direction, by a generator: `spice2va` emitted
`electrical gnd; ground gnd;` for every subcircuit that mentioned node 0, which
put the extra node in every model it wrote. It now leaves the reference
implicit, and the formerly aborting case reports its singular matrix and
completes. That write-up is `work/ground_node.md` in the spice2va tree.
