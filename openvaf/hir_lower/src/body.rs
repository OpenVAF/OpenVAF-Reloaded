use hir::{
    AssignmentLhs, BodyRef, BuiltIn, Event, Expr, ExprId, Node, Ref, ResolvedFun, Stmt, StmtId,
    Type, Variable,
};
use mir::builder::InstBuilder;
use mir::{Block, Value};
use stdx::iter::zip;

use crate::ctx::LoweringCtx;
use crate::{ParamKind, PlaceKind};

pub struct BodyLoweringCtx<'a, 'c1, 'c2> {
    pub ctx: &'a mut LoweringCtx<'c1, 'c2>,
    pub body: BodyRef<'a>,
    pub path: &'a str,
}

impl<'c1, 'c2> BodyLoweringCtx<'_, 'c1, 'c2> {
    pub fn lower_entry_stmts(&mut self) {
        // Pre-pass: find variables assigned inside event handlers. Each is backed
        // by retained limit-state slots so it holds its value across timesteps (true
        // latch/event semantics, e.g. a Schmitt trigger). The variable starts each
        // evaluation at its previous accepted value. An array variable retains every
        // element (one slot each), so e.g. an ADC's sampled bit vector survives.
        let mut retained: Vec<Variable> = Vec::new();
        for &stmnt in self.body.entry() {
            self.collect_event_assigned(stmnt, false, &mut retained);
        }
        let mut seen = ahash::AHashSet::new();
        retained.retain(|v| seen.insert(*v));

        // (place, state, element type) for every retained slot, in store order.
        let mut slots: Vec<(PlaceKind, crate::LimitState, Type)> = Vec::new();

        if !self.ctx.no_equations {
            for &var in &retained {
                let (elem_ty, places) = self.retained_layout(var);
                let mut states = Vec::with_capacity(places.len());
                for place in places {
                    let state = self.ctx.alloc_retained_state();
                    let init = self.retained_load(state, &elem_ty);
                    self.ctx.def_place(place, init);
                    states.push(state);
                    slots.push((place, state, elem_ty.clone()));
                }
                self.ctx.retained_states.insert(var, states);
            }
        }

        for &stmnt in self.body.entry() {
            self.lower_stmt(stmnt)
        }

        // Post-pass: store each retained slot's final value for the next timestep.
        for (place, state, elem_ty) in slots {
            let v_final = self.ctx.use_place(place);
            self.retained_save(state, v_final, &elem_ty);
        }
    }

    /// The element type and the per-element places that back a retained variable: a
    /// scalar has one `Var` place, an array one `VarElement` place per index.
    fn retained_layout(&self, var: Variable) -> (Type, Vec<PlaceKind>) {
        match var.ty(self.ctx.db) {
            Type::Array { ty, len } => {
                let places = (0..len).map(|i| PlaceKind::VarElement(var, i)).collect();
                (*ty, places)
            }
            ty => (ty, vec![PlaceKind::Var(var)]),
        }
    }

    /// Read a retained slot's previous-timestep value (stored as real) back into the
    /// variable's element type. A real element needs no cast.
    fn retained_load(&mut self, state: crate::LimitState, elem_ty: &Type) -> Value {
        let prev = self.ctx.retained_prev(state);
        match elem_ty {
            Type::Real => prev,
            _ => self.ctx.insert_cast(prev, &Type::Real, elem_ty),
        }
    }

    /// Store a retained slot's final value (cast to real) for the next timestep.
    fn retained_save(&mut self, state: crate::LimitState, val: Value, elem_ty: &Type) {
        let as_real = match elem_ty {
            Type::Real => val,
            _ => self.ctx.insert_cast(val, elem_ty, &Type::Real),
        };
        self.ctx.store_retained(state, as_real);
    }

    /// Recursively collect variables assigned inside monitored event handlers.
    fn collect_event_assigned(&self, stmnt: StmtId, in_event: bool, dst: &mut Vec<Variable>) {
        let stmt = match self.body.get_stmt(stmnt) {
            Some(stmt) => stmt,
            None => return,
        };
        match stmt {
            Stmt::Assignment { lhs, rhs } => {
                if in_event {
                    match lhs {
                        AssignmentLhs::Variable(var) => dst.push(var),
                        AssignmentLhs::ArrayElement { var, .. } => dst.push(var),
                        _ => {}
                    }
                    self.collect_event_expr_assigned(rhs, dst);
                }
            }
            Stmt::Expr(expr) if in_event => self.collect_event_expr_assigned(expr, dst),
            Stmt::Contribute { rhs, .. } if in_event => self.collect_event_expr_assigned(rhs, dst),
            Stmt::Expr(_) | Stmt::Contribute { .. } => {}
            Stmt::EventControl { event, body } => {
                let inner = in_event || matches!(event, Event::Cross | Event::Timer { .. });
                self.collect_event_assigned(body, inner, dst);
            }
            Stmt::Block { body } => {
                for &s in body {
                    self.collect_event_assigned(s, in_event, dst);
                }
            }
            Stmt::If { then_branch, else_branch, .. } => {
                self.collect_event_assigned(then_branch, in_event, dst);
                self.collect_event_assigned(else_branch, in_event, dst);
            }
            Stmt::ForLoop { body, .. } | Stmt::WhileLoop { body, .. } => {
                self.collect_event_assigned(body, in_event, dst);
            }
            Stmt::Case { case_arms, .. } => {
                for arm in case_arms {
                    self.collect_event_assigned(arm.body, in_event, dst);
                }
            }
        }
    }

    fn collect_event_expr_assigned(&self, expr: ExprId, dst: &mut Vec<Variable>) {
        match self.body.get_expr(expr) {
            Expr::Read(_) | Expr::Literal(_) => {}
            Expr::BinaryOp { lhs, rhs, .. } => {
                self.collect_event_expr_assigned(lhs, dst);
                self.collect_event_expr_assigned(rhs, dst);
            }
            Expr::UnaryOp { expr, .. } => self.collect_event_expr_assigned(expr, dst),
            Expr::Select { cond, then_val, else_val } => {
                self.collect_event_expr_assigned(cond, dst);
                self.collect_event_expr_assigned(then_val, dst);
                self.collect_event_expr_assigned(else_val, dst);
            }
            Expr::Call { fun, args } => {
                if matches!(fun, ResolvedFun::BuiltIn(BuiltIn::rdist_normal)) {
                    if let Some(&seed) = args.first() {
                        if let Expr::Read(Ref::Variable(var)) = self.body.get_expr(seed) {
                            if var.ty(self.ctx.db) == Type::Integer {
                                dst.push(var);
                            }
                        }
                    }
                }
                for &arg in args {
                    self.collect_event_expr_assigned(arg, dst);
                }
            }
            Expr::Array(args) => {
                for &arg in args {
                    self.collect_event_expr_assigned(arg, dst);
                }
            }
            Expr::Index { base, index } => {
                self.collect_event_expr_assigned(base, dst);
                self.collect_event_expr_assigned(index, dst);
            }
        }
    }

    pub fn nodes_from_args(
        &mut self,
        args: &[ExprId],
        kind: impl Fn(Node, Option<Node>) -> ParamKind,
    ) -> Value {
        let hi = self.body.into_node(args[0]);
        let lo = args.get(1).map(|&arg| self.body.into_node(arg));
        self.ctx.nodes(hi, lo, kind)
    }

    pub fn lower_select(
        &mut self,
        cond: ExprId,
        lower_then_val: impl FnMut(BodyLoweringCtx<'_, 'c1, 'c2>) -> Value,
        lower_else_val: impl FnMut(BodyLoweringCtx<'_, 'c1, 'c2>) -> Value,
    ) -> Value {
        let cond = self.lower_expr(cond);
        self.lower_select_with(cond, lower_then_val, lower_else_val)
    }

    pub fn lower_select_with(
        &mut self,
        cond: Value,
        mut lower_then_val: impl FnMut(BodyLoweringCtx<'_, 'c1, 'c2>) -> Value,
        mut lower_else_val: impl FnMut(BodyLoweringCtx<'_, 'c1, 'c2>) -> Value,
    ) -> Value {
        self.ctx.make_select(cond, |ctx, branch| {
            let ctx = BodyLoweringCtx { ctx, body: self.body, path: self.path };
            if branch {
                lower_then_val(ctx)
            } else {
                lower_else_val(ctx)
            }
        })
    }

    pub fn lower_cond_with<T>(
        &mut self,
        cond: Value,
        mut lower_body: impl FnMut(BodyLoweringCtx<'_, 'c1, 'c2>, bool) -> T,
    ) -> ((Block, T), (Block, T)) {
        self.ctx.make_cond(cond, |ctx, branch| {
            let ctx = BodyLoweringCtx { ctx, body: self.body, path: self.path };
            lower_body(ctx, branch)
        })
    }

    pub fn lower_multi_select<const N: usize>(
        &mut self,
        cond: Value,
        lower_body: impl FnMut(BodyLoweringCtx<'_, 'c1, 'c2>, bool) -> [Value; N],
    ) -> [Value; N] {
        let ((then_bb, mut then_vals), (else_bb, else_vals)) =
            self.lower_cond_with(cond, lower_body);
        for (then_val, else_val) in zip(&mut then_vals, else_vals) {
            *then_val = self.ctx.ins().phi(&[(then_bb, *then_val), (else_bb, else_val)]);
        }
        then_vals
    }
}

impl LoweringCtx<'_, '_> {
    /// Lowers a body
    pub fn lower_expr_body(&mut self, body: BodyRef, i: usize) -> Value {
        BodyLoweringCtx { ctx: self, body, path: "" }.lower_expr(body.get_entry_expr(i))
    }
}
