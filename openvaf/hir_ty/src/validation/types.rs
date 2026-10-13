use std::iter::once;

use basedb::{AstId, ErasedAstId, FileId};
use hir_def::db::HirDefDB;
use hir_def::nameres::diagnostics::PathResolveError;
use hir_def::nameres::{DefMap, ScopeDefItem};
use hir_def::{
    AliasParamId, Branch, BranchId, BranchKind, DefWithBodyId, DisciplineId, ItemLoc, ItemTree,
    LocalDisciplineAttrId, LocalNatureAttrId, Lookup, ModuleBodyKind, ModuleId, ModuleLoc,
    NatureId, NodeId, NodeTypeDecl, ParamId, Path, ScopeId, Stmt, Type,
};
use syntax::ast::ArgListOwner;
use syntax::name::Name;
use syntax::{ast, AstNode, SyntaxNodePtr};
use typed_index_collections::TiSlice;

use crate::db::HirTyDB;

#[derive(PartialEq, Eq, Clone, Debug)]
pub struct DuplicateItem<Item, Def> {
    pub src: Def,
    pub first: Item,
    pub subsequent: Vec<Item>,
}

#[derive(PartialEq, Eq, Clone, Debug)]
pub enum TypeValidationDiagnostic {
    PathError {
        err: PathResolveError,
        src: SyntaxNodePtr,
    },
    DuplicateDisciplineAttr(DuplicateItem<LocalDisciplineAttrId, DisciplineId>),
    DuplicateNatureAttr(DuplicateItem<LocalNatureAttrId, NatureId>),
    MultipleDirections(DuplicateItem<AstId<ast::PortDecl>, NodeId>),
    MultipleDisciplines(DuplicateItem<ErasedAstId, NodeId>),
    MultipleGnds(DuplicateItem<ErasedAstId, NodeId>),
    PortWithoutDirection {
        decl: ErasedAstId,
        name: Name,
    },
    NodeWithoutDiscipline {
        decl: ErasedAstId,
        name: Name,
    },
    ArrayParamWithoutType {
        decl: ErasedAstId,
        name: Name,
    },
    UnknownDefaultDiscipline {
        decl: ErasedAstId,
        name: Name,
        discipline: Name,
    },
    ExpectedPort {
        node: NodeId,
        src: ErasedAstId,
    },
    IncompatibleBranch {
        branch: BranchId,
        node1: NodeId,
        node2: NodeId,
    },

    /// A `.name = expr;` statement in a module, which has no paramset to belong to.
    StrayParamsetStmt {
        src: ErasedAstId,
    },
    /// Something a paramset may not declare (VAMS-2023 6.4).
    ParamsetItem {
        src: SyntaxNodePtr,
        kind: ParamsetItemKind,
    },
    /// The name after a paramset's own is not a module of this file.
    ParamsetUnknownTarget {
        decl: SyntaxNodePtr,
        name: Name,
        target: Name,
    },
    /// 6.4's other form: the second identifier is a second paramset.
    ParamsetChain {
        decl: SyntaxNodePtr,
        name: Name,
        target: Name,
    },
    /// A statement sets something that is not a parameter of the target module.
    ParamsetUnknownParam {
        src: ErasedAstId,
        target: Name,
        param: Name,
    },
    /// A statement sets a `localparam` of the target module, which nothing outside
    /// it may set.
    ParamsetLocalParam {
        src: ErasedAstId,
        target: Name,
        param: Name,
    },
    /// Two statements of one paramset set the same parameter.
    DuplicateParamsetStmt(DuplicateItem<ErasedAstId, Name>),
    /// A statement's value cannot be the parameter's.
    ParamsetTypeMismatch {
        src: ErasedAstId,
        param: Name,
        expected: Type,
        found: Type,
    },
}

/// What a paramset may not hold. VAMS-2023 6.4 gives it parameter declarations and
/// statements; the behaviour, the nodes and everything that reads them belong to
/// the module it names.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum ParamsetItemKind {
    Net,
    Port,
    Branch,
    Function,
    Event,
    Variable,
    Behaviour,
}

impl ParamsetItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ParamsetItemKind::Net => "a net",
            ParamsetItemKind::Port => "a port",
            ParamsetItemKind::Branch => "a branch",
            ParamsetItemKind::Function => "a function",
            ParamsetItemKind::Event => "an event",
            ParamsetItemKind::Variable => "a variable",
            ParamsetItemKind::Behaviour => "behaviour",
        }
    }
}

impl TypeValidationDiagnostic {
    pub fn collect(db: &dyn HirTyDB, root_file: FileId) -> Vec<TypeValidationDiagnostic> {
        let mut res = Vec::new();

        let def_map = db.def_map(root_file);
        let tree = db.item_tree(root_file);
        TypeValidationCtx { db, dst: &mut res, def_map: &def_map, tree: &tree, root_file }
            .validate();

        res
    }
}

struct TypeValidationCtx<'a> {
    db: &'a dyn HirTyDB,
    dst: &'a mut Vec<TypeValidationDiagnostic>,
    def_map: &'a DefMap,
    tree: &'a ItemTree,
    root_file: FileId,
}

impl TypeValidationCtx<'_> {
    fn validate(&mut self) {
        let root = &self.def_map[self.def_map.root()];
        for def in root.declarations.values() {
            match *def {
                ScopeDefItem::NatureId(nature) => self.verify_nature(nature),
                ScopeDefItem::DisciplineId(discipline) => self.verify_discipline(discipline),
                ScopeDefItem::ModuleId(module) => self.verify_module(module),
                _ => (),
            }
        }
    }

    fn verify_module(&mut self, module: ModuleId) {
        let loc = module.lookup(self.db.upcast());
        // A paramset shares this item (VAMS-2023 6.4), and what can be said about
        // its contents is said in one place.
        let is_paramset = self.tree[loc.id].paramset.is_some();
        let scope = loc.scope.local_scope;
        for item in self.def_map[scope].declarations.values() {
            match item {
                // Reported as something a paramset may not declare instead.
                ScopeDefItem::NodeId(_) | ScopeDefItem::BranchId(_) if is_paramset => (),
                ScopeDefItem::NodeId(node) => self.verify_node(*node, loc),
                ScopeDefItem::BranchId(branch) => self.verify_branch(*branch),
                ScopeDefItem::AliasParamId(alias) => self.verify_alias(*alias),
                ScopeDefItem::ParamId(param) => self.verify_param(*param),
                _ => (),
            }
        }
        if is_paramset {
            self.verify_paramset(module, loc);
        } else {
            // `.name = expr;` sets a parameter of the module a paramset names, so
            // in a module it names nothing.
            for stmt in &*self.tree[loc.id].paramset_stmts {
                self.report(TypeValidationDiagnostic::StrayParamsetStmt {
                    src: stmt.ast_id.erased(),
                });
            }
        }
    }

    /// VAMS-2023 6.4: a paramset collects parameter values for a module, so what it
    /// holds is parameter declarations and statements that set that module's
    /// parameters. Everything else belongs to the module it names.
    fn verify_paramset(&mut self, module: ModuleId, loc: ModuleLoc) {
        let decl = &self.tree[loc.id];
        let name = decl.name.clone();
        let target_name = decl.paramset.clone().expect("called for a paramset only");
        let stmts = decl.paramset_stmts.clone();
        let ast = loc.source(self.db.upcast());
        // The target name, or the declaration when the parser never got one.
        let decl_src = match ast.target() {
            Some(target) => SyntaxNodePtr::new(target.syntax()),
            None => SyntaxNodePtr::new(ast.syntax()),
        };

        for item in ast.module_items() {
            let kind = match item {
                ast::ModuleItem::ParamDecl(_)
                | ast::ModuleItem::AliasParam(_)
                | ast::ModuleItem::ParamsetAssign(_) => continue,
                ast::ModuleItem::NetDecl(_) => ParamsetItemKind::Net,
                ast::ModuleItem::BodyPortDecl(_) => ParamsetItemKind::Port,
                ast::ModuleItem::BranchDecl(_) => ParamsetItemKind::Branch,
                ast::ModuleItem::Function(_) => ParamsetItemKind::Function,
                ast::ModuleItem::EventDecl(_) | ast::ModuleItem::GenvarDecl(_) => {
                    ParamsetItemKind::Event
                }
                ast::ModuleItem::VarDecl(_) => ParamsetItemKind::Variable,
                ast::ModuleItem::AnalogBehaviour(_) | ast::ModuleItem::ProceduralBlock(_) => {
                    ParamsetItemKind::Behaviour
                }
            };
            let src = SyntaxNodePtr::new(item.syntax());
            self.report(TypeValidationDiagnostic::ParamsetItem { src, kind });
        }

        let root = self.def_map.root();
        let target = match self.def_map[root].declarations.get(&target_name) {
            Some(ScopeDefItem::ModuleId(target)) => *target,
            _ => {
                self.report(TypeValidationDiagnostic::ParamsetUnknownTarget {
                    decl: decl_src.clone(),
                    name,
                    target: target_name,
                });
                return;
            }
        };
        let target_loc = target.lookup(self.db.upcast());
        if self.tree[target_loc.id].paramset.is_some() {
            self.report(TypeValidationDiagnostic::ParamsetChain {
                decl: decl_src,
                name,
                target: target_name,
            });
            return;
        }

        let def = DefWithBodyId::ModuleId { kind: ModuleBodyKind::Paramset, module };
        let def_db: &dyn HirDefDB = self.db.upcast();
        let body = def_db.body(def);
        let infer = self.db.inference_result(def);
        let target_scope = target_loc.scope.local_scope;

        // One statement per parameter, each of them a settable parameter of the
        // target, each value the type that parameter holds.
        let mut seen: Vec<(Name, DuplicateItem<ErasedAstId, Name>)> = Vec::new();
        for (i, stmt) in stmts.iter().enumerate() {
            let src = stmt.ast_id.erased();
            match seen.iter_mut().find(|(name, _)| *name == stmt.param) {
                Some((_, dup)) => {
                    dup.subsequent.push(src);
                    continue;
                }
                None => seen.push((
                    stmt.param.clone(),
                    DuplicateItem { src: stmt.param.clone(), first: src, subsequent: Vec::new() },
                )),
            }

            let param = match self.def_map[target_scope].declarations.get(&stmt.param) {
                Some(ScopeDefItem::ParamId(param)) => *param,
                _ => {
                    self.report(TypeValidationDiagnostic::ParamsetUnknownParam {
                        src,
                        target: target_name.clone(),
                        param: stmt.param.clone(),
                    });
                    continue;
                }
            };
            if def_db.param_data(param).is_local {
                self.report(TypeValidationDiagnostic::ParamsetLocalParam {
                    src,
                    target: target_name.clone(),
                    param: stmt.param.clone(),
                });
                continue;
            }

            // The conversions an assignment to the parameter would do, which is
            // where an integer becomes a real but a string never becomes either.
            let Stmt::Expr(expr) = body.stmts[body.entry_stmts[i]] else { continue };
            let Some(found) = infer.expr_types[expr].to_value() else { continue };
            let expected = self.db.param_ty(param);
            if !found.is_convertible_to(&expected) {
                self.report(TypeValidationDiagnostic::ParamsetTypeMismatch {
                    src,
                    param: stmt.param.clone(),
                    expected,
                    found,
                });
            }
        }

        for (_, dup) in seen {
            if !dup.subsequent.is_empty() {
                self.report(TypeValidationDiagnostic::DuplicateParamsetStmt(dup));
            }
        }
    }

    fn resolve_node(
        &mut self,
        node: &Path,
        scope: ScopeId,
        branch: &ItemLoc<Branch>,
    ) -> Option<NodeId> {
        let node = scope.resolve_item_path::<NodeId>(self.db.upcast(), node);
        match node {
            Ok(node) => Some(node),
            Err(err) => {
                let arg =
                    branch.source(self.db.upcast()).arg_list().unwrap().args().next().unwrap();
                // Point at the node itself, not at the argument wrapper, so the
                // diagnostic keeps highlighting just the offending node name.
                let node = match arg.expr() {
                    Some(expr) => expr.syntax().clone(),
                    None => arg.syntax().clone(),
                };
                let src = SyntaxNodePtr::new(&node);
                self.report(TypeValidationDiagnostic::PathError { err, src });
                None
            }
        }
    }

    /// VAMS-2023 3.4.1: "the type of a string parameter (see 3.4.6) and any of the
    /// array parameters (see 3.4.4) is mandatory". A parameter's type is otherwise
    /// taken from its value, and an assignment pattern says nothing about what its
    /// elements hold.
    fn verify_param(&mut self, param: ParamId) {
        let data = self.db.param_data(param);
        if data.array_without_ty {
            let decl = param.lookup(self.db.upcast()).ast_id(self.db.upcast()).erased();
            self.report(TypeValidationDiagnostic::ArrayParamWithoutType {
                decl,
                name: data.name.clone(),
            });
        }
    }

    fn verify_alias(&mut self, alias: AliasParamId) {
        if self.db.resolve_alias(alias).is_none() {
            let loc = alias.lookup(self.db.upcast());
            let data = self.db.alias_data(alias);
            if let Some(path) = data.src.as_ref() {
                match loc.scope.resolve_path(self.db.upcast(), path) {
                    // TODO: better errors for cycels
                    Ok(found) => {
                        let src = SyntaxNodePtr::new(
                            loc.source(self.db.upcast()).src().unwrap().syntax(),
                        );
                        self.report(TypeValidationDiagnostic::PathError {
                            err: PathResolveError::ExpectedItemKind {
                                name: path.segments.last().unwrap().clone(),
                                expected: "parameter",
                                found,
                            },
                            src,
                        })
                    }
                    Err(err) => {
                        let src = SyntaxNodePtr::new(
                            loc.source(self.db.upcast()).src().unwrap().syntax(),
                        );
                        self.report(TypeValidationDiagnostic::PathError { err, src })
                    }
                }
            }
        }
    }

    fn report(&mut self, diag: impl Into<TypeValidationDiagnostic>) {
        self.dst.push(diag.into())
    }

    fn verify_branch(&mut self, branch_: BranchId) {
        let branch_data = self.db.branch_data(branch_);
        let kind = &branch_data.kind;
        let branch = branch_.lookup(self.db.upcast());
        let scope = branch.scope;
        match kind {
            BranchKind::PortFlow(port) => {
                if let Some(node) = self.resolve_node(port, scope, &branch) {
                    let node_ = self.db.node_data(node);
                    if !node_.is_input && !node_.is_output {
                        let src = branch.ast_id(self.db.upcast()).into();
                        self.report(TypeValidationDiagnostic::ExpectedPort { node, src });
                    }
                }
            }
            BranchKind::NodeGnd(node) => {
                self.resolve_node(node, scope, &branch);
            }
            BranchKind::Nodes(node1, node2) => {
                let node1 = self.resolve_node(node1, scope, &branch);
                let node2 = self.resolve_node(node2, scope, &branch);
                let (node1, node2) = if let (Some(node1), Some(node2)) = (node1, node2) {
                    (node1, node2)
                } else {
                    return;
                };

                let discipline1 = self.db.node_discipline(node1);
                let discipline2 = self.db.node_discipline(node2);
                // fast path
                if discipline1 == discipline2 {
                    return;
                }

                let (discipline1, discipline2) =
                    if let (Some(d1), Some(d2)) = (discipline1, discipline2) {
                        (d1, d2)
                    } else {
                        return;
                    };

                if !self.db.discipline_info(discipline1).compatible(discipline2, self.db) {
                    self.report(TypeValidationDiagnostic::IncompatibleBranch {
                        branch: branch_,
                        node1,
                        node2,
                    })
                }
            }
            BranchKind::Missing => (),
        };
    }

    /// Whether `` `default_discipline `` (VAMS-2023 10.2) supplies the discipline
    /// a net was declared without, which is the same question
    /// [`NodeData`](hir_def::NodeData) asks when it resolves one. What the
    /// directive names still has to be a discipline, and this is where that is
    /// checked: the directive is read long before any discipline is declared.
    fn has_default_discipline(&mut self, decl: ErasedAstId, name: &Name) -> bool {
        let Some(default) =
            hir_def::directives::default_discipline(self.db.upcast(), self.root_file, decl)
        else {
            return false;
        };

        let discipline = Name::resolve(&default.name);
        if self
            .def_map
            .resolve_local_item_in_scope::<DisciplineId>(self.def_map.root(), &discipline)
            .is_err()
        {
            self.report(TypeValidationDiagnostic::UnknownDefaultDiscipline {
                decl,
                name: name.clone(),
                discipline,
            })
        }

        true
    }

    fn verify_node(&mut self, node: NodeId, module: ModuleLoc) {
        let loc = node.lookup(self.db.upcast());
        let node_ = &self.tree[module.id].nodes[loc.id];
        if node_.decls.is_empty() {
            self.report(TypeValidationDiagnostic::PortWithoutDirection {
                decl: node_.ast_id,
                name: node_.name.clone(),
            });
            if !self.has_default_discipline(node_.ast_id, &node_.name) {
                self.report(TypeValidationDiagnostic::NodeWithoutDiscipline {
                    decl: node_.ast_id,
                    name: node_.name.clone(),
                });
            }
            return; // Do not print other diagnostics here would just lead to duplications
        }
        let mut directions = node_.decls.iter().filter_map(|decl| {
            if let NodeTypeDecl::Port(p) = decl {
                Some(self.tree[*p].ast_id)
            } else {
                None
            }
        });
        if let Some(first) = directions.next() {
            let duplicates: Vec<_> = directions.collect();
            if !duplicates.is_empty() {
                self.report(TypeValidationDiagnostic::MultipleDirections(DuplicateItem {
                    src: node,
                    first,
                    subsequent: duplicates,
                }))
            }
        } else if node_.decls[0].ast_id(self.tree) != node_.ast_id {
            self.report(TypeValidationDiagnostic::PortWithoutDirection {
                decl: node_.ast_id,
                name: node_.name.clone(),
            })
        }

        let mut disciplines = node_
            .decls
            .iter()
            .filter_map(|it| it.discipline(self.tree).as_ref().map(|discipline| (it, discipline)));

        if let Some((decl, discipline)) = disciplines.next() {
            for (decl, discipline) in once((decl, discipline)).chain(disciplines.clone()) {
                if let Err(err) = self
                    .def_map
                    .resolve_local_item_in_scope::<DisciplineId>(self.def_map.root(), discipline)
                {
                    self.report(TypeValidationDiagnostic::PathError {
                        err,
                        src: SyntaxNodePtr::new(
                            decl.discipline_src(self.db.upcast(), self.root_file).unwrap().syntax(),
                        ),
                    })
                }
            }

            let duplicates: Vec<_> = disciplines.map(|(decl, _)| decl.ast_id(self.tree)).collect();
            if !duplicates.is_empty() {
                self.report(TypeValidationDiagnostic::MultipleDisciplines(DuplicateItem {
                    src: node,
                    first: decl.ast_id(self.tree),
                    subsequent: duplicates,
                }))
            }
        } else if !self.has_default_discipline(node_.ast_id, &node_.name) {
            self.report(TypeValidationDiagnostic::NodeWithoutDiscipline {
                decl: node_.ast_id,
                name: node_.name.clone(),
            });
        }

        let mut gnd_declarations = node_.decls.iter().filter(|it| it.is_gnd(self.tree));

        if let Some(first) = gnd_declarations.next() {
            let duplicates: Vec<_> = gnd_declarations.map(|it| it.ast_id(self.tree)).collect();
            if !duplicates.is_empty() {
                self.report(TypeValidationDiagnostic::MultipleDisciplines(DuplicateItem {
                    src: node,
                    first: first.ast_id(self.tree),
                    subsequent: duplicates,
                }))
            }
        }
    }

    // TODO check natures/discipline (~dspom/OpenVAF#1)
    fn verify_discipline(&mut self, discipline: DisciplineId) {
        // let info = self.db.discipline_info(discipline);
        let data = self.db.discipline_data(discipline);
        self.verify_unique_attributes(
            &data.attrs,
            discipline,
            TypeValidationDiagnostic::DuplicateDisciplineAttr,
        );
    }

    fn verify_nature(&mut self, nature: NatureId) {
        // let info = self.db.nature_info(nature);
        let data = self.db.nature_data(nature);

        self.verify_unique_attributes(
            &data.attrs,
            nature,
            TypeValidationDiagnostic::DuplicateNatureAttr,
        );
    }

    fn verify_unique_attributes<Attr: From<usize> + PartialEq, Def: Copy>(
        &mut self,
        attrs: &TiSlice<Attr, impl PartialEq>,
        def: Def,
        wrap_err: impl Fn(DuplicateItem<Attr, Def>) -> TypeValidationDiagnostic,
    ) {
        // This is quadratic (actually its n(n+1)/2). But disciplines and nature usually only have very few (below 5)
        // attributes so this is probably faster than allocating a HashMap. If this ever becomes a
        // problem just use a HashMap instead
        for (id, attr) in attrs.iter_enumerated() {
            let mut duplicates =
                attrs.iter_enumerated().filter_map(
                    |it| {
                        if it.1 == attr {
                            Some(it.0)
                        } else {
                            None
                        }
                    },
                );

            if duplicates.next().unwrap() != id {
                continue;
            }

            let duplicates: Vec<_> = duplicates.collect();

            if !duplicates.is_empty() {
                let err = DuplicateItem { src: def, first: id, subsequent: duplicates };
                self.report(wrap_err(err))
            }
        }
    }
}
