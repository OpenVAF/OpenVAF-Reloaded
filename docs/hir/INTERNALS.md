# `hir` Crate Internals

> **Cross-links:**
> [`hir_def` INTERNALS](../hir_def/INTERNALS.md) |
> [`hir_ty` INTERNALS](../hir_ty/INTERNALS.md) |
> [`hir_lower` INTERNALS](../hir_lower/INTERNALS.md) |
> [Architecture overview](../ARCHITECTURE.md)

---

## 1. Purpose and Position

The `hir` crate is the **public API surface of the entire OpenVAF front end**.
Everything below it — `hir_def` (item tree, name resolution, bodies) and
`hir_ty` (type inference, nature/discipline resolution) — is considered an
implementation detail.

The crate's own module-level comment captures the design intent precisely:

> HIR is written in "OO" style. Each type is self-contained (as in, it knows
> its parents and full context). `hir_*` crates are written in "ECS" style, with
> relatively little abstraction. Many types are not self-contained, and
> explicitly use local indexes, arenas, etc.

Concretely, this means:

- `hir_def` exposes raw Salsa intern IDs (`ModuleId`, `VarId`, `NodeId`, …)
  and arena indices.  Callers must pass a database reference to every
  operation and know which arena to index.
- `hir_ty` exposes `InferenceResult` maps keyed by `ExprId`/`StmtId`.
  Callers must join them with a `hir_def::Body` themselves.
- `hir` wraps each intern ID in a thin newtype (`Module`, `Variable`, `Node`,
  …), combines `Body` and `InferenceResult` into a single `BodyRef`, and
  presents a uniform API where every method takes only `&CompilationDB`.

The crate is the layer that `hir_lower`, `sim_back`, `osdi`, and any external
tooling (e.g. language servers, linters) actually import.  Nothing inside the
`hir_*` implementation crates is visible through the `hir` public API unless
explicitly re-exported.

---

## 2. Module Map

```
hir/src/
  lib.rs           — all public OO types: CompilationUnit, Module, Node, Branch, …
  db.rs            — CompilationDB: the single concrete Salsa database
  body.rs          — Body, BodyRef, Stmt, Expr, Ref, ResolvedFun, AssignmentLhs
  rec_declarations.rs — RecDeclarations: depth-first scope iterator
  declarations.rs  — ScopePaths: scope iterator that builds dotted paths (older variant)
  attributes.rs    — AstCache: SourceFile + AstIdMap pairing for attribute lookup
  diagnostics.rs   — collect(): stitches all diagnostic sources into one sink pass
hir/tests/
  data_tests.rs    — integration and UI tests via mini_harness
```

`declaration.rs` and `rec_declarations.rs` are nearly identical; the former
uses `slice::Iter` on a `Vec<(Name, ScopeDefItem)>` and the latter (the
current implementation) uses `indexmap::map::Iter`.  `RecDeclarations` is what
`Module::rec_declarations()` actually returns.

---

## 3. `CompilationDB` — The Single Concrete Database

```rust
// db.rs
#[salsa::database(BaseDatabase, InternDatabase, HirDefDatabase, HirTyDatabase)]
pub struct CompilationDB {
    storage: salsa::Storage<CompilationDB>,
    vfs: Arc<RwLock<Vfs>>,
    root_file: FileId,
}
```

`CompilationDB` is the only type in the entire OpenVAF compiler that
implements all four Salsa query groups simultaneously:

| Query group | Defined in | Provides |
|---|---|---|
| `BaseDatabase` | `basedb` | VFS, preprocessing, parsing, `AstIdMap` |
| `InternDatabase` | `hir_def` | `intern_*` / `lookup_intern_*` for all Salsa IDs |
| `HirDefDatabase` | `hir_def` | `item_tree`, `def_map`, `body`, `*_data` queries |
| `HirTyDatabase` | `hir_ty` | `inference_result`, `discipline_info`, `branch_info`, … |

Because it implements all four groups, a `&CompilationDB` can be passed
wherever any of the underlying query group trait objects are required.  The
`Upcast<dyn HirDefDB>` and `Upcast<dyn BaseDB>` impls make this explicit.

### Constructors

**`new_fs(root_file, include_dirs, macro_flags, lints)`** — production entry
point.  Reads the file from the real filesystem, registers include directories,
applies the standard macro flags (`STANDARD_FLAGS` from `basedb`), and converts
named lint overrides into the global lint overwrite table.

**`new_virtual(contents)`** — test-and-tooling entry point.  Registers a
virtual file at `/root.va` with no include directories.  Used extensively in
the integration and UI tests.

**`new(root_file, contents, include_dirs, macro_flags, lints)`** — the shared
implementation that both constructors delegate to.  It:

1. Creates a `Vfs`, inserts the standard library (from `basedb`), and assigns
   a `FileId` to the root file.
2. Builds the include-dir list, prepending `/std` (the built-in standard
   library virtual path).
3. Constructs the `STANDARD_FLAGS` prefix for macro flags.
4. Populates `global_lint_overwrites`: the special names `"all"`,
   `"warnings"`, and `"errors"` expand to bulk overrides; any other string is
   looked up in the lint registry.

The `unsafe transmute` inside the lint-overwrite construction is a Salsa
ergonomic workaround: Salsa requires `Arc<TiSlice<K, V>>` but constructing one
requires going through a plain `Arc<[V]>` first.

### Snapshot support

`CompilationDB` implements `ParallelDatabase` by cloning the Salsa storage
snapshot and sharing the same `Arc<RwLock<Vfs>>`.  This enables parallel
query execution in multi-threaded contexts.

---

## 4. `CompilationUnit` — The Compilation Entry Point

```rust
pub struct CompilationUnit {
    root_file: FileId,
}
```

`CompilationUnit` is the user-facing handle to a single compiled Verilog-A
file.  Obtained via `db.compilation_unit()`.

| Method | What it does |
|---|---|
| `name(db)` | Returns the filename from the VFS path |
| `root_file()` | Returns the raw `FileId` |
| `modules(db)` | Walks `def_map(root_file)[entry].declarations` for `ModuleId` items → `Vec<Module>` |
| `diagnostics(db, sink)` | Delegates to `diagnostics::collect(db, root_file, sink)` |
| `test_diagnostics(db)` | Like `diagnostics` but captures to a colour-stripped `Buffer`; used in snapshot tests |
| `ast(db)` | Constructs an `AstCache` for attribute resolution |
| `preprocess(db)` | Returns the `Preprocess` result (macro-expanded token stream) |

The `.modules()` implementation shows the ECS→OO translation pattern in its
simplest form:

```rust
root_def_map[root_def_map.entry()]
    .declarations
    .iter()
    .filter_map(|(_, def)| {
        if let ScopeDefItem::ModuleId(id) = *def {
            Some(Module { id })
        } else {
            None
        }
    })
    .collect()
```

The raw `ScopeDefItem::ModuleId(id)` from `hir_def` is wrapped in the OO
newtype `Module { id }` before being returned.

---

## 5. OO Type Hierarchy

Every entity in a Verilog-A compilation has a corresponding OO newtype in
`hir`.  Each type stores only an intern ID and delegates all queries to
`&CompilationDB`.

### Entity types

| Type | Wraps | Key methods |
|---|---|---|
| `Module` | `ModuleId` | `name`, `ports`, `internal_nodes`, `child_scopes`, `declarations`, `rec_declarations`, `analog_block`, `analog_initial_block`, `lookup_var` |
| `Block` | `BlockId` | `name` |
| `Function` | `FunctionId` | `name`, `return_ty`, `args`, `arg(idx)`, `body` |
| `FunctionArg` | `FunctionId + LocalFunctionArgId` | `name`, `ty`, `is_input`, `is_output`, `function` |
| `Node` | `NodeId` | `name`, `discipline`, `is_input`, `is_output`, `is_port`, `is_gnd` |
| `Variable` | `VarId` | `name`, `ty`, `init`, `get_attr` |
| `Parameter` | `ParamId` | `name`, `default`, `bounds`, `init`, `ty`, `get_attr` |
| `AliasParameter` | `AliasParamId` | `name`, `resolve` → `Option<ResolvedAliasParameter>` |
| `Branch` | `BranchId` | `name`, `discipline`, `kind`, `get_attr` |
| `Discipline` | `DisciplineId` | `name`, `potential` → `Option<Nature>`, `flow` → `Option<Nature>` |
| `Nature` | `NatureId` | `name`, `units` |
| `NatureAttribute` | `NatureAttrId` | `name`, `value` → `Body` |

All types derive `Copy`, `Clone`, `PartialEq`, `Eq`, `Hash`.  `Debug` is
implemented via the `stdx::impl_debug!` macro, which formats using the
underlying Salsa intern ID's debug representation.

### `FunctionArg` is a two-field struct

```rust
pub struct FunctionArg {
    fun_id: FunctionId,
    arg_id: LocalFunctionArgId,
}
```

`LocalFunctionArgId` is a typed index into `FunctionData::args`.  This
diverges from the single-ID pattern because function arguments don't have
their own Salsa intern ID in `hir_def` — they are always accessed through the
parent `FunctionData`.

### `AliasParameter::resolve`

Alias parameters are a Verilog-A construct that lets a parameter be an alias
for another parameter or a system parameter (`$temperature`, etc.).
`AliasParameter::resolve()` calls `db.resolve_alias(id)` (a `hir_ty` query)
and maps the result:

```rust
hir_ty::db::Alias::Cycel        → None          // cycle in alias chain
hir_ty::db::Alias::Param(id)    → Some(ResolvedAliasParameter::Parameter(Parameter { id }))
hir_ty::db::Alias::ParamSysFun  → Some(ResolvedAliasParameter::SystemParameter(param))
```

The `Alias::Cycel` variant name is a typo in `hir_ty` source; it is preserved
faithfully here.

### `Module::analog_block` vs `analog_initial_block`

```rust
pub fn analog_initial_block(&self, db: &CompilationDB) -> Body {
    Body::new(DefWithBodyId::ModuleId { initial: true, module: self.id }, db)
}
pub fn analog_block(&self, db: &CompilationDB) -> Body {
    Body::new(DefWithBodyId::ModuleId { initial: false, module: self.id }, db)
}
```

In Verilog-A, an `analog initial` block runs once at simulation start; the
main `analog` block runs at each time step.  Both are represented as
`DefWithBodyId::ModuleId` differing only in the `initial` flag.

---

## 6. Scope Traversal

### `Scope` enum

```rust
pub enum Scope {
    Module(Module),
    Block(Block),
    Function(Function),
}
```

`Scope` provides a unified handle over the three kinds of declaration
containers.  The private `def_map_and_scope()` method resolves the appropriate
`(LocalScopeId, Arc<DefMap>)` pair for each variant:

- `Module` → `id.lookup(db).scope.local_scope` and the root def map
- `Block` → `db.block_def_map(id)`, entry scope (may be `None` for unnamed blocks)
- `Function` → `db.function_def_map(id)`, entry scope

`.children(db)` returns immediate child scopes by examining
`def_map[scope].children` and mapping `ScopeOrigin` → `Scope` variant.

`.declarations(db)` returns `Vec<(Name, ScopeDef)>` for the user-visible
declarations in this scope.  Implementation details — `BuiltIn`, `NatureId`,
`NatureAccess`, `DisciplineId`, `ParamSysFun`, `FunctionReturn`,
`FunctionArgId`, `NatureAttrId` — are filtered out with `return None`.

### `ScopeDef` enum

```rust
#[non_exhaustive]
pub enum ScopeDef {
    Block(Block),
    ModuleInstance(Module),
    Node(Node),
    Variable(Variable),
    Parameter(Parameter),
    AliasParameter(AliasParameter),
    Branch(Branch),
    Function(Function),
}
```

This is the public projection of `hir_def::nameres::ScopeDefItem`.  The
`#[non_exhaustive]` attribute signals that new variants may be added in future
versions without being a breaking change.

### `RecDeclarations`

`RecDeclarations<'a>` is an `Iterator<Item = (Name, ScopeDef)>` that walks
all declarations in a scope and recursively descends into named `Block`
sub-scopes.

```rust
pub struct RecDeclarations<'a> {
    path: Vec<Name>,
    stack: Vec<Scope>,
    db: &'a CompilationDB,
}
```

The `stack` holds a `Vec<Scope>` of pending scopes to visit; each `Scope`
element wraps an `Arc<DefMap>` to keep the map alive and an `indexmap::Iter`
over its declarations.  When a `BlockId` entry is encountered and that block
has a named def map, a new frame is pushed onto the stack and the traversal
descends.

`to_path(name)` builds the dotted-path string for the current position:
`["foo", "bar"]` + `name = "x"` → `"foo.bar.x"`.

`Module::rec_declarations(db)` is the entry point:

```rust
pub fn rec_declarations(self, db: &CompilationDB) -> RecDeclarations<'_> {
    RecDeclarations::new(Scope::Module(self), db)
}
```

### `BranchWrite` and `BranchKind`

```rust
pub enum BranchWrite {
    Named(Branch),
    Unnamed { hi: Node, lo: Option<Node> },
}

pub enum BranchKind {
    PortFlow(Node),
    NodeGnd(Node),
    Nodes(Node, Node),
}
```

`BranchWrite` represents the target of a contribution statement (`V(a,b) <+`
or `I(b) <+`).  A named branch refers to an explicit `branch` declaration; an
unnamed branch is written directly with node references.

`BranchWrite::nodes(db)` resolves a named branch to its `(hi, Option<lo>)`
node pair by delegating to `Branch::kind(db)`.

`BranchKind` is the resolved form of a `BranchId`: either a two-node branch
(`Nodes`), a single-node-to-ground branch (`NodeGnd`), or a port-flow branch
(`PortFlow`).

The `From<inference::BranchWrite>` impl on the public `BranchWrite` wraps the
`hir_ty` internal type (`inference::BranchWrite`) in the public `hir` types.

---

## 7. `Body` and `BodyRef`

```rust
pub struct Body {
    body: Arc<hir_def::body::Body>,
    infere: Arc<inference::InferenceResult>,
}
```

`Body` bundles the two pieces of data a consumer needs to traverse a
definition's expression tree:

- `hir_def::body::Body` — the unresolved `Expr`/`Stmt` arenas, entry
  statement list, scope assignments
- `hir_ty::inference::InferenceResult` — the five inference maps
  (`expr_types`, `resolved_calls`, `resolved_signatures`,
  `assignment_destination`, `casts`)

Both are `Arc`-wrapped so `Body` is cheap to clone and share across threads.

`Body::borrow()` gives a `BodyRef<'_>`, a borrowed view:

```rust
pub struct BodyRef<'a> {
    body: &'a hir_def::body::Body,
    infere: &'a inference::InferenceResult,
}
```

`BodyRef` is the type all traversal methods live on.  The split between `Body`
(owned) and `BodyRef` (borrowed) follows the same pattern as `String`/`str`.

### Entry point

```rust
pub fn entry(&self) -> &'a [StmtId] {
    &self.body.entry_stmts
}
```

Returns the top-level statement IDs for this body.  Callers iterate over these
and call `get_stmt()` to obtain decoded public `Stmt` values.

### Type-projection helpers

These helpers extract a specific Salsa ID from an `ExprId` by consulting
`infere.expr_types`:

| Method | Returns | When to call |
|---|---|---|
| `into_node(expr)` | `Node` | When expr type is `Ty::Node(id)` |
| `into_port_flow(expr)` | `Node` | When expr type is `Ty::PortFlow(id)` |
| `into_parameter(expr)` | `Parameter` | When expr type is `Ty::Param(_, id)` |
| `into_branch(expr)` | `Branch` | When expr type is `Ty::Branch(id)` |

These are used by `hir_lower` to extract the resolved entity references from
nature-access call arguments.

### `expr_type` and `needs_cast`

```rust
pub fn expr_type(&self, expr: ExprId) -> Type {
    self.infere.expr_types[expr].to_value().unwrap()
}

pub fn needs_cast(&self, expr: ExprId) -> Option<(Type, &'a Type)> {
    let dst = self.infere.casts.get(&expr)?;
    let src = self.expr_type(expr);
    Some((src, dst))
}
```

`expr_type` projects `Ty` (the extended type from `hir_ty`) down to the
simpler `Type` (the surface type from `hir_def`) via `Ty::to_value()`.

`needs_cast` returns `Some((src_type, dst_type))` when the inference result
recorded a required implicit cast.  The backend uses this to emit the
appropriate LLVM conversion instruction.

### Literal helpers (`as_literalint`, `as_literalsignedint`)

Two low-level helpers that pattern-match on `hir_def::Expr` directly to
extract integer literals, including the signed case where a literal is wrapped
in a `UnaryOp::Neg`.  These are used by downstream backends that need to
inspect constant integer values at compile time.

---

## 8. `Stmt` Dispatch

`BodyRef::get_stmt(stmnt: StmtId) → Option<Stmt<'a>>` translates a raw
`hir_def::Stmt` into the public `Stmt` enum.  Returns `None` for
`hir_def::Stmt::Empty` and `hir_def::Stmt::Missing` (placeholder nodes
inserted by the parser on syntax errors).

### Full dispatch table

| `hir_def::Stmt` | Mapped to public `Stmt` |
|---|---|
| `Empty` / `Missing` | `None` |
| `Expr(e)` | `Stmt::Expr(e)` |
| `EventControl { event, body }` | `Stmt::EventControl { event, body }` |
| `Assignment { val, .. }` with `AssignDst::Var(id)` | `Stmt::Assignment { lhs: AssignmentLhs::Variable(..), rhs: val }` |
| `Assignment { val, .. }` with `AssignDst::FunVar { fun, arg: None }` | `Stmt::Assignment { lhs: AssignmentLhs::FunctionReturn(..), rhs: val }` |
| `Assignment { val, .. }` with `AssignDst::FunVar { fun, arg: Some(arg) }` | `Stmt::Assignment { lhs: AssignmentLhs::FunctionArg(..), rhs: val }` |
| `Assignment { val, .. }` with `AssignDst::Flow(branch)` | `Stmt::Contribute { kind: ContributeKind::Flow, branch: branch.into(), rhs: val }` |
| `Assignment { val, .. }` with `AssignDst::Potential(branch)` | `Stmt::Contribute { kind: ContributeKind::Potential, branch: branch.into(), rhs: val }` |
| `Block { body }` | `Stmt::Block { body }` |
| `If { cond, then_branch, else_branch }` | `Stmt::If { .. }` |
| `ForLoop { init, cond, incr, body }` | `Stmt::ForLoop { .. }` |
| `WhileLoop { cond, body }` | `Stmt::WhileLoop { .. }` |
| `Case { discr, case_arms }` | `Stmt::Case { .. }` |

The critical translation is for `hir_def::Stmt::Assignment`.  In the raw AST,
both variable assignments (`foo = bar`) and contribution statements
(`V(a,b) <+`) are represented as the same `Assignment` node.  The
`InferenceResult::assignment_destination` map (keyed by `StmtId`) holds the
resolved `AssignDst` that disambiguates them at the `hir` level:

- `AssignDst::Var` / `AssignDst::FunVar` → `Stmt::Assignment`
- `AssignDst::Flow` / `AssignDst::Potential` → `Stmt::Contribute`

This is one of the most important translations in the crate: callers never
need to know that both are represented identically in `hir_def`.

### Public `Stmt` enum

```rust
pub enum Stmt<'a> {
    Expr(ExprId),
    EventControl { event: &'a Event, body: StmtId },
    Contribute { kind: ContributeKind, branch: BranchWrite, rhs: ExprId },
    Assignment { lhs: AssignmentLhs, rhs: ExprId },
    Block { body: &'a [StmtId] },
    If { cond: ExprId, then_branch: StmtId, else_branch: StmtId },
    ForLoop { init: StmtId, cond: ExprId, incr: StmtId, body: StmtId },
    WhileLoop { cond: ExprId, body: StmtId },
    Case { discr: ExprId, case_arms: &'a [Case] },
}
```

`'a` is the lifetime of the `BodyRef` borrow.  References like `&'a Event`,
`&'a [StmtId]`, and `&'a [Case]` borrow directly from the underlying
`hir_def::Body` arena.

---

## 9. `Expr` Dispatch

`BodyRef::get_expr(expr: ExprId) → Expr<'a>` translates a raw `hir_def::Expr`
into the public `Expr` enum.  Panics on `hir_def::Expr` variants that should
never appear in a valid (post-inference) body.

### Full dispatch table

| `hir_def::Expr` | Mapped to public `Expr` |
|---|---|
| `Path { .. }` | `Expr::Read(resolve_path(expr))` — see below |
| `BinaryOp { lhs, rhs, op: Some(op) }` | `Expr::BinaryOp { lhs, rhs, op }` |
| `UnaryOp { expr, op }` | `Expr::UnaryOp { expr, op }` |
| `Select { cond, then_val, else_val }` | `Expr::Select { .. }` |
| `Call { args, .. }` with `ResolvedFun::User { func, limit }` | `Expr::Call { fun: ResolvedFun::User { func: Function { id: func }, limit }, args }` |
| `Call { args, .. }` with `ResolvedFun::BuiltIn(builtin)` | `Expr::Call { fun: ResolvedFun::BuiltIn(builtin), args }` |
| `Call { .. }` with `ResolvedFun::Param(param)` | `Expr::Read(Ref::ParamSysFun(param))` — special case |
| `Array(args)` | `Expr::Array(args)` |
| `Literal(lit)` | `Expr::Literal(lit)` |

### `resolve_path` — path to `Ref`

`hir_def::Expr::Path` represents any identifier reference.  After inference,
the `InferenceResult::expr_types` map holds the resolved type, which encodes
the identity of the referenced entity:

| `Ty` variant | Mapped to `Ref` |
|---|---|
| `Ty::Var(_, id)` | `Ref::Variable(Variable { id })` |
| `Ty::Param(_, id)` | `Ref::Parameter(Parameter { id })` |
| `Ty::FunctionVar { fun, arg: Some(arg), .. }` | `Ref::FunctionArg(FunctionArg { fun_id: fun, arg_id: arg })` |
| `Ty::FunctionVar { fun, arg: None, .. }` | `Ref::FunctionReturn(Function { id: fun })` |
| `Ty::NatureAttr(_, id)` | `Ref::NatureAttr(NatureAttribute { id })` |
| any other `Ty` + `resolved_calls` entry `ResolvedFun::Param(param)` | `Ref::ParamSysFun(param)` |

### `Param`-as-call special case

Verilog-A system parameters like `$temperature` and `$vt` can be written
either as bare identifiers or as zero-argument calls (`$temperature()`).
`hir_def` parses the call form as `Expr::Call`, but `hir_ty` resolves it to
`inference::ResolvedFun::Param`.  `get_expr()` catches this case and returns
`Expr::Read(Ref::ParamSysFun(param))` instead of `Expr::Call`, hiding the
syntactic distinction from downstream consumers.

### `get_call_signature`

```rust
pub fn get_call_signature(&self, expr: ExprId) -> Signature {
    self.infere.resolved_signatures.get(&expr).copied().unwrap_or(Signature(u32::MAX))
}
```

Returns the resolved overload signature for a call expression.
`Signature(u32::MAX)` signals "no recorded signature" (e.g. user-defined
function calls, which are not overloaded).  The `hir::signatures` module
re-exports all named `Signature` constants from `hir_ty::builtin` and
`hir_ty::types` so callers can match against them by name.

### Public `Expr` enum

```rust
pub enum Expr<'a> {
    Read(Ref),
    BinaryOp { lhs: ExprId, rhs: ExprId, op: BinaryOp },
    UnaryOp { expr: ExprId, op: UnaryOp },
    Select { cond: ExprId, then_val: ExprId, else_val: ExprId },
    Call { fun: ResolvedFun, args: &'a [ExprId] },
    Array(&'a [ExprId]),
    Literal(&'a Literal),
}
```

```rust
pub enum Ref {
    Variable(Variable),
    Parameter(Parameter),
    FunctionArg(FunctionArg),
    FunctionReturn(Function),
    NatureAttr(NatureAttribute),
    ParamSysFun(ParamSysFun),
}

pub enum ResolvedFun {
    User { func: Function, limit: bool },
    BuiltIn(BuiltIn),
}
```

`ResolvedFun::User::limit` is `true` when the call goes through a `$limit`
wrapper (used in certain compact model convergence-aid patterns).

---

## 10. `AstCache` and Attribute Resolution

Verilog-A supports `(*` attribute `*)` annotations on declarations.  The
`hir` API surfaces this through `get_attr()` methods on `Variable`,
`Parameter`, and `Branch`.  These methods require an `AstCache`:

```rust
pub struct AstCache {
    ast: syntax::SourceFile,
    id_map: Arc<AstIdMap>,
}
```

`AstCache` is constructed via `CompilationUnit::ast(db)`, which calls
`db.parse(root_file).tree()` and `db.ast_id_map(root_file)`.  It pairs the
concrete syntax tree with the stable `AstIdMap` (the `ErasedAstId` index
structure maintained by `basedb`).

`resolve_attribute(name, erased_id)` locates an attribute by name on any AST
node identified by its `ErasedAstId`:

1. Calls `id_map.get_attr(id, attribute)` to get the index of the named
   attribute (returns `None` if absent).
2. Calls `id_map.get_syntax(id).to_node(ast.syntax())` to recover the CST node.
3. Retrieves the attributes iterator for that node.  For `Var` and `Param`
   nodes, attributes are attached to the parent statement, not the declaration
   itself, so `ast.parent().unwrap()` is called.
4. Returns `attrs.nth(idx)` — the `ast::Attr` CST node.

The returned `ast::Attr` can be inspected to extract the attribute's value
expression.  This round-trip through the CST is intentional: the attribute
system is not lowered into `hir_def`, so the only way to access attributes is
to go back to the original parse tree.

---

## 11. Diagnostics Collection

`hir::diagnostics::collect(db, root_file, sink)` is the single function that
aggregates all front-end diagnostics into one `DiagnosticSink` pass.  It is
called by `CompilationUnit::diagnostics()`.

The collection pipeline runs in this order:

1. **Preprocessor diagnostics** — `db.preprocess(root_file).diagnostics`
   (unknown macros, unterminated `ifdef`, etc.)

2. **Parser diagnostics** — `db.parse(root_file).errors()` (syntax errors
   from the CST parser)

3. **Type-validation diagnostics** — `hir_ty::validation::TypeValidationDiagnostic::collect(db, root_file)`
   wrapped in `TypeValidationDiagnosticWrapped` with access to the parse tree,
   source map, and item tree for source location rendering

4. **Name-resolution (def) diagnostics** — `collect_def_map()` iterates
   `def_map.diagnostics`, wrapping each in `DefDiagnosticWrapped`

5. **Body/inference diagnostics** — for each module (analog + initial blocks),
   function, and named block:
   - `InferenceDiagnosticWrapped` — type mismatches, unresolved names, invalid
     nature accesses, etc. from `inference_result(def).diagnostics`
   - `BodyValidationDiagnostic::collect(db, def)` — semantic rule violations
     (e.g. `ddt`/`idt` used outside analog context) wrapped in
     `BodyValidationDiagnosticWrapped`

The traversal is recursive: `collect_scope()` recursively visits child
functions and named blocks inside each module scope.

All diagnostic types implement the `Diagnostic` trait from `basedb`, which
provides a `render()` method that produces a source-annotated message using
the parse tree, source map, and AST ID map.

---

## 12. Worked Example

### Verilog-A source

```verilog
`include "disciplines.vams"

module resistor(p, n);
  inout p, n;
  electrical p, n;
  parameter real R = 1e3;
  analog begin
    V(p,n) <+ R * I(p,n);
  end
endmodule
```

### Step 1: Build the database

```rust
use hir::CompilationDB;

let db = CompilationDB::new_virtual(SOURCE)?;
let unit = db.compilation_unit();
```

`new_virtual` registers the source at `/root.va`, inserts the standard
library, and returns a `CompilationDB`.  No Salsa queries have run yet;
everything is demand-driven.

### Step 2: Enumerate modules

```rust
let modules = unit.modules(&db);
// → [Module { id: ModuleId(0) }]

let m = modules[0];
println!("{}", m.name(&db)); // "resistor"
```

`modules()` walks `def_map(root_file)[entry].declarations` for
`ScopeDefItem::ModuleId` entries and wraps each in `Module { id }`.

### Step 3: Inspect ports and internal nodes

```rust
let ports = m.ports(&db);
// → [Node { NodeId(0) = "p" }, Node { NodeId(1) = "n" }]

for node in &ports {
    println!("{}: discipline={}", node.name(&db), node.discipline(&db).name(&db));
}
// p: discipline=electrical
// n: discipline=electrical
```

`Node::discipline()` calls `db.node_discipline(id)` (a `hir_ty` query that
resolves the discipline from the node's declaration scope).

### Step 4: Enumerate declarations

```rust
for (name, def) in m.rec_declarations(&db) {
    println!("{name}: {def:?}");
}
// p:     Node(Node { NodeId(0) })
// n:     Node(Node { NodeId(1) })
// R:     Parameter(Parameter { ParamId(0) })
```

`rec_declarations` uses `RecDeclarations`, recursing into named sub-scopes.
Builtins, natures, and disciplines are filtered out.

### Step 5: Walk the analog body

```rust
let body = m.analog_block(&db);
let br = body.borrow();

for &sid in br.entry() {
    match br.get_stmt(sid) {
        Some(hir::Stmt::Contribute { kind, branch, rhs }) => {
            println!("Contribute ({kind:?}) to {branch:?}");
            // Contribute (Potential) to Unnamed { hi: Node(p), lo: Some(Node(n)) }
        }
        _ => {}
    }
}
```

`get_stmt` consults `infere.assignment_destination[sid]`.  The raw
`hir_def::Stmt::Assignment` for `V(p,n) <+` maps to `AssignDst::Potential`,
so the result is `Stmt::Contribute { kind: ContributeKind::Potential, … }`.

### Step 6: Walk the RHS expression

The `rhs: ExprId` from the contribute statement refers to `R * I(p,n)`.
Walking it:

```rust
// rhs → BinaryOp { lhs: ExprId(R_read), rhs: ExprId(I_call), op: Mul }
match br.get_expr(rhs) {
    Expr::BinaryOp { lhs, rhs: i_call, op } => {
        // lhs: Expr::Read(Ref::Parameter(Parameter { ParamId(0) "R" }))
        // i_call: Expr::Call { fun: ResolvedFun::BuiltIn(BuiltIn::flow), args: [p, n] }
        let sig = br.get_call_signature(i_call);
        // sig == hir::signatures::NATURE_ACCESS_NODES
    }
    _ => unreachable!()
}
```

The nature access `I(p,n)` has been resolved to `ResolvedFun::BuiltIn(flow)`
with signature `NATURE_ACCESS_NODES`.  The two argument expressions have types
`Ty::Node(NodeId(0))` and `Ty::Node(NodeId(1))`, accessible via
`br.into_node(arg_expr)`.

### Summary of resolved state

| What | Raw `hir_def` | Resolved in `hir` |
|---|---|---|
| `V(p,n) <+` | `Stmt::Assignment` | `Stmt::Contribute { kind: Potential, branch: Unnamed { p, n } }` |
| `R` identifier | `Expr::Path` | `Expr::Read(Ref::Parameter(R))` |
| `I(p,n)` call | `Expr::Call` | `Expr::Call { fun: BuiltIn(flow), sig: NATURE_ACCESS_NODES }` |
| `p` argument | `Expr::Path` | `Ty::Node(p)` → `into_node()` → `Node { p }` |
