# `mir_reader` — MIR text format parser

**Location:** `openvaf/mir_reader/`
**Role:** Parses the human-readable MIR text format into a `mir::Function`.
The text format is Cranelift's `.clif` format, adapted for OpenVAF's MIR.
`mir_reader` is the companion to `Function::print` (in the `mir` crate); the
two together support round-trip testing of MIR transformations.

Cross-links: [mir INTERNALS](../mir/INTERNALS.md) ·
[mir_interpret INTERNALS](../mir_interpret/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate relationships

```
mir_reader   (openvaf/mir_reader/)
  └─► mir_autodiff  (tests: parse MIR text, run AD, interpret result)
```

The crate depends on `mir`, `bforest` (for `Map` used in phi node parsing),
and `lasso` (for string interning). It is only used as a `[dev-dependency]` in
`mir_autodiff`.

---

## The MIR text format

A function in the text format looks like this (taken from the roundtrip test):

```
function %bar(v4, v8, v9, v10) {
    v5 = iconst 42
    v6 = iconst 23
block0:
    v7 = iadd v5, v8
    v11 = iadd v6, v9
    v12 = ilt v8, v10
    br v12, block1, block2

block1:
    v13 = isub v7, v10
    jmp block3

block2:
    jmp block3

block3:
    v14 = phi [v13, block1], [v11, block2]
}
```

The format has four sections:

1. **Header**: `function %name(v0, v1, …)` — function name and parameter
   value numbers.
2. **Preamble**: constant definitions (`fconst`, `iconst`, `sconst`) and
   function signature declarations (`fn0 = const fn %name(1) -> 1`), before
   the first block.
3. **Basic blocks**: each introduced by `blockN:` followed by zero or more
   instructions.
4. **Closing `}`**.

### Token reference

| Text | Token | Examples |
|------|-------|---------|
| `vN` | `Value(Value)` | `v0`, `v42` |
| `blockN` | `Block(Block)` | `block0`, `block3` |
| `fnN` | `FuncRef(u32)` | `fn0`, `fn2` |
| `%name` | `Name(&str)` | `%bar`, `%ddx_v10` |
| `"…"` | `String(&str)` | `"hello"` |
| `0x…` or decimal | `Integer(&str)` | `42`, `0xff` |
| float literal | `Float(&str)` | `0x1.8p+1`, `NaN`, `Inf` |
| `#89AF` | `HexSequence(&str)` | |
| `@00c7` | `SourceLoc(&str)` | |
| `;` to end of line | `Comment(&str)` | |
| identifiers | `Identifier(&str)` | `iadd`, `fconst`, `loop` |

Whitespace (spaces, tabs, `\n`, `\r`) is skipped by the lexer. Comments
(`; …`) are consumed but not stored.

---

## Crate structure

```
mir_reader/src/
  lib.rs        — public API: parse_function, parse_functions, ParseError, LexError
  error.rs      — Location, ParseError, ParseResult, err! macro
  lexer.rs      — Lexer, Token, split_entity_name
  lexer/tests.rs
  parser.rs     — Parser, Context, VariableArgs
  parser/tests.rs
```

---

## `error.rs` — error types

```rust
pub struct Location { pub line_number: usize }

pub struct ParseError {
    pub location:   Location,
    pub message:    String,
    pub is_warning: bool,
}

pub type ParseResult<T> = Result<T, ParseError>;
```

`Location` is just a line number — no column, no file path. `line_number == 0`
means the error originated from a command-line argument rather than source text.

The `err!` macro constructs a `ParseError` at the current `loc`:

```rust
err!(self.loc, "expected '{' before function body")
err!(self.loc, "expected {} result values, {} given", num_results, results.len())
```

---

## `lexer.rs` — `Lexer`

```rust
pub struct Lexer<'a> {
    source:      &'a str,
    chars:       CharIndices<'a>,
    lookahead:   Option<char>,
    pos:         usize,
    line_number: usize,
}
```

The lexer keeps one character of lookahead and advances character-by-character
via `next_ch`. It tracks `line_number` by counting `'\n'` characters. All
`Token` variants that carry string data hold `&'a str` slices directly into
`source` — no copies.

### Key lexing rules

- **Numbers**: `scan_number` handles `+`/`-` signs, hex prefixes (`0x`),
  floats (`.` or `p` exponent), `NaN[:payload]`, `Inf`, and `sNaN`. A `-`
  followed by a non-numeric character is emitted as `Token::Minus` rather than
  a number prefix.
- **Entity names**: `scan_word` reads an alphanumeric word then calls
  `split_entity_name` to check if it matches `v{N}`, `block{N}`, or `fn{N}`.
  If so, the corresponding typed token is returned; otherwise `Token::Identifier`.
- **`split_entity_name`**: splits a word at the boundary between a letter
  prefix and a decimal suffix. Leading zeros in the suffix are rejected
  (e.g. `block007` is not a valid entity name).
- **`%names`**: `scan_name` reads alphanumeric + `_` characters after `%` and
  returns the interior as `Token::Name`.
- **Strings**: `scan_string` reads until the closing `"`, handling `\0`,
  `\n`, `\r`, `\t`, `\\`, `\"` escape sequences.
- **`->` arrow**: detected with `looking_at("->")` before trying to scan `-`
  as a number.
- **`@N`** (source location) and **`#N`** (hex sequence): each scanned until a
  non-hex digit is found.

---

## `parser.rs` — `Parser` and `Context`

### `Parser<'a>`

```rust
pub struct Parser<'a> {
    lex:       Lexer<'a>,
    lex_error: Option<LexError>,
    lookahead: Option<Token<'a>>,
    loc:       Location,
    interner:  Rodeo,       // lasso string interner for sconst values
}
```

The parser is a single-token lookahead recursive-descent parser over the
lexer's token stream. `token()` lazily fills `lookahead` by calling
`lex.next()`, skipping `Comment` tokens implicitly (they are consumed but not
stored). `consume()` takes the lookahead. `match_token` / `optional` are the
standard LL(1) primitives.

`interner` is a `lasso::Rodeo` that interns string constant values (`sconst`).
All interned `Spur` handles refer to this rodeo; the caller receives it back
alongside the `Function` to allow string lookups after parsing.

### `Context`

```rust
struct Context { function: Function }
```

`Context` wraps the `Function` being built and provides two allocating helpers:

- `add_sig(sig, data)` — grows `dfg.signatures` up to the given `FuncRef`
  index (filling gaps with default signatures) and then writes `data` at that
  slot. This allows preamble declarations to appear in any order.
- `add_block(block)` — grows `layout` up to the given block number and
  appends it. Blocks are always processed in declaration order.

### `match_value`

```rust
fn match_value(&mut self, ctx: &mut Context, err_msg: &str) -> ParseResult<Value>
```

When a `Token::Value(v)` is consumed, `ctx.function.dfg` is grown
(`make_invalid_value()` is called repeatedly) until `num_values > v`. This
allows forward references: a value like `v14` can appear in a phi-node operand
before its defining instruction has been parsed. The defining instruction's
`make_inst_results_reusing` call later patches the `Invalid` slot to the real
`ValueDef`.

### Parsing pipeline

```
parse_functions / parse_function
  └── parse_function
        ├── match_identifier("function")
        ├── parse_external_name        → function name
        ├── parse_func_params          → v0, v1, … registered as Param values
        ├── match_token(LBrace)
        ├── parse_preamble             → constant defs + signature decls
        ├── parse_function_body        → basic blocks + instructions
        └── match_token(RBrace)
```

**Preamble** (`parse_preamble`): loops on:
- `FuncRef` token → `parse_signature_decl` → `ctx.add_sig`
- `Value = fconst|iconst|sconst …` → constant definition via `dfg.values.fconst_at` etc.
- Anything else → exits the preamble loop.

**Function body** (`parse_function_body`): loops calling `parse_basic_block`
until a `RBrace` is seen.

**Basic block** (`parse_basic_block`): consumes `blockN:`, calls `ctx.add_block`,
then loops calling `parse_instruction` while the lookahead is a `Value`,
`Identifier`, `LBracket`, or `SourceLoc` token.

**Instruction** (`parse_instruction` + `parse_inst_operands`): reads the
opcode identifier via `text.parse::<Opcode>()` (using the `FromStr` impl
generated by `sourcegen`), then dispatches on `opcode.format()`:

| Format | Text syntax | Example |
|--------|-------------|---------|
| `Unary` | `opcode v` | `fneg v3` |
| `Binary` | `opcode v, v` | `fadd v1, v2` |
| `Jump` | `jmp blockN` | `jmp block3` |
| `Branch` | `br v, blockN[loop]?, blockN` | `br v12, block1[loop], block2` |
| `Call` | `opcode fnN(v, …)` | `call fn0(v1, v2)` |
| `PhiNode` | `phi [v, blockN], …` | `phi [v13, block1], [v11, block2]` |
| `Exit` | `exit` | `exit` |

After building `InstructionData`, the parser calls:
- `dfg.make_inst(inst_data)` — allocates the `Inst`.
- `dfg.make_inst_results_reusing(inst, results)` — creates result `Value`s,
  reusing the pre-allocated `Invalid` slots from earlier `match_value` calls.
- `layout.append_inst_to_bb(inst, block)` — places the instruction in the CFG.

The optional `@hexnum` source location prefix is parsed by `optional_srcloc`
and stored in `func.srclocs[inst]`.

### Phi node parsing

Phi operands are pairs `[value, block]`:

```
phi [v13, block1], [v11, block2]
```

For each pair, the value is pushed onto a `ValueList` (getting a position
index), and the block is mapped to that position in a `bforest::Map<Block, u32>`
(the `blocks` field of `PhiNode`). This is the same representation used by the
MIR at runtime.

### `VariableArgs`

A thin `Vec<Value>` newtype with a helper:

```rust
pub fn into_value_list(self, fixed: &[Value], pool: &mut ValueListPool) -> ValueList
```

Used by `Call` parsing to convert the argument list into a `ValueList` in the
pool, prepending any fixed arguments.

### Signature syntax

```
fn0 = const fn %name(2) -> 1
fn1 = fn %callback(0) -> 0
```

- `const` prefix → `has_sideeffects = false`
- `fn %name` → function name
- `(N)` → parameter count
- `-> N` → return count (optional; 0 if absent)

---

## Public API

```rust
pub fn parse_function(text: &str)  -> ParseResult<(Function, Rodeo)>
pub fn parse_functions(text: &str) -> ParseResult<(Vec<Function>, Rodeo)>
```

Both return the `Rodeo` string interner alongside the function(s) so that
callers can look up `sconst` string values by their `Spur` handle. The rodeo
is created fresh per `Parser` and not shared across multiple `parse_function`
calls in a single session.

---

## Worked example: `mir_autodiff` test

The `check_num` test helper in `mir_autodiff/src/builder/tests.rs` uses
`mir_reader` to set up a fixture function, then verifies both the textual and
numerical output of auto-differentiation:

```rust
let src = r##"
    function %bar(v10, v11) {
        fn0 = const fn %ddx_v10(1) -> 1
        fn1 = const fn %ddx_v11(1) -> 1
    block0:
        v0 = fmul v10, v11
        v1 = call fn0(v0)
        exit
    }
"##;

let (mut func, _rodeo) = parse_function(src).unwrap();
// … run auto_diff, then interpret and check v100 ≈ expected derivative
```

`parse_function` converts the text into a live `Function` complete with
`DataFlowGraph`, `Layout`, signatures, and constant values. The `_rodeo` is
discarded here because there are no `sconst` values in the fixture. After
`auto_diff` transforms the function, `Function::print` serialises it back to
text (using the same `Rodeo`) for the `expect_test` snapshot comparison.

---

## Key design decisions

**Cranelift `.clif` format as the baseline.** Reusing the established Cranelift
IR text format means the MIR is human-readable in a familiar style and the
parser design is well-understood. The deviations from `.clif` are minor
(phi node syntax, no type annotations) and documented by the grammar comments
in `parser.rs`.

**`&'a str` slices, not `String` copies.** All `Token` variants that carry
text (`Name`, `Identifier`, `Float`, `Integer`, `String`, `HexSequence`,
`SourceLoc`, `Comment`) hold references into the original source string. No
heap allocation occurs during lexing. The parser interns only `sconst` string
values (into the `Rodeo`) since those need to outlive the source text.

**Forward references via `make_invalid_value`.** Rather than requiring a
two-pass parse or topological ordering of values, `match_value` eagerly
allocates `Invalid` placeholder slots up to the referenced index. The defining
instruction then overwrites the slot via `make_inst_results_reusing`. This
keeps the parser single-pass at the cost of allocating a few extra `Invalid`
entries for forward references.

**`lasso::Rodeo` returned to the caller.** The interner is not hidden inside
the parser; it is handed back alongside the `Function` so that callers can
resolve `Spur` handles to their underlying strings. This avoids a separate
global interner and keeps `mir_reader` stateless between calls.

**`parse_functions` for multi-function files.** The `mir_autodiff` tests
occasionally embed multiple functions in a single source string. `parse_functions`
loops `parse_function` until EOF, sharing one `Parser` (and one `Rodeo`) across
all functions in the file.
