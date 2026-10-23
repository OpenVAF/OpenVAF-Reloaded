# `hir_ty` — Internals

Type inference and semantic resolution for OpenVAF's HIR.

---

## 1. Purpose and Position

`hir_ty` is the layer that turns `hir_def`'s unresolved `Body` (where every path is still a sequence of name strings) into a fully typed, resolved representation. It sits above `hir_def` and below `hir_lower`, which consumes its results to emit MIR.

Concretely, `hir_ty` adds three things that `hir_def` leaves unresolved:

1. **Type assignment** — every `ExprId` in a `Body` gets a `Ty`, the extended type that distinguishes between values, node references, branch references, nature references, and so on.
2. **Call resolution** — every `Expr::Call` gets a `ResolvedFun` (a built-in, a user function, a system parameter) and a `Signature` (the overload that matched).
3. **Assignment destination analysis** — every `Stmt::Assignment` gets an `AssignDst`, distinguishing variable writes from flow/potential contributions.

The crate also resolves the nature and discipline hierarchy (`NatureTy`, `DisciplineTy`, `BranchTy`) and performs semantic validation beyond what the parser can catch.

---

## 2. Module Map

| File | Role |
|---|---|
| `lib.rs` | Re-exports; `BranchTy`, `DisciplineTy`, `NatureTy` |
| `db.rs` | `HirTyDB` Salsa query group; `Alias` enum; transparent queries |
| `lower.rs` | `NatureTy`, `DisciplineTy`, `BranchTy`, `DisciplineAccess`, `BranchKind` |
| `types.rs` | `Ty`, `TyRequirement`, `TyEquivalence`, `Signature`, `SignatureData`, `BuiltinInfo` |
| `inference.rs` | `InferenceResult`, `Ctx`, inference engine, `ResolvedFun`, `AssignDst`, `BranchWrite` |
| `inference/fmt_parser.rs` | Format-string argument parser for `$display` family |
| `builtin.rs` | `bultins!` macro; `BuiltinInfo` factories; nature-access and ddx signature constants |
| `builtin/generated.rs` | Code-generated `BUILTIN_INFO` table mapping `BuiltIn` discriminants to `BuiltinInfo` |
| `diagnostics.rs` | `TypeMismatch`, `SignatureMismatch`, `ArrayTypeMismatch`, diagnostic rendering |
| `validation/body.rs` | `BodyValidationDiagnostic`; context-sensitive checks (illegal contribute, port misuse, etc.) |
| `validation/types.rs` | Type-level validation helpers |

---

## 3. `HirTyDB` Query Group

```rust
#[salsa::query_group(HirTyDatabase)]
pub trait HirTyDB: HirDefDB + Upcast<dyn HirDefDB> { … }
```

`HirTyDB` extends `HirDefDB` with the following queries:

| Query | Input | Output | Notes |
|---|---|---|---|
| `nature_info` | `NatureId` | `Arc<NatureTy>` | Cycle-recovered (circular parent chains) |
| `discipline_info` | `DisciplineId` | `Arc<DisciplineTy>` | |
| `branch_info` | `BranchId` | `Option<Arc<BranchTy>>` | `None` if paths can't be resolved |
| `inference_result` | `DefWithBodyId` | `Arc<InferenceResult>` | Main type-inference result |
| `nature_attr_ty` | `NatureAttrId` | `Option<Type>` | Cycle-recovered |
| `resolve_alias` | `AliasParamId` | `Option<Alias>` | Cycle-recovered |
| `node_discipline` | `NodeId` | `Option<DisciplineId>` | Transparent |
| `param_ty` | `ParamId` | `Type` | Transparent; infers from default if no explicit type |
| `known_limit_functions` | — | `Option<Arc<[LimitSignature]>>` | **Input** query; set by the CLI |

**Input queries** have no computed implementation — their value is injected by the caller (the `osdi` driver sets `known_limit_functions` from the simulator's list of `$limit`-aware functions).

**Cycle recovery.** Verilog-A allows circular nature parent chains in principle; Salsa would loop forever. `nature_info_recover` breaks the cycle by calling `NatureTy::obtain(db, nature, false)` — the `false` skips parent resolution. Similarly, `resolve_alias_recover` returns `Some(Alias::Cycel)` (note: a typo in the source that is preserved here) to signal a circular alias.

**`Alias`** is the result of resolving an `aliasparam`:

```rust
pub enum Alias {
    Cycel,            // circular alias chain detected
    Param(ParamId),
    ParamSysFun(ParamSysFun),
}
```

---

## 4. `NatureTy` and `DisciplineTy`

### `NatureTy`

```rust
pub struct NatureTy {
    pub ddt_nature: NatureId,   // nature of d/dt of this nature (self if none declared)
    pub idt_nature: NatureId,   // nature of ∫dt of this nature (self if none declared)
    pub parent: Option<NatureId>,
    pub base_nature: NatureId,  // root of the inheritance chain
    pub units: Option<String>,
}
```

`NatureTy::obtain(db, nature, resolve_parent)` builds the struct by:

1. Looking up the nature's `ddt_nature` and `idt_nature` from `NatureData`, resolving the `NatureRef` against the root `DefMap`.
2. If a parent is declared and `resolve_parent` is true, recursively calling `db.nature_info(parent)` and inheriting unset fields from it.
3. Setting `base_nature` to the parent's `base_nature`, or `nature` itself if no parent.

**`NatureTy::compatible(db, n1, n2)`** — two natures are compatible if they share the same `units` string. This is the predicate used to determine whether an access function (e.g., `V`) applies to a node's discipline.

**`NatureTy::related(db, n1, n2)`** — two natures are related if they share the same `base_nature`. Used for checking that `ddt`/`idt` pairs are coherent.

**`NatureTy::lookup_attr(db, nature, name)`** — walks up the parent chain looking for a nature attribute by name. Returns `NatureAttrId` or `PathResolveError::NotFoundIn`.

### `DisciplineTy`

```rust
pub struct DisciplineTy {
    pub flow: Option<NatureId>,
    pub potential: Option<NatureId>,
}
```

`discipline_info_query` resolves the `NatureRef` names in `DisciplineData` to actual `NatureId`s by calling `lookup_nature` against the root `DefMap`.

**`DisciplineTy::access(nature, db) -> Option<DisciplineAccess>`** — the key method for resolving an access call. It checks:

1. Is `nature` compatible with the discipline's `flow` nature? → `Some(DisciplineAccess::Flow)`
2. Is `nature` compatible with the discipline's `potential` nature? → `Some(DisciplineAccess::Potential)`
3. Neither → `None` (invalid access, diagnosed as `InvalidNatureAccess`)

**`DisciplineTy::compatible(other, db)`** — both `flow` natures must be compatible with each other AND both `potential` natures must be compatible. Used during `BranchKind::Nodes` discipline resolution.

---

## 5. `BranchTy`

```rust
pub struct BranchTy {
    pub discipline: DisciplineId,
    pub kind: BranchKind,
}

pub enum BranchKind {
    PortFlow(NodeId),
    NodeGnd(NodeId),
    Nodes(NodeId, NodeId),
}
```

`branch_info_query` resolves the `Path`-based `hir_def::BranchKind` into the `NodeId`-based `hir_ty::BranchKind`:

```
hir_def::BranchKind::PortFlow(Path)  →  BranchKind::PortFlow(NodeId)
hir_def::BranchKind::NodeGnd(Path)   →  BranchKind::NodeGnd(NodeId)
hir_def::BranchKind::Nodes(P1, P2)  →  BranchKind::Nodes(NodeId, NodeId)
hir_def::BranchKind::Missing         →  None
```

After resolving node IDs, the discipline is extracted via `BranchKind::discipline(db)`:

- `PortFlow(node)` / `NodeGnd(node)` — use `db.node_discipline(node)`.
- `Nodes(node1, node2)` — use `node1`'s discipline; if different from `node2`'s, check `DisciplineTy::compatible`. If incompatible, no discipline can be assigned and `branch_info` returns `None`.

---

## 6. `Ty` — the Extended Type

`Type` (from `hir_def`) represents only value types: `Real`, `Integer`, `Bool`, `String`, `Array`, `Void`, `Err`. `Ty` extends this to cover every kind of expression position that can appear in a Verilog-A body:

```rust
pub enum Ty {
    Val(Type),                                       // a computed value
    Node(NodeId),                                    // a net/port reference
    PortFlow(NodeId),                                // a <port> reference
    Nature(NatureId),                                // a nature reference
    Discipline(DisciplineId),                        // a discipline reference
    Var(Type, VarId),                               // a variable reference (lvalue)
    NatureAttr(Type, NatureAttrId),                 // a nature attribute reference
    FunctionVar { ty: Type, fun: FunctionId,         // function return var or arg
                  arg: Option<LocalFunctionArgId> },
    Param(Type, ParamId),                           // a parameter reference
    Literal(Type),                                  // an uncoerced literal
    InfLiteral,                                     // the keyword `inf`
    Branch(BranchId),                               // a branch reference
    Scope,                                          // a module/block name (not a value)
    BuiltInFunction,                                // resolved but not yet called
    UserFunction(FunctionId),                       // resolved but not yet called
}
```

`Ty::to_value() -> Option<Type>` extracts the scalar type from any `Ty` that carries a value: `Val`, `Var`, `NatureAttr`, `Param`, `Literal`, `FunctionVar`. `InfLiteral` promotes to `Real`. All other variants return `None`.

### `TyRequirement`

What an expression position *demands*:

```rust
pub enum TyRequirement {
    Val(Type),                  // a value of a specific type
    Condition,                  // anything assignable to Bool
    AnyVal,                     // any value type
    ArrayAnyLength { ty: Type },// array of any length with element type ty
    Node, PortFlow, Nature,     // reference kinds
    Var(Type), Param(Type),     // exact-type references (no conversion)
    AnyParam,                   // parameter of any type
    Branch,                     // branch reference
    Literal(Type),              // uncoerced literal of given type
    Function,                   // callable
}
```

`TyEquivalence` (internal) controls how `satisfies` compares types: `Exact`, `Semantic` (treats `Bool` ↔ `Integer` as equivalent), or `Conversion` (allows implicit widening, e.g., `Integer` → `Real`). The `expect` method in `Ctx` calls `satisfies_with_conversion` and records a cast when a type needs coercion.

### `Signature` and `SignatureData`

```rust
pub struct Signature(pub u32);   // index into a function's signatures array

pub struct SignatureData {
    pub args: Cow<'static, [TyRequirement]>,
    pub return_ty: Type,
}
```

Each built-in function has one or more overloaded `SignatureData`s (e.g., `abs` has `ABS_INT` and `ABS_REAL`). Overload resolution picks the best-matching `Signature` and records it in `InferenceResult::resolved_signatures`.

---

## 7. `InferenceResult`

```rust
pub struct InferenceResult {
    pub expr_types: ArenaMap<Expr, Ty>,
    pub resolved_calls: AHashMap<ExprId, ResolvedFun>,
    pub resolved_signatures: AHashMap<ExprId, Signature>,
    pub assignment_destination: AHashMap<StmtId, AssignDst>,
    pub casts: AHashMap<ExprId, Type>,
    pub diagnostics: Vec<InferenceDiagnostic>,
}
```

**`expr_types`** — maps every `ExprId` to its `Ty`. Initialised to `Ty::Val(Type::Err)` for all expressions before inference runs; filled in by `infere_expr`.

**`resolved_calls`** — maps call expressions (`Expr::Call`) to `ResolvedFun`:

```rust
pub enum ResolvedFun {
    User { func: FunctionId, limit: bool },   // user function; `limit` = called via $limit
    BuiltIn(BuiltIn),                          // built-in; includes `potential` and `flow`
    Param(ParamSysFun),                        // $mfactor etc.
    InvalidNatureAccess(NatureId),             // nature used on wrong discipline
}
```

**`resolved_signatures`** — maps call expressions to the `Signature` (overload index) that was selected. For nature access calls this is one of `NATURE_ACCESS_BRANCH`, `NATURE_ACCESS_NODES`, `NATURE_ACCESS_NODE_GND`, `NATURE_ACCESS_PORT_FLOW`.

**`assignment_destination`** — maps `StmtId`s of `Stmt::Assignment` to `AssignDst`:

```rust
pub enum AssignDst {
    Var(VarId),
    FunVar { fun: FunctionId, arg: Option<LocalFunctionArgId> },
    Flow(BranchWrite),
    Potential(BranchWrite),
}

pub enum BranchWrite {
    Named(BranchId),
    Unnamed { hi: NodeId, lo: Option<NodeId> },
}
```

**`casts`** — maps expression IDs to the target type when an implicit conversion is needed (e.g., an `Integer` literal used where `Real` is expected).

---

## 8. `Ctx` and the Inference Walk

`infere_body_query` constructs a `Ctx` and drives the walk:

```rust
struct Ctx<'a> {
    result: InferenceResult,
    body: &'a Body,
    db: &'a dyn HirTyDB,
    expr_stmt_ty: Option<Type>,  // expected value type for Expr-statement bodies
}
```

`expr_stmt_ty` is set for parameters (`param_data(param).ty`) and variables (`var_data(var).ty`) — bodies that consist of a single value expression rather than a procedural block. For module analog blocks and functions it is `None`.

**`infere_stmt(stmt)`** dispatches on `Stmt` variant:

- `Stmt::Expr(expr)` → `infere_assignment(stmt, expr, expr_stmt_ty)` (treats the expression as an implicit assignment to the declared type)
- `Stmt::Assignment { dst, val, op }` → `infere_assignment_dst` to resolve `dst`, then `infere_assignment` to check `val` against the destination type
- `Stmt::If/ForLoop/WhileLoop { cond }` → `infere_cond` (expects `Condition`)
- `Stmt::Case { discr, case_arms }` → infers discriminant; checks each arm value satisfies the discriminant's `TyRequirement`
- All statement kinds call `walk_child_stmts` to recurse

**`infere_expr(stmt, expr) -> Option<Ty>`** is the core dispatch. It matches on `self.body.exprs[expr]`:

| `Expr` variant | Result |
|---|---|
| `Missing` | `None` |
| `Path { port: true }` | `Ty::PortFlow(resolve_item_path)` |
| `Path { port: false }` | dispatch on resolved `ScopeDefItem` (see §10) |
| `BinaryOp { op: None }` | recurse into both sides, return `None` |
| `BinaryOp { op: Some(op) }` | `infere_bin_op` — selects signature based on op category |
| `UnaryOp { Identity }` | propagate child type |
| `UnaryOp { Neg }` | expect `Integer` or `Real`; return same type |
| `UnaryOp { BitNegate }` | expect `Integer`; return `Integer` |
| `UnaryOp { Not }` | expect `Condition`; return `Bool` |
| `Select { cond, then_val, else_val }` | `infere_cond(cond)`; resolve then/else against `SELECT` signatures |
| `Call { fun, args }` | `infere_fun_call` |
| `Array([])` | `Ty::Val(EmptyArray)` |
| `Array(args)` | `infere_array` — all elements must share a common type |
| `Literal(Float)` | `Ty::Literal(Real)` |
| `Literal(Int)` | `Ty::Literal(Integer)` |
| `Literal(Inf)` | sets `expr_types[expr]` to `expr_stmt_ty` and returns `None` |
| `Literal(String)` | `Ty::Literal(String)` |

After computing the type, `infere_expr` stores it in `result.expr_types[expr]` and returns it.

**`expect<CAST: bool>(expr, parent_expr, found_ty, requirements)`** checks that `found_ty` satisfies at least one requirement in the list. On success it returns the index of the matching requirement variant. If `CAST` is true (compile-time const generic) it also records a cast. On failure it pushes a `TypeMismatch` diagnostic.

---

## 9. Assignment Destination Resolution

`infere_assignment_dst(stmt, dst_expr, op)` infers `dst_expr`, then classifies the result:

| `Ty` of `dst_expr` | `AssignDst` |
|---|---|
| `Ty::Var(ty, var)` | `AssignDst::Var(var)` |
| `Ty::FunctionVar { fun, ty, arg }` | `AssignDst::FunVar { fun, arg }` |
| `Ty::Val(Real)` with `resolved_calls[dst_expr] == BuiltIn::potential` | `AssignDst::Potential(BranchWrite)` |
| `Ty::Val(Real)` with `resolved_calls[dst_expr] == BuiltIn::flow` | `AssignDst::Flow(BranchWrite)` |
| Anything else | `InvalidAssignDst` diagnostic |

The `BranchWrite` for a nature access is extracted from `resolved_signatures[dst_expr]`:

- `NATURE_ACCESS_BRANCH` → `BranchWrite::Named(args[0].unwrap_branch())`
- `NATURE_ACCESS_NODES` → `BranchWrite::Unnamed { hi: args[0].unwrap_node(), lo: Some(args[1].unwrap_node()) }`
- `NATURE_ACCESS_NODE_GND` → `BranchWrite::Unnamed { hi: args[0].unwrap_node(), lo: None }`
- `NATURE_ACCESS_PORT_FLOW` → error (`PotentialOfPortFlow` is illegal as an assignment destination)

**Operator cross-check.** After determining the `AssignDst`, the operator is validated:

- `Var`/`FunVar` + `<+` → `InvalidAssignDst` (suggest `=`)
- `Flow`/`Potential` + `=` → `InvalidAssignDst` (suggest `<+`)
- All other combinations → `assignment_destination.insert(stmt, dst)`

---

## 10. Nature Access Resolution

When `infere_expr` encounters `Expr::Call { fun, args }` and the resolved `ScopeDefItem` is `NatureAccess(access)`, it calls `infere_nature_access(stmt, expr, access, args)`.

The resolution proceeds in three steps:

**Step 1 — Argument validation.** `infere_builtin(stmt, expr, BuiltIn::flow, args)` is called unconditionally. The `FLOW` built-in has four signatures:

```
NATURE_ACCESS_BRANCH(Branch) -> Real
NATURE_ACCESS_NODES(Node, Node) -> Real
NATURE_ACCESS_NODE_GND(Node) -> Real
NATURE_ACCESS_PORT_FLOW(PortFlow) -> Real
```

`resolve_function_args` picks the matching signature, records it in `resolved_signatures`, and validates the argument types. If no signature matches, the function returns early.

**Step 2 — Discipline lookup.** `infere_access_kind(nature, expr, arg0)` is called with the access attribute's owning `NatureId`. It reads `resolved_signatures[expr]` to determine the argument kind, extracts the node or branch from `expr_types[arg0]`, and calls `db.node_discipline(node)` (or `branch_info(branch).discipline`) to get the `DisciplineId`.

**Step 3 — Access kind determination.** `DisciplineTy::access(nature, db)` checks:

1. `NatureTy::compatible(discipline.flow, nature)` → `DisciplineAccess::Flow`
2. `NatureTy::compatible(discipline.potential, nature)` → `DisciplineAccess::Potential`
3. Neither → `None`

The result updates `resolved_calls[expr]`:

| `DisciplineAccess` | `ResolvedFun` stored |
|---|---|
| `Flow` | `BuiltIn::flow` |
| `Potential` | `BuiltIn::potential` |
| `None` | `InvalidNatureAccess(nature)` |

This is the mechanism by which `V(p,n)` (a `NatureAccess` for the `Voltage` nature) becomes `ResolvedFun::BuiltIn(BuiltIn::potential)` in the `InferenceResult` — because `electrical` discipline maps `Voltage` as a potential nature.

---

## 11. Worked Example — Resistor `V(p,n) <+ R * I(p,n)`

Starting from the `Body` produced by `hir_def` (see [`docs/hir_def/INTERNALS.md`](../hir_def/INTERNALS.md) §11):

```
stmt_0 = Stmt::Assignment { dst: expr_0, val: expr_1, assignment_kind: Contribute }
expr_0 = Expr::Call { fun: Some(Path["V"]), args: [expr_2, expr_3] }
expr_2 = Expr::Path { path: ["p"], port: false }
expr_3 = Expr::Path { path: ["n"], port: false }
expr_1 = Expr::BinaryOp { lhs: expr_4, rhs: expr_5, op: Some(Mul) }
expr_4 = Expr::Path { path: ["R"], port: false }
expr_5 = Expr::Call { fun: Some(Path["I"]), args: [expr_6, expr_7] }
expr_6 = Expr::Path { path: ["p"], port: false }
expr_7 = Expr::Path { path: ["n"], port: false }
```

**Step 1 — `infere_stmt(stmt_0)`.**  
Dispatches to `infere_assignment_dst(stmt_0, expr_0, Contribute)`.

**Step 2 — Resolve `expr_0 = V(p,n)`.**  
`infere_expr(stmt_0, expr_0)` → `Expr::Call`. `resolve_path(…, ["V"])` → `ScopeDefItem::NatureAccess(voltage_access_id)`.  
`infere_nature_access` called:

- `infere_builtin(BuiltIn::flow, [expr_2, expr_3])`:
  - `infere_expr(stmt_0, expr_2)` → `"p"` → `ScopeDefItem::NodeId(node_p)` → `Ty::Node(node_p)`
  - `infere_expr(stmt_0, expr_3)` → `"n"` → `Ty::Node(node_n)`
  - Signature `NATURE_ACCESS_NODES(Node, Node) -> Real` selected
  - `resolved_signatures[expr_0] = NATURE_ACCESS_NODES`
- `infere_access_kind(voltage_nature, expr_0, expr_2)`:
  - `node_discipline(node_p)` → `electrical_discipline_id`
  - `discipline_info(electrical).access(voltage_nature, db)`:
    - `NatureTy::compatible(current_nature, voltage_nature)` → false (units differ: A vs V)
    - `NatureTy::compatible(voltage_nature, voltage_nature)` → true
    - Returns `DisciplineAccess::Potential`
- `resolved_calls[expr_0] = ResolvedFun::BuiltIn(BuiltIn::potential)`
- `expr_types[expr_0] = Ty::Val(Type::Real)`

**Step 3 — Assignment destination.**  
Back in `infere_assignment_dst`:  
`Ty::Val(Real)` + `resolved_calls == BuiltIn::potential` + `NATURE_ACCESS_NODES` →  
`AssignDst::Potential(BranchWrite::Unnamed { hi: node_p, lo: Some(node_n) })`

Operator check: `Potential` + `Contribute` → valid.  
`assignment_destination[stmt_0] = AssignDst::Potential(Unnamed { hi: node_p, lo: Some(node_n) })`

**Step 4 — Resolve `expr_1 = R * I(p,n)`.**  
`infere_assignment(stmt_0, expr_1, Some(Real))`:

- `infere_expr(stmt_0, expr_4)` → `"R"` → `ScopeDefItem::ParamId(param_R)` → `Ty::Param(Real, param_R)`
- `infere_expr(stmt_0, expr_5)` = `I(p,n)` → same process as `V(p,n)` but with `Current` nature → `resolved_calls[expr_5] = BuiltIn::flow` → `Ty::Val(Real)`
- `infere_bin_op(Mul, expr_4, expr_5)`:
  - Both satisfy `REAL_BIN_OP`; signature `REAL_OP` selected
  - `resolved_signatures[expr_1] = REAL_OP`
  - `expr_types[expr_1] = Ty::Val(Real)`
- `Real.is_assignable_to(Real)` → no cast needed

**Final `InferenceResult` (relevant entries):**

```
expr_types:
  expr_0 → Ty::Val(Real)       // V(p,n)
  expr_1 → Ty::Val(Real)       // R * I(p,n)
  expr_2 → Ty::Node(node_p)
  expr_3 → Ty::Node(node_n)
  expr_4 → Ty::Param(Real, param_R)
  expr_5 → Ty::Val(Real)       // I(p,n)
  expr_6 → Ty::Node(node_p)
  expr_7 → Ty::Node(node_n)

resolved_calls:
  expr_0 → BuiltIn::potential
  expr_5 → BuiltIn::flow

resolved_signatures:
  expr_0 → NATURE_ACCESS_NODES
  expr_5 → NATURE_ACCESS_NODES

assignment_destination:
  stmt_0 → Potential(Unnamed { hi: node_p, lo: Some(node_n) })

casts: {}
diagnostics: []
```

`hir_lower` consumes this `InferenceResult` to emit the MIR contribution statement, using `assignment_destination[stmt_0]` to know that this is a potential (voltage) write across the `p`–`n` branch.
