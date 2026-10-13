use std::hash::BuildHasherDefault;

use ahash::AHashSet;
use hir::diagnostics::{BaseDB, ConsoleSink, Diagnostic, FileId, Label, LabelStyle, Report};
use hir::{
    CompilationDB, CompilationUnit, DiagnosticSink, Module, ParamSysFun, Parameter,
    ResolvedAliasParameter, ScopeDef, Variable,
};
use hir_lower::ParamsetInit;
use indexmap::IndexMap;
use rustc_hash::FxHasher;
use smol_str::SmolStr;
use syntax::ast::{self, Expr};
use syntax::sourcemap::FileSpan;
use syntax::AstNode;

#[cfg(test)]
mod tests;

pub fn collect_modules(
    db: &CompilationDB,
    all_vars_opvars: bool,
    sink: &mut ConsoleSink,
) -> Option<Vec<ModuleInfo>> {
    let cu = db.compilation_unit();
    let name = cu.name(db);

    cu.diagnostics(db, sink);

    if sink.summary(&name) {
        return None;
    }

    // A paramset (VAMS-2023 6.4) shares the declaration a module uses, so the two
    // are told apart by whether it named a module to collect values for.
    let decls = cu.modules(db);
    let (paramsets, modules): (Vec<Module>, Vec<Module>) =
        decls.into_iter().partition(|decl| decl.paramset_target(db).is_some());

    let mut res: Vec<ModuleInfo> = modules
        .into_iter()
        .map(|module| ModuleInfo::collect(db, cu, module, sink, all_vars_opvars))
        .collect();

    // One descriptor per paramset, beside the module's own: the paramset is what a
    // netlist instantiates, and the module is still a module.
    let mut sets = Vec::with_capacity(paramsets.len());
    for decl in paramsets {
        if let Some(info) = ModuleInfo::collect_paramset(db, cu, decl, &res, sink, all_vars_opvars)
        {
            sets.push(info);
        }
    }
    res.extend(sets);

    if sink.summary(&name) {
        return None;
    }

    Some(res)
}

pub struct ModuleInfo {
    /// The module whose behaviour this unit compiles. For a paramset that is the
    /// module it names, not the paramset itself: "the paramset itself contains no
    /// behavioural code" (VAMS-2023 6.4).
    pub module: Module,
    /// The parameters an instance may set. For a paramset those are the paramset's
    /// own, and the module's are not among them.
    pub params: IndexMap<Parameter, ParamInfo, BuildHasherDefault<FxHasher>>,
    pub sys_fun_alias: IndexMap<ParamSysFun, Vec<SmolStr>, BuildHasherDefault<FxHasher>>,
    pub op_vars: IndexMap<Variable, OpVar, BuildHasherDefault<FxHasher>>,
    /// Set when this unit compiles a paramset over `module` rather than `module`
    /// itself.
    pub paramset: Option<ParamsetInfo>,
}

/// A paramset (VAMS-2023 6.4) and what it does to the module it names.
pub struct ParamsetInfo {
    /// The paramset declaration: its name is the descriptor's, and its statements
    /// are the entry statements of its body.
    pub decl: Module,
    /// Every parameter of the target module, in declaration order. None of them is
    /// settable: each one's value is the paramset's statement for it, or its own
    /// declared default.
    pub target_params: Vec<Parameter>,
    /// A target parameter and the index of the statement that sets it.
    pub assigns: Vec<(Parameter, usize)>,
}

impl ModuleInfo {
    /// The name a netlist instantiates: the paramset's where there is one.
    pub fn name(&self, db: &CompilationDB) -> String {
        match &self.paramset {
            Some(set) => set.decl.name(db),
            None => self.module.name(db),
        }
    }

    /// What makes this descriptor's symbols unique. A paramset and the module it
    /// names are two descriptors, so they cannot share one.
    pub fn uuid(&self, db: &CompilationDB) -> u32 {
        match &self.paramset {
            Some(set) => set.decl.uuid(db),
            None => self.module.uuid(db),
        }
    }

    /// The substitution [`hir_lower`](hir_lower) applies to the module's body, or
    /// `None` for a module compiled as itself.
    pub fn paramset_init(&self, db: &CompilationDB) -> Option<ParamsetInit<'_>> {
        let set = self.paramset.as_ref()?;
        Some(ParamsetInit {
            target_params: &set.target_params,
            body: set.decl.paramset_block(db),
            assigns: &set.assigns,
        })
    }

    /// The unit that compiles `decl`, a paramset, as the module it names. `None`
    /// when the target is not a module of this file, which `hir_ty` reports.
    fn collect_paramset(
        db: &CompilationDB,
        cu: CompilationUnit,
        decl: Module,
        modules: &[ModuleInfo],
        sink: &mut ConsoleSink,
        all_vars_opvars: bool,
    ) -> Option<ModuleInfo> {
        let target_name = decl.paramset_target(db)?;
        let base = modules.iter().find(|info| info.module.name(db) == target_name)?;

        // The paramset's own declarations: its parameters, and the aliases for them.
        let own = ModuleInfo::collect(db, cu, decl, sink, all_vars_opvars);

        let target_params: Vec<Parameter> = base.params.keys().copied().collect();
        let assigns: Vec<(Parameter, usize)> = decl
            .paramset_stmts(db)
            .into_iter()
            .enumerate()
            .filter_map(|(stmt, name)| {
                // A statement naming something that is not a settable parameter of
                // the target is reported by `hir_ty`; here it just sets nothing,
                // and the parameter keeps its declared default.
                let (param, _) = base
                    .params
                    .iter()
                    .find(|(param, info)| info.name == name && !param.is_local(db))?;
                Some((*param, stmt))
            })
            .collect();

        Some(ModuleInfo {
            module: base.module,
            params: own.params,
            // The system parameters are the module's, because they are what its
            // behaviour reads; the paramset's own aliases ride along with its
            // parameters.
            sys_fun_alias: base.sys_fun_alias.clone(),
            op_vars: base.op_vars.clone(),
            paramset: Some(ParamsetInfo { decl, target_params, assigns }),
        })
    }

    fn collect(
        db: &CompilationDB,
        cu: CompilationUnit,
        module: Module,
        sink: &mut ConsoleSink,
        all_vars_opvars: bool,
    ) -> ModuleInfo {
        let mut params: IndexMap<Parameter, ParamInfo, BuildHasherDefault<FxHasher>> =
            IndexMap::default();
        let mut sys_fun_alias: IndexMap<ParamSysFun, Vec<SmolStr>, BuildHasherDefault<FxHasher>> =
            IndexMap::default();
        let mut op_vars: IndexMap<Variable, OpVar, BuildHasherDefault<FxHasher>> =
            IndexMap::default();

        let ast = cu.ast(db);

        let mut resolved_attrs = AHashSet::new();
        let mut declarations = module.rec_declarations(db);
        let mut add_diagnostic = |attr: ast::Attr, diag: &dyn Diagnostic| {
            if resolved_attrs.insert(attr.syntax().text_range()) {
                sink.add_diagnostic(diag, cu.root_file(), db)
            }
        };
        while let Some((name, dec)) = declarations.next() {
            match dec {
                ScopeDef::Variable(var) => {
                    // 3.2.1 Output variables
                    //
                    // operating point variables must fulfill two properties
                    // * have a description or units attribute
                    // * belong to a module (not a block/function) -> no path

                    // check for units or description
                    let units = var.get_attr(db, &ast, "units");
                    let desc = var.get_attr(db, &ast, "desc");
                    if units.is_none() && desc.is_none() && !all_vars_opvars {
                        continue;
                    }

                    // check that we are not in a block
                    let name_len = name.len();
                    let path = declarations.to_path(name);
                    if path.len() != name_len {
                        continue;
                    }
                    let units = units
                        .and_then(|attr| {
                            let lit = attr.val().and_then(|e| e.as_str_literal());
                            if lit.is_none() {
                                add_diagnostic(attr.clone(), &IllegalAttr { attr });
                            }
                            lit
                        })
                        .unwrap_or_default();
                    let desc = desc
                        .and_then(|attr| {
                            let lit = attr.val().and_then(|e| e.as_str_literal());
                            if lit.is_none() {
                                add_diagnostic(attr.clone(), &IllegalAttr { attr });
                            }
                            lit
                        })
                        .unwrap_or_default();
                    op_vars.insert(var, OpVar { unit: units, description: desc });
                }

                ScopeDef::Parameter(param) => {
                    let units = param
                        .get_attr(db, &ast, "units")
                        .and_then(|attr| {
                            let lit = attr.val().and_then(|e| e.as_str_literal());
                            if lit.is_none() {
                                add_diagnostic(attr.clone(), &IllegalAttr { attr });
                            }
                            lit
                        })
                        .unwrap_or_default();

                    let desc = param
                        .get_attr(db, &ast, "desc")
                        .and_then(|attr| {
                            let lit = attr.val().and_then(|e| e.as_str_literal());
                            if lit.is_none() {
                                add_diagnostic(attr.clone(), &IllegalAttr { attr });
                            }
                            lit
                        })
                        .unwrap_or_default();

                    let group = param
                        .get_attr(db, &ast, "group")
                        .and_then(|attr| {
                            let lit = attr.val().and_then(|e| e.as_str_literal());
                            if lit.is_none() {
                                add_diagnostic(attr.clone(), &IllegalAttr { attr });
                            }
                            lit
                        })
                        .unwrap_or_default();

                    let type_attr = param.get_attr(db, &ast, "type");
                    let type_ = param.get_attr(db, &ast, "type").and_then(|attr| {
                        let lit = attr.val().and_then(|e| e.as_str_literal());
                        if lit.is_none() {
                            add_diagnostic(attr.clone(), &IllegalAttr { attr });
                        }
                        lit
                    });
                    let is_instance = match type_.as_deref() {
                        Some("instance") => true,
                        Some("model") | None => false,
                        Some(found) => {
                            let attr = type_attr.unwrap();
                            add_diagnostic(
                                attr.clone(),
                                &UnknownType { expr: attr.val().unwrap(), found },
                            );
                            false
                        }
                    };

                    params.insert(
                        param,
                        ParamInfo {
                            name: declarations.to_path(name),
                            alias: Vec::new(),
                            unit: units,
                            description: desc,
                            group,
                            is_instance,
                        },
                    );
                }

                ScopeDef::AliasParameter(alias) => match alias.resolve(db).unwrap() {
                    ResolvedAliasParameter::Parameter(param) => {
                        params.entry(param).or_default().alias.push(declarations.to_path(name))
                    }
                    ResolvedAliasParameter::SystemParameter(sys_fun) => {
                        sys_fun_alias.entry(sys_fun).or_default().push(declarations.to_path(name))
                    }
                },

                _ => (),
            }
        }

        ModuleInfo { module, params, op_vars, sys_fun_alias, paramset: None }
    }
}

struct IllegalAttr {
    attr: ast::Attr,
}

impl Diagnostic for IllegalAttr {
    fn build_report(&self, root_file: FileId, db: &dyn BaseDB) -> Report {
        let FileSpan { range, file } = db
            .parse(root_file)
            .to_file_span(self.attr.syntax().text_range(), &db.sourcemap(root_file));
        Report::error()
            .with_message(format!(
                "illegal expression supplied to '{}' attribute; expected a string literal",
                self.attr.name().unwrap(),
            ))
            .with_labels(vec![Label {
                style: LabelStyle::Primary,
                file_id: file,
                range: range.into(),
                message: "expected a string literal".to_owned(),
            }])
    }
}

struct UnknownType<'a> {
    expr: Expr,
    found: &'a str,
}

impl Diagnostic for UnknownType<'_> {
    fn build_report(&self, root_file: FileId, db: &dyn BaseDB) -> Report {
        let FileSpan { range, file } = db.parse(root_file).to_file_span(
            self.expr.syntax().parent().unwrap().text_range(),
            &db.sourcemap(root_file),
        );
        Report::warning()
            .with_message(format!(
                "unknown type \"{}\" expected \"model\" or \"instance\"",
                self.found
            ))
            .with_labels(vec![Label {
                style: LabelStyle::Primary,
                file_id: file,
                range: range.into(),
                message: "unknown type".to_owned(),
            }])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParamInfo {
    pub name: SmolStr,
    pub alias: Vec<SmolStr>,
    pub unit: String,
    pub description: String,
    pub group: String,
    pub is_instance: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpVar {
    pub unit: String,
    pub description: String,
}
