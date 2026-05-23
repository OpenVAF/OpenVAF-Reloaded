# `hir_def` — Internals

The first compilation layer above parsing: item collection, name resolution, and expression lowering.

---

## 1. Purpose and Position

`hir_def` is the crate that turns a parsed Verilog-A source file into the compiler's internal representation. It sits immediately above `basedb` (which owns the file system, the virtual file system, and the raw CST parser) and immediately below `hir_ty` (which resolves types and checks semantic correctness).

The crate produces four independent artifacts, each cached as a Salsa query:

1. **`ItemTree`** — a flat, body-free summary of all item declarations in a file. This is the *invalidation barrier*: editing inside a function body does not change the `ItemTree`, so name resolution and item data do not need to rerun.
2. **`DefMap`** — the result of name resolution: a tree of scopes, each mapping names to `ScopeDefItem` handles.
3. **`Body`** — the normalized expression/statement tree for each definition that has a body (modules, parameters, variables, functions, nature attributes).
4. **`*Data` queries** — semantic structs (`DisciplineData`, `NatureData`, `ModuleData`, …) that present item information without requiring the caller to hold a reference to the `ItemTree`.

---

## 2. Module Map

| File | Role |
|---|---|
| `lib.rs` | ID types and `impl_intern!` macro; `ItemLoc<N>`, `DefWithBodyId`, `ScopeId`; `Intern`/`Lookup` traits |
| `db.rs` | `InternDB` and `HirDefDB` Salsa query group traits |
| `item_tree.rs` | `ItemTree`, `ItemTreeData`, all item structs, `ItemTreeNode` trait |
| `item_tree/lower.rs` | `Ctx` — lowering from CST to `ItemTree` |
| `nameres.rs` | `DefMap`, `Scope`, `ScopeDefItem`, `ScopeOrigin`, path resolution |
| `nameres/collect.rs` | `DefCollector` — walks `ItemTree` to build `DefMap` |
| `body.rs` | `Body`, `BodySourceMap`, `body_with_sourcemap_query` |
| `body/lower.rs` | `LowerCtx` — lowering from CST expressions/statements to `Body` |
| `expr.rs` | `Expr`, `ExprId`, `Stmt`, `StmtId`, `Literal` |
| `data.rs` | `DisciplineData`, `NatureData`, `VarData`, `ParamData`, `NodeData`, `BranchData`, `FunctionData`, `ModuleData`, `AliasParamData` |
| `builtin.rs` | `BuiltIn`, `ParamSysFun`; `insert_builtin_scope` |
| `path.rs` | `Path` |
| `types.rs` | `Type` enum |

---

## 3. Salsa Query Architecture

The crate is structured around two Salsa query group traits defined in `db.rs`.

### `InternDB`

Provides bidirectional interning for all compound location types. For each kind of definition there is a symmetric pair of queries:

```
intern_module(ModuleLoc) → ModuleId       lookup_intern_module(ModuleId) → ModuleLoc
intern_param(ParamLoc)   → ParamId        lookup_intern_param(ParamId)   → ParamLoc
intern_var(VarLoc)       → VarId          lookup_intern_var(VarId)       → VarLoc
intern_nature(NatureLoc) → NatureId       ...
intern_discipline(DisciplineLoc) → DisciplineId
intern_block(BlockLoc)   → BlockId
intern_branch(BranchLoc) → BranchId
intern_function(FunctionLoc) → FunctionId
intern_nature_attr(NatureAttrLoc) → NatureAttrId
intern_discipline_attr(DisciplineAttrLoc) → DisciplineAttrId
intern_node(NodeLoc)     → NodeId
intern_function_arg(FunctionArgLoc) → FunctionArgId
intern_alias_param(AliasParamLoc) → AliasParamId
```

### `HirDefDB`

Provides the derived queries that downstream crates consume:

| Query | Input | Output | Notes |
|---|---|---|---|
| `item_tree` | `FileId` | `Arc<ItemTree>` | Parses file; strips bodies |
| `def_map` | `FileId` | `Arc<DefMap>` | Root-file name resolution |
| `block_def_map` | `BlockId` | `Option<Arc<DefMap>>` | Named block scope; `None` if block has no declarations |
| `function_def_map` | `FunctionId` | `Arc<DefMap>` | Function-body scope |
| `body` | `DefWithBodyId` | `Arc<Body>` | Normalized expression tree |
| `body_with_sourcemap` | `DefWithBodyId` | `(Arc<Body>, Arc<BodySourceMap>)` | With AST provenance |
| `param_body_with_sourcemap` | `ParamId` | `(Arc<Body>, Arc<BodySourceMap>, ParamExprs)` | Parameter default/bounds |
| `discipline_data` | `DisciplineId` | `Arc<DisciplineData>` | |
| `nature_data` | `NatureId` | `Arc<NatureData>` | |
| `var_data` | `VarId` | `Arc<VarData>` | |
| `param_data` | `ParamId` | `Arc<ParamData>` | |
| `node_data` | `NodeId` | `Arc<NodeData>` | |
| `branch_data` | `BranchId` | `Arc<BranchData>` | |
| `function_data` | `FunctionId` | `Arc<FunctionData>` | |
| `module_data` | `ModuleId` | `Arc<ModuleData>` | |
| `alias_data` | `AliasParamId` | `Arc<AliasParamData>` | |

The dependency graph that Salsa tracks automatically: `item_tree` is a leaf (depends only on the parsed CST). `def_map` depends on `item_tree`. `body` depends on `def_map` and `item_tree`. When a file changes, Salsa invalidates `item_tree` for that file; if the resulting `ItemTree` is structurally identical (only a body expression changed), `def_map` and `body` for unrelated definitions are not recomputed.

---

## 4. `ItemTree` — the AST Invalidation Barrier

```rust
pub struct ItemTree {
    pub top_level: Box<[RootItem]>,
    pub(crate) data: ItemTreeData,
    pub(crate) blocks: AHashMap<AstId<BlockStmt>, Block>,
}
```

The `ItemTree` captures every *item declaration* in a source file while deliberately omitting expression bodies. This means it is unchanged when the user edits inside an `analog begin ... end` block or a parameter default expression — making it an ideal Salsa invalidation boundary.

`top_level` lists the root items: only `Module`, `Nature`, and `Discipline` can appear at file scope.

`data` is the flat `ItemTreeData` struct, which holds one `Arena<T>` per item kind:

```
modules, disciplines, natures, nature_attrs, discipline_attrs,
variables, parameters, alias_parameters, nets, ports, branches, functions
```

`blocks` maps each named `begin : name` block (by its stable `AstId`) to a `Block { name, scope_items }`.

### `ItemTreeNode` trait

Every item type implements `ItemTreeNode`:

```rust
pub trait ItemTreeNode: Clone {
    type Source: AstNode;
    fn name(&self) -> &Name;
    fn ast_id(&self) -> AstId<Self::Source>;
    fn lookup(tree: &ItemTree, index: Idx<Self>) -> &Self;
    fn id_from_mod_item(mod_item: ScopeItem) -> Option<ItemTreeId<Self>>;
    fn id_to_mod_item(id: ItemTreeId<Self>) -> ScopeItem;
}
```

This makes the item types generic over the `ScopeItem` enum via the `item_tree_nodes!` macro. `Index<Idx<T>> for ItemTree` is also derived by the macro, so `tree[idx]` works for all item types.

### Item structs

| Type | Key fields |
|---|---|
| `Module` | `name`, `nodes: TiVec<LocalNodeId, Node>`, `num_ports: u32`, `items: Vec<ModuleItem>` |
| `Node` | `name`, `is_port: bool`, `decls: Vec<NodeTypeDecl>` |
| `Param` | `name`, `ty: Option<Type>`, `is_local: bool` |
| `Var` | `name`, `ty: Type` |
| `AliasParam` | `name`, `src: Option<Path>` |
| `Branch` | `name`, `kind: BranchKind` |
| `Function` | `name`, `ty: Type`, `args: TiVec<LocalFunctionArgId, FunctionArg>`, `items: Vec<FunctionItem>` |
| `Nature` | `name`, `parent: Option<NatureRef>`, `access`, `ddt_nature`, `idt_nature`, `abstol`, `units`, `attrs: IdxRange<NatureAttr>` |
| `Discipline` | `name`, `potential: Option<(NatureRef, LocalDisciplineAttrId)>`, `flow`, `domain: Option<(Domain, LocalDisciplineAttrId)>` |

`BranchKind` encodes the three syntactic forms of a Verilog-A branch declaration:

```rust
pub enum BranchKind {
    PortFlow(Path),        // branch(port_name)
    NodeGnd(Path),         // branch(node_name)  — second node is ground
    Nodes(Path, Path),     // branch(node_a, node_b)
    Missing,               // parse error recovery
}
```

A `Node` groups together all declarations that refer to the same electrical node. A module port like `inout p;` and a subsequent `electrical p;` both produce `NodeTypeDecl` entries pointing at `p`'s `Node`. The `decls: Vec<NodeTypeDecl>` field holds `NodeTypeDecl::Port(port_idx)` or `NodeTypeDecl::Net(net_idx)` for each declaration of that node.

---

## 5. ID System: Three Layers

Every definition in `hir_def` is identified at three levels of abstraction.

### Layer 1 — `ItemTreeId<N>`

```rust
pub type ItemTreeId<N> = Idx<N>;
```

A typed arena index into `ItemTreeData`. It is local to a specific `ItemTree` (i.e., a specific file). Two items in different files can have the same `ItemTreeId` and mean different things.

### Layer 2 — `ItemLoc<N>`

```rust
pub struct ItemLoc<N: ItemTreeNode> {
    pub scope: ScopeId,
    pub id: ItemTreeId<N>,
}
```

A globally unique item location: the `ScopeId` identifies which file and scope the item lives in; the `ItemTreeId` identifies which item within that file's `ItemTree`. `ItemLoc<Module>` is the type alias `ModuleLoc`, `ItemLoc<Param>` is `ParamLoc`, and so on.

### Layer 3 — Interned IDs

```rust
pub struct ModuleId(salsa::InternId);
pub struct ParamId(salsa::InternId);
// … one per item kind
```

Opaque handles that wrap a Salsa-interned integer. They are `Copy`, `Hash`, and stable across incremental recomputation. The `impl_intern!` macro generates both the struct and the `Intern` / `Lookup` trait implementations:

```rust
impl Intern for ModuleLoc {
    type ID = ModuleId;
    fn intern(self, db: &dyn HirDefDB) -> ModuleId { db.intern_module(self) }
}
impl Lookup for ModuleId {
    type Data = ModuleLoc;
    fn lookup(&self, db: &dyn HirDefDB) -> ModuleLoc { db.lookup_intern_module(*self) }
}
```

Callers obtain the full location with `id.lookup(db)`, and from there can reach the `ItemTree` entry with `loc.item_tree(db)[loc.id]`.

---

## 6. `DefMap` and Name Resolution

```rust
pub struct DefMap {
    src: DefMapSource,
    scopes: Arena<Scope>,
    root_scope: LocalScopeId,
    pub diagnostics: Vec<DefDiagnostic>,
}

pub struct Scope {
    pub origin: ScopeOrigin,
    parent: Option<LocalScopeId>,
    pub children: IndexMap<Name, LocalScopeId>,
    pub declarations: IndexMap<Name, ScopeDefItem>,
}
```

A `DefMap` is a flat arena of `Scope`s connected by parent links. `root_scope` is the entry scope (always index 0). `DefMap::entry()` returns `LocalScopeId::from(0u32)`.

**`DefMapSource`** records which Salsa query produced this map:

```rust
pub enum DefMapSource {
    Root,                // def_map(file)
    Block(BlockId),      // block_def_map(block)
    Function(FunctionId), // function_def_map(fun)
}
```

**`ScopeOrigin`** records what construct opened the scope:

```rust
pub enum ScopeOrigin {
    Root,
    Module(ModuleId),
    Block(BlockId),
    Function(FunctionId),
}
```

**`ScopeDefItem`** is the union of all things a name can resolve to:

```rust
pub enum ScopeDefItem {
    ModuleId(ModuleId), BlockId(BlockId),
    NatureId(NatureId), NatureAccess(NatureAccess), DisciplineId(DisciplineId),
    NodeId(NodeId), VarId(VarId), ParamId(ParamId), AliasParamId(AliasParamId),
    BranchId(BranchId), FunctionId(FunctionId),
    ParamSysFun(ParamSysFun),     // $mfactor, $vflip, etc.
    BuiltIn(BuiltIn),             // abs, sin, V, I, …
    FunctionReturn(FunctionId),   // the implicit return variable of an analog function
    FunctionArgId(FunctionArgId),
    NatureAttrId(NatureAttrId),
}
```

`NatureAccess(NatureAttrId)` deserves a note: in Verilog-A, each nature defines an *access function* (e.g., `Voltage` defines `V`, `Current` defines `I`). OpenVAF represents this as a `NatureAccess` in the scope, pointing at the nature attribute that records the access name. This is how `V(a,b)` and `I(a,b)` resolve.

### `DefCollector` and scope building

`nameres/collect.rs` provides three entry points:

- `collect_root_def_map(db, file)` — walks `tree.top_level`, creating child scopes for each Module, Nature, and Discipline. Inside each module scope it registers nodes, parameters, variables, branches, and functions as declarations.
- `collect_function_map(db, fun)` — builds the function's own scope with its arguments and local variables.
- `collect_block_map(db, block)` — builds a named block's scope. Returns `None` if the block has no declarations (no scope needed).

### Builtin scope

```rust
static BUILTIN_SCOPE: Lazy<IndexMap<Name, ScopeDefItem>> = Lazy::new(|| {
    let mut scope = IndexMap::default();
    insert_builtin_scope(&mut scope);
    scope
});
```

The builtin scope is initialized once (lazily) and contains all Verilog-A built-in functions (`BuiltIn` variants: `abs`, `sin`, `exp`, `ln`, `V`, `I`, …) and system parameters (`ParamSysFun` variants: `$mfactor`, `$vflip`, etc.). During name resolution, when a lookup exhausts all parent scopes, the collector falls back to `BUILTIN_SCOPE`.

---

## 7. `ScopeId` and Path Resolution

```rust
pub struct ScopeId {
    pub root_file: FileId,
    pub local_scope: LocalScopeId,
    pub src: DefMapSource,
}
```

`ScopeId` is a portable scope reference that can be stored inside `Body` (in `stmt_scopes`) without holding a reference to the `DefMap` itself. Given a `db`, `ScopeId::def_map(db)` reconstructs the owning `DefMap` by dispatching on `src`.

### Path resolution

`ScopeId::resolve_path(db, path)` dispatches:

- If `path.is_root_path` (written `::name` in Verilog-A): look up in the root `def_map` of the file, bypassing the local scope hierarchy.
- Otherwise: delegate to `def_map.resolve_normal_path_in_scope(local_scope, segments, db)`.

`resolve_normal_path_in_scope` walks up the parent chain within the `DefMap`, checking `scope.declarations` at each level. If the root of the `DefMap` is reached without a match, the search *crosses* into the parent `DefMap`:

- For a block scope (`DefMapSource::Block(block)`): continues into the enclosing function or module scope via `block.parent.def_map(db)`.
- For a function scope (`DefMapSource::Function`): escalates to the module's root `DefMap`.

Multi-segment paths (`nature.attr`) resolve the first segment to a `ScopeDefItem` that has children (a `ModuleId` or `NatureId`), then step into the child scope for the remaining segments.

`resolve_item_path<T: ScopeDefItemKind>` is the typed variant: it calls `resolve_path` and then tries to downcast to `T`, producing a `PathResolveError::ExpectedItemKind` on mismatch.

---

## 8. `DefWithBodyId` and `Body`

### `DefWithBodyId`

```rust
pub enum DefWithBodyId {
    ParamId(ParamId),
    ModuleId { initial: bool, module: ModuleId },
    FunctionId(FunctionId),
    VarId(VarId),
    NatureAttrId(NatureAttrId),
    DisciplineAttrId(DisciplineAttrId),
}
```

This enum enumerates every definition that has an expression body. `ModuleId { initial: false, module }` selects the regular `analog begin ... end` block; `initial: true` selects the `analog initial begin ... end` block. The body query dispatches on `initial` to call `ast.analog_behaviour()` vs `ast.analog_initial_behaviour()` on the AST node.

### `Body`

```rust
pub struct Body {
    pub exprs: Arena<Expr>,
    pub stmt_scopes: ArenaMap<Stmt, ScopeId>,
    pub stmts: Arena<Stmt>,
    pub entry_stmts: Box<[StmtId]>,
}
```

`exprs` and `stmts` are flat arenas. All `ExprId` and `StmtId` values are valid indices into these arenas. `entry_stmts` holds the top-level statement IDs — for a module analog block these are the statements directly inside `analog begin ... end`.

`stmt_scopes` maps each statement to the `ScopeId` that was active when the statement was lowered. This is essential for `hir_ty`: when it resolves an expression inside a named block, it needs to know which `DefMap` to use.

### `BodySourceMap`

```rust
pub struct BodySourceMap {
    pub expr_map: HashMap<AstPtr<ast::Expr>, ExprId>,
    pub expr_map_back: ArenaMap<Expr, Option<AstPtr<ast::Expr>>>,
    pub stmt_map: HashMap<AstPtr<ast::Stmt>, StmtId>,
    pub stmt_map_back: ArenaMap<Stmt, Option<AstPtr<ast::Stmt>>>,
    lint_map: ArenaMap<Stmt, LintAttrs>,
    pub diagnostics: Vec<AttrDiagnostic>,
}
```

The source map provides bidirectional provenance between `Body` arenas and the CST. `expr_map_back[expr_id]` gives the `AstPtr` of the original AST expression, used by diagnostics and IDE features to map an error back to a source location.

---

## 9. `Expr` and `Stmt`

`Body` uses an unresolved expression representation: `Path` values are sequences of `Name`s, not resolved `ScopeDefItem`s. Resolution happens in `hir_ty`.

### `Expr`

```rust
pub enum Expr {
    Missing,
    Path { path: Path, port: bool },
    BinaryOp { lhs: ExprId, rhs: ExprId, op: Option<BinaryOp> },
    UnaryOp { expr: ExprId, op: UnaryOp },
    Select { cond: ExprId, then_val: ExprId, else_val: ExprId },
    Call { fun: Option<Path>, args: Vec<ExprId> },
    Array(Vec<ExprId>),
    Literal(Literal),
}
```

`Missing` is the error-recovery node; it arises when the parser could not produce a valid expression for a required position.

`Path { port: bool }` — the `port` flag distinguishes a port reference `<port>` from an ordinary name reference, as they are syntactically different in Verilog-A.

`Call { fun: Option<Path>, args }` — both user-defined analog functions and built-in access functions (`V(a,b)`, `I(a)`) are represented the same way here. Resolution in `hir_ty` distinguishes them.

`Literal` variants:

| Variant | Type |
|---|---|
| `String(Box<str>)` | string literal |
| `Int(i32)` | integer literal |
| `Float(Ieee64)` | real literal (bit-preserving IEEE 754 wrapper) |
| `Inf` | the keyword `inf` |

### `Stmt`

```rust
pub enum Stmt {
    Missing,
    Empty,
    Expr(ExprId),
    EventControl { event: Event, body: StmtId },
    Assignment { dst: ExprId, val: ExprId, assignment_kind: ast::AssignOp },
    Block { body: Vec<StmtId> },
    If { cond: ExprId, then_branch: StmtId, else_branch: StmtId },
    ForLoop { init: StmtId, cond: ExprId, incr: StmtId, body: StmtId },
    WhileLoop { cond: ExprId, body: StmtId },
    Case { discr: ExprId, case_arms: Vec<Case> },
}
```

`Assignment` covers both regular assignments (`=`) and Verilog-A contribution statements (`<+`); the distinction is captured in `assignment_kind: ast::AssignOp`. Downstream (`hir_ty`, `hir_lower`), contribution statements become branch current/voltage contributions.

`EventControl { event, body }` represents `@(initial_step) begin ... end`. The `Event` enum currently has one non-exhaustive variant: `Event::Global { kind: GlobalEvent, phases: Vec<String> }` where `GlobalEvent` is `InitialStep` or `FinalStep`.

Both `Expr` and `Stmt` provide `walk_child_exprs` and `walk_child_stmts` helper methods that call a closure on all direct child IDs, enabling traversal without pattern-matching the full enum.

---

## 10. `*Data` Queries

The `data.rs` module provides a second query layer above `ItemTree`. Each `*Data` struct contains the same information as the corresponding `ItemTree` item but with:

- Index indirections resolved (e.g., `IdxRange<NatureAttr>` expanded into `Arena<NatureAttrData>`)
- Self-contained values (no need to hold an `Arc<ItemTree>` reference)

`DisciplineData` is the most frequently used:

```rust
pub struct DisciplineData {
    pub name: Name,
    pub potential: Option<NatureRef>,
    pub flow: Option<NatureRef>,
    pub domain: Option<Domain>,
    pub attrs: Arena<DisciplineAttrData>,
}
```

`DisciplineData::compatible(self, other)` is a key predicate: two nodes can be connected only if their disciplines are compatible. Compatibility holds if either discipline has an unspecified domain, or if both have no natures declared (abstract discipline), or if both share identical `potential`, `flow`, and `domain`.

`NatureRef { name: Name, kind: NatureRefKind }` — used inside `DisciplineData` to refer to a nature by name and kind (`Nature`, `DisciplinePotential`, `DisciplineFlow`). The actual `NatureId` is resolved later by `hir_ty`.

Other `*Data` types follow the same pattern (query key → `Arc<Data>`):

| Query | Struct | Notable fields |
|---|---|---|
| `var_data(VarId)` | `VarData` | `name`, `ty: Type` |
| `param_data(ParamId)` | `ParamData` | `name`, `ty: Option<Type>` |
| `node_data(NodeId)` | `NodeData` | `name`, `is_port`, `discipline: Option<Name>` |
| `branch_data(BranchId)` | `BranchData` | `name`, `kind: BranchKind` (paths resolved) |
| `function_data(FunctionId)` | `FunctionData` | `name`, `ty`, `args` |
| `module_data(ModuleId)` | `ModuleData` | `name`, `nodes`, `ports` |
| `alias_data(AliasParamId)` | `AliasParamData` | `name`, `src: Option<Path>` |

---

## 11. Worked Example — `resistor.va`

Starting from the resistor source:

```verilog
`include "disciplines.vams"
module resistor (p, n);
  inout p, n;
  electrical p, n;
  parameter real R = 1e3 from (0:inf);
  analog begin
    V(p,n) <+ R * I(p,n);
  end
endmodule
```

### Step 1 — `ItemTree`

`item_tree(resistor.va)` produces (simplified):

```
top_level: [RootItem::Module(idx=0)]

data.modules[0] = Module {
    name: "resistor",
    nodes: [
        Node { name: "p", is_port: true, decls: [Port(port_p), Net(net_p)] },
        Node { name: "n", is_port: true, decls: [Port(port_n), Net(net_n)] },
    ],
    num_ports: 2,
    items: [Node(0), Node(1), Parameter(idx_R)],
    ast_id: AstId<ModuleDecl>(…),
}

data.parameters[idx_R] = Param {
    name: "R",
    ty: Some(Type::Real),
    is_local: false,
    ast_id: AstId<Param>(…),
}
```

The analog block body (`V(p,n) <+ R * I(p,n)`) is *not* present in the `ItemTree`.

### Step 2 — `DefMap`

`def_map(resistor.va)` produces:

```
scope[0] (root, origin=Root):
  children: { "resistor" → scope[1] }
  declarations: (Natures and Disciplines from included file, built-ins via BUILTIN_SCOPE)

scope[1] (module, origin=Module(module_id)):
  parent: scope[0]
  children: {}
  declarations: {
    "p"  → NodeId(node_p),
    "n"  → NodeId(node_n),
    "R"  → ParamId(param_R),
    "V"  → NatureAccess(voltage_access_attr_id),
    "I"  → NatureAccess(current_access_attr_id),
  }
```

`V` and `I` appear as `NatureAccess` because the included `disciplines.vams` defines the `electrical` discipline with `Voltage` and `Current` natures, and those natures declare access functions `V` and `I` respectively.

### Step 3 — `Body`

`body(DefWithBodyId::ModuleId { initial: false, module: module_id })` produces:

```
entry_stmts: [stmt_0]

stmts[stmt_0] = Stmt::Assignment {
    dst:  expr_0,
    val:  expr_1,
    assignment_kind: AssignOp::Contribute,   // <+
}

exprs[expr_0] = Expr::Call {
    fun:  Some(Path { segments: ["V"], is_root_path: false }),
    args: [expr_2, expr_3],
}
exprs[expr_2] = Expr::Path { path: Path { segments: ["p"] }, port: false }
exprs[expr_3] = Expr::Path { path: Path { segments: ["n"] }, port: false }

exprs[expr_1] = Expr::BinaryOp {
    lhs: expr_4,
    rhs: expr_5,
    op:  Some(BinaryOp::Mul),
}
exprs[expr_4] = Expr::Path { path: Path { segments: ["R"] }, port: false }

exprs[expr_5] = Expr::Call {
    fun:  Some(Path { segments: ["I"], is_root_path: false }),
    args: [expr_6, expr_7],
}
exprs[expr_6] = Expr::Path { path: Path { segments: ["p"] }, port: false }
exprs[expr_7] = Expr::Path { path: Path { segments: ["n"] }, port: false }
```

All paths are still unresolved strings at this stage. `hir_ty` will resolve `"V"` → `NatureAccess(voltage_access_attr_id)`, `"R"` → `ParamId(param_R)`, `"p"` → `NodeId(node_p)`, and so on.

The `BodySourceMap` records the `AstPtr` for each `ExprId` and `StmtId`, so that if type checking later rejects the `R * I(p,n)` expression, the diagnostic can point at the exact source range.
