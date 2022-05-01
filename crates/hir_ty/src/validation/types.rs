use std::iter::once;

use basedb::{AstId, ErasedAstId, FileId};
use hir_def::nameres::diagnostics::PathResolveError;
use hir_def::nameres::{DefMap, ScopeDefItem};
use hir_def::{
    AliasParamId, BranchId, BranchKind, DisciplineId, ItemTree, LocalDisciplineAttrId,
    LocalNatureAttrId, Lookup, ModuleId, ModuleLoc, NatureId, NodeId, NodeTypeDecl,
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
    PathError { err: PathResolveError, src: SyntaxNodePtr },
    DuplicateDisciplineAttr(DuplicateItem<LocalDisciplineAttrId, DisciplineId>),
    DuplicateNatureAttr(DuplicateItem<LocalNatureAttrId, NatureId>),
    MultipleDirections(DuplicateItem<AstId<ast::PortDecl>, NodeId>),
    MultipleDisciplines(DuplicateItem<ErasedAstId, NodeId>),
    MultipleGnds(DuplicateItem<ErasedAstId, NodeId>),
    PortWithoutDirection { decl: ErasedAstId, name: Name },
    NodeWithoutDiscipline { decl: ErasedAstId, name: Name },
    ExpectedPort { node: NodeId, src: ErasedAstId },
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
        let scope = loc.scope.local_scope;
        for item in self.def_map[scope].declarations.values() {
            match item {
                ScopeDefItem::NodeId(node) => self.verify_node(*node, loc),
                ScopeDefItem::BranchId(branch) => self.verify_branch(*branch),
                ScopeDefItem::AliasParamId(alias) => self.verify_alias(*alias),
                _ => (),
            }
        }
    }

    fn verify_alias(&mut self, alias: AliasParamId) {
        if self.db.resolve_alias(alias).is_none() {
            let loc = alias.lookup(self.db.upcast());
            let data = self.db.alias_data(alias);
            if let Some(src) = data.src.as_ref() {
                if let Result::Err(err) = loc.scope.resolve_path(self.db.upcast(), src) {
                    let src =
                        SyntaxNodePtr::new(loc.source(self.db.upcast()).src().unwrap().syntax());
                    self.dst.push(TypeValidationDiagnostic::PathError { err, src })
                }
            }
        }
    }

    fn verify_branch(&mut self, branch: BranchId) {
        let branch_data = self.db.branch_data(branch);
        let kind = &branch_data.kind;
        let branch = branch.lookup(self.db.upcast());
        let scope = branch.scope;
        match kind {
            BranchKind::PortFlow(port) => {
                match scope.resolve_item_path::<NodeId>(self.db.upcast(), port) {
                    Ok(node) => {
                        let node_ = self.db.node_data(node);
                        if !node_.is_input && !node_.is_output {
                            let src = branch.ast_id(self.db.upcast()).into();
                            self.dst.push(TypeValidationDiagnostic::ExpectedPort { node, src });
                        }
                    }
                    Err(err) => {
                        let src = SyntaxNodePtr::new(
                            branch
                                .source(self.db.upcast())
                                .arg_list()
                                .unwrap()
                                .args()
                                .next()
                                .unwrap()
                                .syntax(),
                        );
                        self.dst.push(TypeValidationDiagnostic::PathError { err, src });
                    }
                }
            }
            BranchKind::NodeGnd(node) => {
                if let Err(err) = scope.resolve_item_path::<NodeId>(self.db.upcast(), node) {
                    let src = SyntaxNodePtr::new(
                        branch
                            .source(self.db.upcast())
                            .arg_list()
                            .unwrap()
                            .args()
                            .next()
                            .unwrap()
                            .syntax(),
                    );
                    self.dst.push(TypeValidationDiagnostic::PathError { err, src })
                }
            }
            BranchKind::Nodes(node1, node2) => {
                if let Err(err) = scope.resolve_item_path::<NodeId>(self.db.upcast(), node1) {
                    let src = SyntaxNodePtr::new(
                        branch
                            .source(self.db.upcast())
                            .arg_list()
                            .unwrap()
                            .args()
                            .next()
                            .unwrap()
                            .syntax(),
                    );
                    self.dst.push(TypeValidationDiagnostic::PathError { err, src })
                }

                if let Err(err) = scope.resolve_item_path::<NodeId>(self.db.upcast(), node2) {
                    let src = SyntaxNodePtr::new(
                        branch
                            .source(self.db.upcast())
                            .arg_list()
                            .unwrap()
                            .args()
                            .nth(1)
                            .unwrap()
                            .syntax(),
                    );
                    self.dst.push(TypeValidationDiagnostic::PathError { err, src })
                }
            }
            BranchKind::Missing => (),
        };

        // TODO discipline compatability
    }

    fn verify_node(&mut self, node: NodeId, module: ModuleLoc) {
        let loc = node.lookup(self.db.upcast());
        let node_ = &self.tree[module.id].nodes[loc.id];
        if node_.decls.is_empty() {
            self.dst.push(TypeValidationDiagnostic::PortWithoutDirection {
                decl: node_.ast_id,
                name: node_.name.clone(),
            });
            self.dst.push(TypeValidationDiagnostic::NodeWithoutDiscipline {
                decl: node_.ast_id,
                name: node_.name.clone(),
            });
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
                self.dst.push(TypeValidationDiagnostic::MultipleDirections(DuplicateItem {
                    src: node,
                    first,
                    subsequent: duplicates,
                }))
            }
        } else if node_.decls[0].ast_id(self.tree) != node_.ast_id {
            self.dst.push(TypeValidationDiagnostic::PortWithoutDirection {
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
                    self.dst.push(TypeValidationDiagnostic::PathError {
                        err,
                        src: SyntaxNodePtr::new(
                            decl.discipline_src(self.db.upcast(), self.root_file).unwrap().syntax(),
                        ),
                    })
                }
            }

            let duplicates: Vec<_> = disciplines.map(|(decl, _)| decl.ast_id(self.tree)).collect();
            if !duplicates.is_empty() {
                self.dst.push(TypeValidationDiagnostic::MultipleDisciplines(DuplicateItem {
                    src: node,
                    first: decl.ast_id(self.tree),
                    subsequent: duplicates,
                }))
            }
        } else {
            self.dst.push(TypeValidationDiagnostic::NodeWithoutDiscipline {
                decl: node_.ast_id,
                name: node_.name.clone(),
            });
        }

        let mut gnd_declarations = node_.decls.iter().filter(|it| it.is_gnd(self.tree));

        if let Some(first) = gnd_declarations.next() {
            let duplicates: Vec<_> = gnd_declarations.map(|it| it.ast_id(self.tree)).collect();
            if !duplicates.is_empty() {
                self.dst.push(TypeValidationDiagnostic::MultipleDisciplines(DuplicateItem {
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
                self.dst.push(wrap_err(err))
            }
        }
    }
}
