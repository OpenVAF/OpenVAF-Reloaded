use std::mem;

use basedb::lints::LintRegistry;
use basedb::{AstIdMap, ErasedAstId, LintAttrs};
use syntax::ast::{self, ArgListOwner, AttrIter, AttrsOwner, FunctionRef};
use syntax::name::{AsName, Name};
use syntax::{AstNode, AstPtr};

// use tracing::debug;
use super::{Body, BodySourceMap};
use crate::db::HirDefDB;
use crate::expr::{CaseCond, Event, GlobalEvent};
use crate::nameres::DefMapSource;
use crate::{BlockLoc, Case, Expr, ExprId, Intern, Literal, Path, ScopeId, Stmt, StmtId};

pub(super) struct LowerCtx<'a> {
    pub(super) db: &'a dyn HirDefDB,
    pub(super) body: &'a mut Body,
    pub(super) source_map: &'a mut BodySourceMap,
    pub(super) ast_id_map: &'a AstIdMap,
    pub(super) curr_scope: (ScopeId, ErasedAstId),
    pub(super) registry: &'a LintRegistry,
    /// Enclosing module (for compile-time constant evaluation of genvar/bus
    /// expressions against module parameters). `None` for function/var/param bodies.
    pub(super) module: Option<ast::ModuleDecl>,
    /// Names declared `genvar` in the enclosing module.
    pub(super) genvar_names: Vec<syntax::name::Name>,
    /// Net names declared as a vectored/bus (`electrical [0:n] inode;`).
    pub(super) bus_names: Vec<syntax::name::Name>,
    /// Currently-bound genvar values during compile-time loop unrolling.
    pub(super) genvars: Vec<(syntax::name::Name, i64)>,
    /// Currently-bound integer loop values during static loop unrolling.
    pub(super) const_ints: Vec<(syntax::name::Name, i64)>,
    /// Formal-port substitutions used while flattening module instances.
    pub(super) substitutions: Vec<(syntax::name::Name, ExprId)>,
}

impl LowerCtx<'_> {
    pub fn collect_opt_expr(&mut self, expr: Option<ast::Expr>) -> ExprId {
        if let Some(expr) = expr {
            self.collect_expr(expr)
        } else {
            self.missing_expr()
        }
    }

    pub fn collect_expr(&mut self, expr: ast::Expr) -> ExprId {
        let e = match &expr {
            ast::Expr::PrefixExpr(e) => {
                let expr = self.collect_opt_expr(e.expr());
                if let Some(op) = e.op_kind() {
                    Expr::UnaryOp { expr, op }
                } else {
                    Expr::Missing
                }
            }

            ast::Expr::BinExpr(e) => {
                let lhs = self.collect_opt_expr(e.lhs());
                let rhs = self.collect_opt_expr(e.rhs());
                Expr::BinaryOp { lhs, rhs, op: e.op_kind() }
            }

            ast::Expr::ParenExpr(e) => return self.collect_opt_expr(e.expr()),

            ast::Expr::ArrayExpr(e) => {
                let vals = e.exprs().map(|expr| self.collect_expr(expr)).collect();
                Expr::Array(vals)
            }

            ast::Expr::Call(call) => {
                let fun = call.function_ref().and_then(|fun| match fun {
                    FunctionRef::Path(path) => Path::resolve(path),
                    FunctionRef::SysFun(fun) => Some(Path::new_ident(fun.as_name())),
                });

                let args = if let Some(args) = call.arg_list().map(|list| list.args()) {
                    args.map(|arg| self.collect_expr(arg)).collect()
                } else {
                    vec![]
                };

                Expr::Call { fun, args }
            }

            ast::Expr::SelectExpr(e) => {
                let cond = self.collect_opt_expr(e.condition());
                let then_val = self.collect_opt_expr(e.then_val());
                let else_val = self.collect_opt_expr(e.else_val());
                Expr::Select { cond, then_val, else_val }
            }

            ast::Expr::IndexExpr(e) => {
                if let Some(id) = self.try_substituted_index(e, &expr) {
                    return id;
                }
                // Vectored/bus node element `inode[i]` with a compile-time-constant
                // index resolves to the expanded scalar node `inode[<k>]`.
                if let Some(id) = self.try_bus_index(e, &expr) {
                    return id;
                }
                let base = self.collect_opt_expr(e.base());
                let index = self.collect_opt_expr(e.index());
                Expr::Index { base, index }
            }

            // TODO refactor with if let binding and default case is missing expression
            // BLOCK
            ast::Expr::PathExpr(path) => {
                if let Some(id) = self.try_substituted_path(path) {
                    return id;
                }
                // A reference to a bound genvar folds to its current constant value.
                if let Some(id) = self.try_genvar_path(path, &expr) {
                    return id;
                }
                if let Some(path) = path.path().and_then(Path::resolve) {
                    Expr::Path { path, port: false }
                } else {
                    return self.missing_expr();
                }
            }

            ast::Expr::PortFlow(port_flow) => {
                if let Some(path) = port_flow.port().and_then(Path::resolve) {
                    Expr::Path { path, port: true }
                } else {
                    return self.missing_expr();
                }
            }

            ast::Expr::Literal(lit) => Expr::Literal(Literal::new(lit.kind())),
        };
        self.alloc_expr(e, AstPtr::new(&expr))
    }

    pub fn collect_module_inst(&mut self, inst: ast::ModuleInst) -> Vec<StmtId> {
        let child = match self.resolve_module_inst_target(&inst) {
            Some(child) => child,
            None => return Vec::new(),
        };
        let formals = self.formal_ports(&child);
        let mut res = Vec::new();
        for item in inst.instances() {
            let actuals = match item.arg_list() {
                Some(args) => args.args().collect::<Vec<_>>(),
                None => continue,
            };
            let mut formal_iter = formals.iter();
            let mut subst = Vec::new();
            for actual in actuals {
                let Some(first_formal) = formal_iter.next() else { break };
                let mut group = vec![first_formal.clone()];
                let first_base = bus_base(first_formal).unwrap_or(first_formal.as_ref());
                while let Some(next) = formal_iter.clone().next() {
                    if bus_base(next).unwrap_or(next.as_ref()) == first_base {
                        group.push(formal_iter.next().unwrap().clone());
                    } else {
                        break;
                    }
                }
                let expanded = self.expand_actual(&actual, group.len());
                for (formal, actual) in group.into_iter().zip(expanded) {
                    subst.push((formal, actual));
                }
            }

            let old_len = self.substitutions.len();
            self.substitutions.extend(subst);
            for child_item in child.module_items() {
                match child_item {
                    ast::ModuleItem::AnalogBehaviour(behaviour)
                        if behaviour.initial_token().is_none() =>
                    {
                        if let Some(stmt) = behaviour.stmt() {
                            res.push(self.collect_stmt(stmt));
                        }
                    }
                    ast::ModuleItem::ModuleInst(nested) => {
                        res.extend(self.collect_module_inst(nested));
                    }
                    _ => {}
                }
            }
            self.substitutions.truncate(old_len);
        }
        res
    }

    fn resolve_module_inst_target(&self, inst: &ast::ModuleInst) -> Option<ast::ModuleDecl> {
        let name = inst.module()?.as_raw_ident()?.text().to_string();
        let root = self.db.parse(self.curr_scope.0.root_file).tree();
        root.items().find_map(|item| match item {
            ast::Item::ModuleDecl(module) if module.name().map_or(false, |n| n.text() == name) => {
                Some(module)
            }
            _ => None,
        })
    }

    fn formal_ports(&self, module: &ast::ModuleDecl) -> Vec<Name> {
        let bus_ranges = collect_bus_ranges(module);
        let mut res = Vec::new();
        if let Some(ports) = module.module_ports() {
            for port in ports.ports() {
                match port.kind() {
                    ast::ModulePortKind::Name(name) => {
                        let base = name.as_name();
                        extend_bus_names(&mut res, base.clone(), bus_ranges.get(&base));
                    }
                    ast::ModulePortKind::PortRef(port_ref) => {
                        if let Some(name) = port_ref.name() {
                            if let Some(idx) =
                                port_ref.expr().and_then(|e| eval_const_int_in_module(&e, module))
                            {
                                res.push(Name::resolve(&format!("{}[{}]", name.text(), idx)));
                            }
                        }
                    }
                    ast::ModulePortKind::PortDecl(decl) => {
                        let range = decl.dimension().and_then(|dim| {
                            dim.msb()
                                .and_then(|m| eval_const_int_in_module(&m, module))
                                .zip(dim.lsb().and_then(|l| eval_const_int_in_module(&l, module)))
                        });
                        for name in decl.names() {
                            extend_bus_names(&mut res, name.as_name(), range.as_ref());
                        }
                    }
                }
            }
        }
        res
    }

    fn expand_actual(&mut self, actual: &ast::Expr, width: usize) -> Vec<ExprId> {
        if width == 1 {
            return vec![self.collect_expr(actual.clone())];
        }

        if let ast::Expr::IndexExpr(idx) = actual {
            if idx.colon_token().is_some() {
                let expanded = idx
                    .base()
                    .and_then(|base| single_path_name(&base))
                    .zip(idx.index().and_then(|msb| self.eval_genvar_const(&msb)))
                    .zip(idx.lsb().and_then(|lsb| self.eval_genvar_const(&lsb)));
                if let Some(((base, msb), lsb)) = expanded {
                    return expand_index_range(msb, lsb)
                        .into_iter()
                        .map(|idx| self.synthetic_path(&format!("{}[{}]", base, idx), actual))
                        .collect();
                }
            }
        }

        if let Some(base) = single_path_name(actual) {
            return (0..width)
                .map(|idx| self.synthetic_path(&format!("{}[{}]", base, idx), actual))
                .collect();
        }

        vec![self.collect_expr(actual.clone())]
    }

    fn synthetic_path(&mut self, name: &str, src: &ast::Expr) -> ExprId {
        self.alloc_expr(
            Expr::Path { path: Path::new_ident(Name::resolve(name)), port: false },
            AstPtr::new(src),
        )
    }

    fn try_substituted_path(&self, path: &ast::PathExpr) -> Option<ExprId> {
        let ident = path.path()?.as_raw_ident()?;
        let name = Name::resolve(ident.text().as_ref());
        self.substitutions.iter().rev().find_map(
            |(formal, actual)| {
                if formal == &name {
                    Some(*actual)
                } else {
                    None
                }
            },
        )
    }

    fn try_substituted_index(&mut self, e: &ast::IndexExpr, _expr: &ast::Expr) -> Option<ExprId> {
        let base = single_path_name(&e.base()?)?;
        let k = self.eval_genvar_const(&e.index()?)?;
        let name = Name::resolve(&format!("{}[{}]", base, k));
        self.substitutions.iter().rev().find_map(
            |(formal, actual)| {
                if formal == &name {
                    Some(*actual)
                } else {
                    None
                }
            },
        )
    }

    pub fn collect_opt_stmt(&mut self, stmt: Option<ast::Stmt>) -> StmtId {
        match stmt {
            Some(stmt) => self.collect_stmt(stmt),
            None => self.missing_stmt(),
        }
    }

    pub fn collect_stmt(&mut self, stmt: ast::Stmt) -> StmtId {
        let s = match &stmt {
            ast::Stmt::EmptyStmt(_) => Stmt::Empty,
            ast::Stmt::AssignStmt(stmt) => match stmt.assign() {
                Some(a) => Stmt::Assignment {
                    dst: self.collect_opt_expr(a.lval()),
                    val: self.collect_opt_expr(a.rval()),
                    assignment_kind: a.op().unwrap(),
                },
                None => {
                    // debug!(
                    //     tree = debug(stmt),
                    //     src = display(stmt),
                    //     "Assign Statement without assign?"
                    // );
                    Stmt::Missing
                }
            },
            ast::Stmt::ExprStmt(stmt) => Stmt::Expr(self.collect_opt_expr(stmt.expr())),
            ast::Stmt::IfStmt(stmt) => {
                let cond = self.collect_opt_expr(stmt.condition());
                let then_branch = self.collect_opt_stmt(stmt.then_branch());
                let else_branch = self.collect_opt_stmt(stmt.else_branch());
                Stmt::If { cond, then_branch, else_branch }
            }
            ast::Stmt::WhileStmt(stmt) => {
                let cond = self.collect_opt_expr(stmt.condition());
                let body = self.collect_opt_stmt(stmt.body());
                Stmt::WhileLoop { cond, body }
            }
            ast::Stmt::ForStmt(stmt) => {
                // A `for` loop over a genvar or bus-index loop variable with
                // compile-time bounds is unrolled into a flat block of body copies.
                if let Some(id) = self.try_unroll_const_for(stmt) {
                    return id;
                }
                let cond = self.collect_opt_expr(stmt.condition());
                let init = self.collect_opt_stmt(stmt.init());
                let incr = self.collect_opt_stmt(stmt.incr());
                let body = self.collect_opt_stmt(stmt.for_body());
                Stmt::ForLoop { init, cond, incr, body }
            }
            ast::Stmt::CaseStmt(stmt) => self.collect_case_stmt(stmt),
            ast::Stmt::EventStmt(stmt) => return self.collect_event_stmt(stmt),
            ast::Stmt::BlockStmt(stmt) => self.collect_block(stmt),
        };
        self.alloc_stmt(s, AstPtr::new(&stmt), stmt.attrs())
    }

    fn collect_event_stmt(&mut self, event_stmt: &ast::EventStmt) -> StmtId {
        let kind = if event_stmt.initial_step_token().is_some() {
            GlobalEvent::InitialStep
        } else if event_stmt.final_step_token().is_some() {
            GlobalEvent::FinalStep
        } else if let Some(event) = self.collect_timer_event(event_stmt) {
            let body = self.collect_opt_stmt(event_stmt.stmt());
            let stmt = Stmt::EventControl { event, body };
            return self.alloc_stmt(
                stmt,
                AstPtr::new(event_stmt).cast().unwrap(),
                event_stmt.attrs(),
            );
        } else {
            // Monitored event (`@(cross(...))`, etc.): preserve it so MIR lowering
            // can give the variables it assigns cross-timestep retention.
            let body = self.collect_opt_stmt(event_stmt.stmt());
            let stmt = Stmt::EventControl { event: Event::Cross, body };
            return self.alloc_stmt(
                stmt,
                AstPtr::new(event_stmt).cast().unwrap(),
                event_stmt.attrs(),
            );
        };

        let phases = event_stmt.sim_phases().map(|lit| lit.unescaped_value()).collect();
        let event = Event::Global { kind, phases };
        let stmt = Stmt::EventControl { event, body: self.collect_opt_stmt(event_stmt.stmt()) };

        self.alloc_stmt(stmt, AstPtr::new(event_stmt).cast().unwrap(), event_stmt.attrs())
    }

    fn collect_timer_event(&mut self, event_stmt: &ast::EventStmt) -> Option<Event> {
        let ast::Expr::Call(call) = event_stmt.expr()? else { return None };
        let fun = call.function_ref().and_then(|fun| match fun {
            FunctionRef::Path(path) => Path::resolve(path),
            FunctionRef::SysFun(fun) => Some(Path::new_ident(fun.as_name())),
        })?;
        let [name] = fun.segments.as_slice() else { return None };
        if &**name != "timer" {
            return None;
        }

        let mut args = call.arg_list()?.args().map(|arg| self.collect_expr(arg));
        let start = args.next()?;
        let period = args.next();
        Some(Event::Timer { start, period })
    }

    fn collect_case_stmt(&mut self, case_stmt: &ast::CaseStmt) -> Stmt {
        let discr = self.collect_opt_expr(case_stmt.discriminant());
        let case_arms = case_stmt
            .cases()
            .map(|case| {
                let cond = if case.default_token().is_some() {
                    debug_assert_eq!(case.exprs().next(), None);
                    CaseCond::Default
                } else {
                    let vals = case.exprs().map(|e| self.collect_expr(e)).collect();
                    CaseCond::Vals(vals)
                };
                Case { cond, body: self.collect_opt_stmt(case.stmt()) }
            })
            .collect();

        Stmt::Case { discr, case_arms }
    }

    pub fn collect_block(&mut self, block: &ast::BlockStmt) -> Stmt {
        let ast = self.ast_id_map.ast_id(block);
        let id = BlockLoc { ast, parent: self.curr_scope.0 }.intern(self.db);
        let scope = self.db.block_def_map(id);

        let parent_scope = match scope {
            Some(def_map) => {
                let scope = ScopeId {
                    root_file: self.curr_scope.0.root_file,
                    local_scope: def_map.entry(),
                    src: DefMapSource::Block(id),
                };

                mem::replace(&mut self.curr_scope, (scope, ast.into()))
            }
            None => {
                let scope = self.curr_scope.0;
                mem::replace(&mut self.curr_scope, (scope, ast.into()))
            }
        };

        let body = block.body().map(|stmt| self.collect_stmt(stmt)).collect();

        self.curr_scope = parent_scope;
        Stmt::Block { body }
    }

    /// Evaluate a compile-time integer expression in the current genvar/parameter
    /// environment (literals, integer arithmetic, bound genvars and module
    /// parameter defaults). Returns `None` if it is not a compile-time constant.
    fn eval_genvar_const(&self, expr: &ast::Expr) -> Option<i64> {
        use syntax::ast::{BinaryOp, LiteralKind, UnaryOp};
        match expr {
            ast::Expr::Literal(lit) => match lit.kind() {
                LiteralKind::IntNumber(i) => Some(i.value() as i64),
                _ => None,
            },
            ast::Expr::PrefixExpr(p) => {
                let v = self.eval_genvar_const(&p.expr()?)?;
                match p.op_kind()? {
                    UnaryOp::Neg => Some(-v),
                    UnaryOp::Identity => Some(v),
                    _ => None,
                }
            }
            ast::Expr::ParenExpr(p) => self.eval_genvar_const(&p.expr()?),
            ast::Expr::BinExpr(b) => {
                let l = self.eval_genvar_const(&b.lhs()?)?;
                let r = self.eval_genvar_const(&b.rhs()?)?;
                match b.op_kind()? {
                    BinaryOp::Addition => Some(l.wrapping_add(r)),
                    BinaryOp::Subtraction => Some(l.wrapping_sub(r)),
                    BinaryOp::Multiplication => Some(l.wrapping_mul(r)),
                    BinaryOp::Division if r != 0 => Some(l / r),
                    BinaryOp::Remainder if r != 0 => Some(l % r),
                    _ => None,
                }
            }
            ast::Expr::PathExpr(pe) => {
                let ident = pe.path()?.as_raw_ident()?;
                let tname = ident.text();
                // Active loop bindings take precedence over parameters.
                if let Some((_, val)) = self.const_ints.iter().rev().find(|(gv, _)| tname == &**gv)
                {
                    return Some(*val);
                }
                if let Some((_, val)) = self.genvars.iter().rev().find(|(gv, _)| tname == &**gv) {
                    return Some(*val);
                }
                let module = self.module.as_ref()?;
                for pdecl in module.syntax().descendants().filter_map(ast::ParamDecl::cast) {
                    for para in pdecl.paras() {
                        if para.name().map_or(false, |n| n.text() == tname) {
                            return self.eval_genvar_const(&para.default()?);
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Evaluate a compile-time boolean loop condition (a comparison of two
    /// compile-time integers). Returns `None` if it cannot be evaluated.
    fn eval_genvar_cond(&self, expr: &ast::Expr) -> Option<bool> {
        use syntax::ast::BinaryOp;
        match expr {
            ast::Expr::ParenExpr(p) => self.eval_genvar_cond(&p.expr()?),
            ast::Expr::BinExpr(b) => {
                let l = self.eval_genvar_const(&b.lhs()?)?;
                let r = self.eval_genvar_const(&b.rhs()?)?;
                match b.op_kind()? {
                    BinaryOp::LesserTest => Some(l < r),
                    BinaryOp::GreaterTest => Some(l > r),
                    BinaryOp::LesserEqualTest => Some(l <= r),
                    BinaryOp::GreaterEqualTest => Some(l >= r),
                    BinaryOp::EqualityTest => Some(l == r),
                    BinaryOp::NegatedEqualityTest => Some(l != r),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// If `expr` is a single identifier path, return its name.
    fn single_ident(expr: &ast::Expr) -> Option<syntax::name::Name> {
        match expr {
            ast::Expr::PathExpr(pe) => {
                let ident = pe.path()?.as_raw_ident()?;
                Some(syntax::name::Name::resolve(ident.text().as_ref()))
            }
            _ => None,
        }
    }

    /// Fold a reference to a bound static loop variable into its current constant value.
    fn try_genvar_path(&mut self, path: &ast::PathExpr, expr: &ast::Expr) -> Option<ExprId> {
        let ident = path.path()?.as_raw_ident()?;
        let tname = ident.text();
        let val = self
            .const_ints
            .iter()
            .rev()
            .chain(self.genvars.iter().rev())
            .find(|(gv, _)| tname == &**gv)
            .map(|(_, v)| *v)?;
        Some(self.alloc_expr(Expr::Literal(Literal::Int(val as i32)), AstPtr::new(expr)))
    }

    /// Resolve `inode[i]` (bus net element, compile-time-constant index) to the
    /// expanded scalar node `inode[<k>]`.
    fn try_bus_index(&mut self, e: &ast::IndexExpr, expr: &ast::Expr) -> Option<ExprId> {
        let base = e.base()?;
        let pe = match &base {
            ast::Expr::PathExpr(pe) => pe,
            _ => return None,
        };
        let ident = pe.path()?.as_raw_ident()?;
        let bname = ident.text();
        if !self.bus_names.iter().any(|b| bname == &**b) {
            return None;
        }
        let k = self.eval_genvar_const(&e.index()?)?;
        let synth = syntax::name::Name::resolve(&format!("{}[{}]", bname, k));
        let path = Path::new_ident(synth);
        Some(self.alloc_expr(Expr::Path { path, port: false }, AstPtr::new(expr)))
    }

    /// Unroll a static `for` loop into a flat block of body copies. Declared genvars
    /// always qualify; ordinary integer loop variables qualify only when this is
    /// needed to resolve a vectored node index such as `V(out[i])`.
    fn try_unroll_const_for(&mut self, stmt: &ast::ForStmt) -> Option<StmtId> {
        let init = stmt.init()?;
        let init_assign = match &init {
            ast::Stmt::AssignStmt(a) => a.assign()?,
            _ => return None,
        };
        let gv = Self::single_ident(&init_assign.lval()?)?;
        let is_genvar = self.genvar_names.contains(&gv);
        let body = stmt.for_body();
        if !is_genvar && !body.as_ref().is_some_and(|body| self.body_has_bus_index_var(body, &gv)) {
            return None;
        }
        let start = self.eval_genvar_const(&init_assign.rval()?)?;
        let cond = stmt.condition()?;
        let incr = stmt.incr()?;
        let incr_assign = match &incr {
            ast::Stmt::AssignStmt(a) => a.assign()?,
            _ => return None,
        };
        let incr_rval = incr_assign.rval()?;

        let mut bodies = Vec::new();
        let mut val = start;
        let mut guard = 0u64;
        loop {
            if is_genvar {
                self.genvars.push((gv.clone(), val));
            } else {
                self.const_ints.push((gv.clone(), val));
            }
            match self.eval_genvar_cond(&cond) {
                Some(true) => {}
                Some(false) => {
                    self.pop_const_loop_binding(is_genvar);
                    break;
                }
                None => {
                    self.pop_const_loop_binding(is_genvar);
                    return None;
                }
            }
            let body_id = self.collect_opt_stmt(stmt.for_body());
            bodies.push(body_id);
            let next = self.eval_genvar_const(&incr_rval);
            self.pop_const_loop_binding(is_genvar);
            match next {
                Some(n) => val = n,
                None => return None,
            }
            guard += 1;
            if guard > 1_000_000 {
                break;
            }
        }
        if !is_genvar {
            let lval = self.alloc_expr(
                Expr::Path { path: Path::new_ident(gv), port: false },
                AstPtr::new(&init_assign.lval()?),
            );
            let rval = self.alloc_expr(
                Expr::Literal(Literal::Int(val as i32)),
                AstPtr::new(&init_assign.rval()?),
            );
            bodies.push(self.alloc_stmt_desugared(Stmt::Assignment {
                dst: lval,
                val: rval,
                assignment_kind: ast::AssignOp::Assign,
            }));
        }
        Some(self.alloc_stmt_desugared(Stmt::Block { body: bodies }))
    }

    fn pop_const_loop_binding(&mut self, is_genvar: bool) {
        if is_genvar {
            self.genvars.pop();
        } else {
            self.const_ints.pop();
        }
    }

    fn body_has_bus_index_var(&self, stmt: &ast::Stmt, var: &Name) -> bool {
        stmt.syntax().descendants().filter_map(ast::IndexExpr::cast).any(|idx| {
            let Some(base) = idx.base().and_then(|base| single_path_name(&base)) else {
                return false;
            };
            if !self.bus_names.iter().any(|bus| base == &**bus) {
                return false;
            }
            idx.index().is_some_and(|expr| expr_contains_ident(&expr, var))
                || idx.lsb().is_some_and(|expr| expr_contains_ident(&expr, var))
        })
    }

    fn alloc_expr(&mut self, expr: Expr, ptr: AstPtr<ast::Expr>) -> ExprId {
        let id = self.make_expr(expr, Some(ptr.clone()));
        self.source_map.expr_map.insert(ptr, id);
        id
    }
    // desugared exprs don't have ptr, that's wrong and should be fixed
    // somehow.
    pub(super) fn alloc_expr_desugared(&mut self, expr: Expr) -> ExprId {
        self.make_expr(expr, None)
    }

    fn missing_expr(&mut self) -> ExprId {
        self.alloc_expr_desugared(Expr::Missing)
    }

    fn make_expr(&mut self, expr: Expr, src: Option<AstPtr<ast::Expr>>) -> ExprId {
        let id = self.body.exprs.push_and_get_key(expr);
        self.source_map.expr_map_back.insert(id, src);
        id
    }

    fn alloc_stmt(&mut self, stmt: Stmt, ptr: AstPtr<ast::Stmt>, attrs: AttrIter) -> StmtId {
        let attrs = LintAttrs::resolve(
            self.registry,
            attrs,
            &mut self.source_map.diagnostics,
            self.curr_scope.1,
        );
        let id = self.make_stmt(stmt, Some(ptr.clone()), attrs);
        self.source_map.stmt_map.insert(ptr, id);

        id
    }

    // desugared stmts don't have ptr, that's wrong and should be fixed
    // somehow.
    pub(super) fn alloc_stmt_desugared(&mut self, stmt: Stmt) -> StmtId {
        self.make_stmt(stmt, None, LintAttrs::empty(self.curr_scope.1))
    }

    pub(super) fn missing_stmt(&mut self) -> StmtId {
        self.alloc_stmt_desugared(Stmt::Missing)
    }

    fn make_stmt(
        &mut self,
        stmt: Stmt,
        src: Option<AstPtr<ast::Stmt>>,
        attrs: LintAttrs,
    ) -> StmtId {
        let id = self.body.stmts.push_and_get_key(stmt);
        let id2 = self.body.stmt_scopes.push_and_get_key(self.curr_scope.0);
        let id3 = self.source_map.lint_map.push_and_get_key(attrs);
        debug_assert_eq!(id, id2);
        debug_assert_eq!(id2, id3);
        self.source_map.stmt_map_back.insert(id, src);
        id
    }
}

fn collect_bus_ranges(module: &ast::ModuleDecl) -> ahash::AHashMap<Name, (i64, i64)> {
    let mut ranges = ahash::AHashMap::new();
    let mut add = |dim: Option<ast::Dimension>, names: ast::AstChildren<ast::Name>| {
        if let Some(dim) = dim {
            if let Some(range) = dim
                .msb()
                .and_then(|e| eval_const_int_in_module(&e, module))
                .zip(dim.lsb().and_then(|e| eval_const_int_in_module(&e, module)))
            {
                for name in names {
                    ranges.insert(name.as_name(), range);
                }
            }
        }
    };

    for item in module.module_items() {
        match item {
            ast::ModuleItem::BodyPortDecl(decl) => {
                if let Some(decl) = decl.port_decl() {
                    add(decl.dimension(), decl.names());
                }
            }
            ast::ModuleItem::NetDecl(decl) => add(decl.dimension(), decl.names()),
            _ => {}
        }
    }
    ranges
}

fn extend_bus_names(dst: &mut Vec<Name>, base: Name, range: Option<&(i64, i64)>) {
    match range {
        Some((msb, lsb)) => {
            for idx in expand_index_range(*msb, *lsb) {
                dst.push(Name::resolve(&format!("{}[{}]", base, idx)));
            }
        }
        None => dst.push(base),
    }
}

fn expand_index_range(msb: i64, lsb: i64) -> Vec<i64> {
    let (lo, hi) = if msb <= lsb { (msb, lsb) } else { (lsb, msb) };
    (lo..=hi).collect()
}

fn bus_base(name: &Name) -> Option<&str> {
    let name: &str = &**name;
    name.split_once('[').map(|(base, _)| base)
}

fn single_path_name(expr: &ast::Expr) -> Option<String> {
    match expr {
        ast::Expr::PathExpr(pe) => {
            let ident = pe.path()?.as_raw_ident()?;
            Some(ident.text().to_string())
        }
        _ => None,
    }
}

fn expr_contains_ident(expr: &ast::Expr, name: &Name) -> bool {
    expr.syntax()
        .descendants()
        .filter_map(ast::PathExpr::cast)
        .filter_map(|path| path.path()?.as_raw_ident())
        .any(|ident| ident.text() == &**name)
}

fn eval_const_int_in_module(expr: &ast::Expr, module: &ast::ModuleDecl) -> Option<i64> {
    use syntax::ast::{BinaryOp, LiteralKind, UnaryOp};
    match expr {
        ast::Expr::Literal(lit) => match lit.kind() {
            LiteralKind::IntNumber(i) => Some(i.value() as i64),
            _ => None,
        },
        ast::Expr::PrefixExpr(p) => {
            let v = eval_const_int_in_module(&p.expr()?, module)?;
            match p.op_kind()? {
                UnaryOp::Neg => Some(-v),
                UnaryOp::Identity => Some(v),
                _ => None,
            }
        }
        ast::Expr::ParenExpr(p) => eval_const_int_in_module(&p.expr()?, module),
        ast::Expr::BinExpr(b) => {
            let l = eval_const_int_in_module(&b.lhs()?, module)?;
            let r = eval_const_int_in_module(&b.rhs()?, module)?;
            match b.op_kind()? {
                BinaryOp::Addition => Some(l.wrapping_add(r)),
                BinaryOp::Subtraction => Some(l.wrapping_sub(r)),
                BinaryOp::Multiplication => Some(l.wrapping_mul(r)),
                BinaryOp::Division if r != 0 => Some(l / r),
                BinaryOp::Remainder if r != 0 => Some(l % r),
                _ => None,
            }
        }
        ast::Expr::PathExpr(pe) => {
            let ident = pe.path()?.as_raw_ident()?;
            let name = ident.text();
            for pdecl in module.syntax().descendants().filter_map(ast::ParamDecl::cast) {
                for para in pdecl.paras() {
                    if para.name().map_or(false, |n| n.text() == name) {
                        return eval_const_int_in_module(&para.default()?, module);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

impl Literal {
    pub fn new(ast: ast::LiteralKind) -> Literal {
        match ast {
            ast::LiteralKind::String(lit) => {
                Literal::String(lit.unescaped_value().into_boxed_str())
            }
            ast::LiteralKind::IntNumber(lit) => Literal::Int(lit.value()),
            ast::LiteralKind::SiRealNumber(lit) => Literal::Float(lit.value().into()),
            ast::LiteralKind::StdRealNumber(lit) => Literal::Float(lit.value().into()),
            ast::LiteralKind::Inf => {
                // TODO check that this allowed somewhere?
                Literal::Inf
            }
        }
    }
}
