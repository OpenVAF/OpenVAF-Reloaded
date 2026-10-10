use ahash::AHashSet;
use hir::{AssignmentLhs, BodyRef, CaseCond, Event, ExprId, Node, Stmt, StmtId, Type, Variable};
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
        // Pre-pass: find the variables whose value has to survive to the next
        // evaluation, i.e. those that can be read before they are written. Each is
        // backed by retained slots and starts every evaluation at its previous
        // accepted value, which is what makes an `@(cross)` latch a latch and lets an
        // event handler accumulate. An array variable retains every element (one slot
        // each), so e.g. an ADC's sampled bit vector survives.
        let mut retained: Vec<Variable> = Vec::new();
        let mut assigned = ahash::AHashSet::new();
        for &stmnt in self.body.entry() {
            self.collect_retained(stmnt, &mut assigned, &mut retained);
        }
        let mut seen = ahash::AHashSet::new();
        retained.retain(|v| seen.insert(*v));

        // (place, state, element type) for every retained slot, in store order.
        let mut slots: Vec<(PlaceKind, crate::RetainedState, Type)> = Vec::new();

        if !self.ctx.no_equations {
            for &var in &retained {
                let (elem_ty, places) = self.retained_layout(var);
                let mut states = Vec::with_capacity(places.len());
                for place in places {
                    let state = self.ctx.alloc_retained_state(0.0);
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
    fn retained_load(&mut self, state: crate::RetainedState, elem_ty: &Type) -> Value {
        let prev = self.ctx.retained_prev(state);
        match elem_ty {
            Type::Real => prev,
            _ => self.ctx.insert_cast(prev, &Type::Real, elem_ty),
        }
    }

    /// Store a retained slot's final value (cast to real) for the next timestep.
    fn retained_save(&mut self, state: crate::RetainedState, val: Value, elem_ty: &Type) {
        let as_real = match elem_ty {
            Type::Real => val,
            _ => self.ctx.insert_cast(val, elem_ty, &Type::Real),
        };
        self.ctx.store_retained(state, as_real);
    }

    /// Collect the variables whose value has to survive from one evaluation of the
    /// analog block to the next: those whose read can be reached without a write.
    ///
    /// VAMS-2023 re-executes the analog block from the top on every evaluation while
    /// its variables keep what they held, so `n = n + 1` under an event accumulates
    /// (5.10.2's bit-error counter) and 4.5.10's period example can read a value that
    /// is assigned further down the block. A variable that is always written before
    /// it is read needs none of that -- its value comes from this evaluation -- so
    /// retention is granted exactly where a read can come first.
    ///
    /// Definite assignment, conservatively. A write settles a variable for what
    /// follows only if it is certain to have happened, so anything that may not run
    /// -- one arm of an `if`, a loop body, an event handler -- leaves the variable
    /// unsettled afterwards. That is also what gives `@(cross)` latches their
    /// retention: the handler may not fire, so a later read can come first.
    ///
    /// Writing one element of an array does not settle the whole array.
    fn collect_retained(
        &self,
        stmnt: StmtId,
        assigned: &mut AHashSet<Variable>,
        dst: &mut Vec<Variable>,
    ) {
        let stmt = match self.body.get_stmt(stmnt) {
            Some(stmt) => stmt,
            None => return,
        };

        match stmt {
            Stmt::Assignment { lhs, rhs } => {
                self.collect_reads(rhs, assigned, dst);
                match lhs {
                    AssignmentLhs::Variable(var) => {
                        assigned.insert(var);
                    }
                    // A partial write leaves the rest of the array unsettled, and an
                    // array is retained whole or not at all.
                    AssignmentLhs::ArrayElement { index, .. } => {
                        self.collect_reads(index, assigned, dst)
                    }
                    _ => {}
                }
            }
            Stmt::Expr(expr) => self.collect_reads(expr, assigned, dst),
            Stmt::Contribute { rhs, .. } => self.collect_reads(rhs, assigned, dst),
            Stmt::EventTrigger { .. } | Stmt::Break | Stmt::Continue => {}
            Stmt::Return { value } => {
                if let Some(value) = value {
                    self.collect_reads(value, assigned, dst)
                }
            }
            Stmt::EventControl { events, body } => {
                // The event expression is evaluated every time; the body is not.
                for event in events {
                    match *event {
                        Event::Named { event } => self.collect_reads(event, assigned, dst),
                        Event::Cross { call: Some(call) } => {
                            self.collect_reads(call, assigned, dst)
                        }
                        _ => {}
                    }
                }
                self.collect_maybe(body, assigned, dst);
            }
            Stmt::Block { body } => {
                for &s in body {
                    self.collect_retained(s, assigned, dst);
                }
            }
            Stmt::If { cond, then_branch, else_branch } => {
                self.collect_reads(cond, assigned, dst);
                let then_set = self.collect_maybe(then_branch, assigned, dst);
                let else_set = self.collect_maybe(else_branch, assigned, dst);
                // Settled afterwards only if both arms settled it.
                for var in then_set.intersection(&else_set) {
                    assigned.insert(*var);
                }
            }
            Stmt::Case { discr, case_arms } => {
                self.collect_reads(discr, assigned, dst);
                // Without knowing that some arm is always taken, nothing is settled.
                for arm in case_arms {
                    if let CaseCond::Vals(vals) = &arm.cond {
                        for &val in vals {
                            self.collect_reads(val, assigned, dst);
                        }
                    }
                    self.collect_maybe(arm.body, assigned, dst);
                }
            }
            Stmt::ForLoop { init, cond, incr, body } => {
                // The initializer runs exactly once, before anything else.
                self.collect_retained(init, assigned, dst);
                self.collect_reads(cond, assigned, dst);
                let mut inner = self.collect_maybe(body, assigned, dst);
                self.collect_retained(incr, &mut inner, dst);
            }
            Stmt::WhileLoop { cond, body } => {
                self.collect_reads(cond, assigned, dst);
                self.collect_maybe(body, assigned, dst);
            }
        }
    }

    /// Analyse a statement that may or may not run: its reads count, but its writes
    /// settle nothing for what comes after it. Returns what it would have settled.
    fn collect_maybe(
        &self,
        stmnt: StmtId,
        assigned: &AHashSet<Variable>,
        dst: &mut Vec<Variable>,
    ) -> AHashSet<Variable> {
        let mut inner = assigned.clone();
        self.collect_retained(stmnt, &mut inner, dst);
        inner
    }

    /// Record every variable this expression reads that is not settled yet.
    fn collect_reads(
        &self,
        expr: ExprId,
        assigned: &AHashSet<Variable>,
        dst: &mut Vec<Variable>,
    ) {
        if self.body.is_missing(expr) {
            return;
        }
        // `try_get_expr`, not `get_expr`: this walks into every sub-expression,
        // including the node arguments of `V()` and `I()`, which name no value.
        let expr = match self.body.try_get_expr(expr) {
            Some(expr) => expr,
            None => return,
        };
        match expr {
            hir::Expr::Read(hir::Ref::Variable(var)) => {
                if !assigned.contains(&var) {
                    dst.push(var);
                }
            }
            hir::Expr::Read(_) | hir::Expr::Literal(_) => {}
            hir::Expr::BinaryOp { lhs, rhs, .. } => {
                self.collect_reads(lhs, assigned, dst);
                self.collect_reads(rhs, assigned, dst);
            }
            hir::Expr::UnaryOp { expr, .. } => self.collect_reads(expr, assigned, dst),
            hir::Expr::Select { cond, then_val, else_val } => {
                self.collect_reads(cond, assigned, dst);
                self.collect_reads(then_val, assigned, dst);
                self.collect_reads(else_val, assigned, dst);
            }
            hir::Expr::Index { base, index } => {
                self.collect_reads(base, assigned, dst);
                self.collect_reads(index, assigned, dst);
            }
            hir::Expr::Call { args, .. } => {
                for &arg in args {
                    self.collect_reads(arg, assigned, dst);
                }
            }
            hir::Expr::Array(vals) => {
                for &val in vals {
                    self.collect_reads(val, assigned, dst);
                }
            }
        }
    }

    /// Bound the next timestep (VAMS-2023 9.17.2). Each call bounds it, so the
    /// effective bound is the smallest of them -- `$bound_step` is not an assignment.
    /// The place is only read back once an earlier call has declared it, so a module
    /// with a single call (the common case) lowers exactly as it did before.
    pub fn bound_step(&mut self, step_size: Value) {
        let step_size = if self.ctx.get_place(PlaceKind::BoundStep).is_some() {
            let prev = self.ctx.use_place(PlaceKind::BoundStep);
            let smaller = self.ctx.ins().flt(step_size, prev);
            self.lower_select_with(smaller, |_| step_size, |_| prev)
        } else {
            step_size
        };
        self.ctx.def_place(PlaceKind::BoundStep, step_size);
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
