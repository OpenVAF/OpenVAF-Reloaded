"""Measure ps_out.txt against the paramsets in paramset.va and the cards in the netlist.

VAMS-2023 6.4: a paramset is a model card the library carries. `paramset.va` has one
module and two paramsets over it, so what the netlist instantiates is three names
for one compiled module, and what each device's parameters are is decided in three
places at once: the paramset's statements, the paramset's own parameters as the card
sets them, and the module's declared defaults for everything the paramset left
alone.

So this reads all three out of the source rather than restating them:

  * the paramset blocks out of `paramset.va`, both their statements and the
    declared defaults of their own parameters;
  * the `.model` cards out of `paramset.cir`;
  * the module's own parameter defaults out of `paramset.va`, for the parameters
    no paramset sets.

What that pins down:

  * the module and the paramset over it with the same shape are the same device:
    the first two devices are carded to agree, and their waveforms have to agree
    exactly rather than closely;
  * each paramset's statements reach the module, since the resistance of each
    device is recomputed from them and from the shape on its card;
  * a module parameter the paramset is silent about keeps its declared default,
    which `rprec` leaves `nf` and `dw` to;
  * the dependent default survives the substitution: `r0` is never set by either
    paramset, and its default reads every parameter declared before it, so a
    substitution in the wrong order would give the wrong resistance.

The reference is recomputed arithmetic, not a stored waveform: the device is
algebraic, so its current at the voltage the waveform carries is a closed form.

Exits non-zero on failure.
"""

import re
import sys

RS = 1000.0  # the sense resistor the netlist puts in series with each device

# The currents are of order 10 mA and `wrdata` writes nine significant digits, so
# a voltage this recomputes from is already rounded to about 10 nA's worth. A
# paramset statement that did not arrive would miss by a per cent of 10 mA.
TOL_I = 1e-7

# `parameter <type> <name> = <value>`, with 3.4.2's optional range after it.
DECL = r"\bparameter\s+(?:real|integer)\s+(\w+)\s*=\s*([^;]+?)(?:\s+from\b[^;]*)?;"

SUFFIX = {"t": 1e12, "g": 1e9, "meg": 1e6, "k": 1e3, "m": 1e-3,
          "u": 1e-6, "n": 1e-9, "p": 1e-12, "f": 1e-15, "a": 1e-18}


def number(text):
    """A Verilog-A or SPICE number, with the scale factor either of them allows."""
    m = re.match(r"^([-+]?[0-9.]+(?:[eE][-+]?[0-9]+)?)([a-zA-Z]*)$", text.strip())
    if not m:
        raise ValueError("not a number: %r" % text)
    val = float(m.group(1))
    suffix = m.group(2).lower()
    if suffix:
        # `1meg` before `1m`, and a trailing unit (`1kOhm`) is not a scale.
        for name in ("meg", "t", "g", "k", "m", "u", "n", "p", "f", "a"):
            if suffix.startswith(name):
                val *= SUFFIX[name]
                break
        else:
            raise ValueError("unknown scale factor: %r" % text)
    return val


def read_source(path):
    """The module's parameter defaults, and one entry per paramset.

    Returns (module_defaults, {paramset: {"params": {...}, "stmts": {...}}}).
    Statement values are kept as written: a number, or the name of one of the
    paramset's own parameters.
    """
    text = open(path).read()
    # Strip comments so a commented-out statement is not read as one.
    text = re.sub(r"//[^\n]*", "", text)

    module_defaults = {}
    paramsets = {}
    for block in re.finditer(
        r"\bmodule\s+(\w+)\s*\([^)]*\)\s*;(.*?)\bendmodule", text, re.S
    ):
        for pm in re.finditer(DECL, block.group(2)):
            module_defaults[pm.group(1)] = pm.group(2).strip()

    for block in re.finditer(
        r"\bparamset\s+(\w+)\s+(\w+)\s*;(.*?)\bendparamset", text, re.S
    ):
        name, target, body = block.group(1), block.group(2), block.group(3)
        params = {}
        for pm in re.finditer(DECL, body):
            params[pm.group(1)] = number(pm.group(2))
        stmts = {}
        for sm in re.finditer(r"\.(\w+)\s*=\s*([^;]+);", body):
            stmts[sm.group(1)] = sm.group(2).strip()
        paramsets[name.lower()] = {"target": target, "params": params, "stmts": stmts}
    return module_defaults, paramsets


def model_cards(path):
    """{card: (module_or_paramset, {parameter: value})} for every `.model` line."""
    cards = {}
    for line in open(path):
        line = line.strip()
        if line.startswith("*"):
            continue
        m = re.match(r"\.model\s+(\S+)\s+(\S+)\s*(.*)", line, re.I)
        if not m:
            continue
        name, kind, rest = m.groups()
        vals = {
            pm.group(1).lower(): number(pm.group(2))
            for pm in re.finditer(r"(\w+)\s*=\s*(\S+)", rest)
        }
        cards[name.lower()] = (kind.lower(), vals)
    return cards


def resolve(card, module_defaults, paramsets):
    """The module parameters one `.model` card amounts to.

    A card on the module sets them directly. A card on a paramset sets the
    paramset's parameters, and the paramset's statements set the module's; what no
    statement sets keeps the module's declared default, `r0` included.
    """
    kind, vals = card
    params = {name: number(text) for name, text in module_defaults.items()
              if name != "r0"}

    if kind not in paramsets:
        params.update(vals)
    else:
        set_ = paramsets[kind]
        own = dict(set_["params"])
        for name, val in vals.items():
            if name not in own:
                sys.exit("card sets '%s', which paramset '%s' does not declare" % (name, kind))
            own[name] = val
        for target, text in set_["stmts"].items():
            params[target] = own[text] if text in own else number(text)

    # The module's own default for `r0`, which neither paramset sets.
    params["r0"] = params["rsh"] * params["l"] / ((params["w"] - params["dw"]) * params["nf"])
    return params


module_defaults, paramsets = read_source("paramset.va")
cards = model_cards("paramset.cir")

# The devices, in the order `wrdata` wrote them, and the card each one is on.
DEVICES = [("a", "mfull"), ("b", "mpoly"), ("c", "mnarrow"), ("d", "mprec"), ("e", "mdef")]
resolved = [resolve(cards[card], module_defaults, paramsets) for _, card in DEVICES]

rows = []
for line in open("ps_out.txt"):
    parts = line.split()
    if len(parts) < 12:
        continue
    try:
        v = [float(x) for x in parts]
    except ValueError:
        continue
    # wrdata writes a time column per variable: t v(s), t v(a), ... t v(e)
    rows.append((v[0], v[1], v[3], v[5], v[7], v[9], v[11]))

if len(rows) < 500:
    sys.exit("ps_out.txt has %d rows, expected a transient run" % len(rows))

# The module and the paramset over it, carded to be the same device. Not "close":
# the same compiled module with the same parameters is the same arithmetic.
same = sum(1 for row in rows if row[2] == row[3])
if same != len(rows):
    off = max(abs(row[2] - row[3]) for row in rows)
    sys.exit(
        "the module and the paramset over it differ on %d of %d rows, worst %.3e V"
        % (len(rows) - same, len(rows), off)
    )

# Every device against the resistance its card, its paramset and the module's
# defaults amount to.
worst = []
for i, (_, card) in enumerate(DEVICES):
    p = resolved[i]
    err = 0.0
    for row in rows:
        v = row[1] - row[2 + i]
        i_meas = row[2 + i] / RS
        i_want = v / (p["r0"] * (1.0 + p["vc1"] * v * v))
        err = max(err, abs(i_meas - i_want))
    worst.append(err)

print(
    "a paramset is a model card: the module and the paramset over it agree on "
    "%d of %d rows exactly" % (same, len(rows))
)
for (_, card), p, err in zip(DEVICES, resolved, worst):
    print(
        "  %-8s r0 = %8.3f Ohm, vc1 = %.3g /V^2, nf = %g: worst current error %.3e A"
        % (card, p["r0"], p["vc1"], p["nf"], err)
    )

bad = [
    "%s: off by %.3e A" % (card, err)
    for (_, card), err in zip(DEVICES, worst)
    if err > TOL_I
]
if bad:
    sys.exit("; ".join(bad))
