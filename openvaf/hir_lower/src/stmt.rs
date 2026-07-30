use hir::{
    BranchWrite, Case, CaseCond, ContributeKind, Event, ExprId, GlobalEvent, Node, Stmt, StmtId,
    Type,
};
use mir::builder::InstBuilder;
use mir::{Opcode, Value, F_ZERO, TRUE};

use crate::body::BodyLoweringCtx;
use crate::{CallBackKind, CurrentKind, LoweringCtx, ParamKind, PlaceKind};

impl BodyLoweringCtx<'_, '_, '_> {
    pub(super) fn lower_stmt(&mut self, stmnt: StmtId) {
        // TODO(msrv): let .. else
        let stmnt = if let Some(stmnt) = self.body.get_stmt(stmnt) {
            stmnt
        } else {
            return;
        };
        match stmnt {
            Stmt::Expr(expr) => {
                self.lower_expr(expr);
            }
            Stmt::EventControl { event: Event::Global { kind, phases }, body }
                if !self.ctx.compact =>
            {
                self.lower_global_event(*kind, phases, body)
            }
            Stmt::EventControl { event, body } => {
                // Track `@(initial_step)` so resets of retained (`@cross`) variables
                // inside it are treated as initial values (read from the retained
                // state) rather than per-evaluation resets. Other events lower their
                // body directly; their effect is gated by guards in the body.
                if matches!(event, hir::Event::Global { kind: hir::GlobalEvent::InitialStep, .. }) {
                    let prev = self.ctx.in_initial_step;
                    self.ctx.in_initial_step = true;
                    self.lower_stmt(body);
                    self.ctx.in_initial_step = prev;
                } else if let hir::Event::Named { event } = *event {
                    // `@(ev)` runs its body only if `ev` was triggered earlier in
                    // this evaluation of the analog block (VAMS-2023 5.10.4).
                    match self.body.resolve_event(event) {
                        Some(event) => {
                            let cond = self.ctx.use_place(PlaceKind::NamedEvent(event));
                            self.ctx.make_cond(cond, |ctx, branch| {
                                if branch {
                                    BodyLoweringCtx { body: self.body, path: self.path, ctx }
                                        .lower_stmt(body)
                                }
                            });
                        }
                        // unresolved event; already diagnosed
                        None => self.lower_stmt(body),
                    }
                } else {
                    self.lower_stmt(body);
                }
            }
            // `-> ev;` records that the event occurred in this evaluation
            Stmt::EventTrigger { event } => {
                self.ctx.def_place(PlaceKind::NamedEvent(event), mir::TRUE);
            }
            Stmt::Assignment { lhs, rhs } => {
                // A retained variable's `@(initial_step)` reset is its initial value
                // (already loaded from the retained state); skip it so it is not
                // re-applied on every evaluation.
                if self.ctx.in_initial_step {
                    let retained = match &lhs {
                        hir::AssignmentLhs::Variable(var)
                        | hir::AssignmentLhs::ArrayElement { var, .. } => {
                            self.ctx.retained_states.contains_key(var)
                        }
                        _ => false,
                    };
                    if retained {
                        return;
                    }
                }
                // Whole-array assignment (`g = '{1.0, 2.0};` or `g = h;`) writes the
                // element places directly: an array is not a single MIR value.
                if let hir::AssignmentLhs::Variable(var) = lhs {
                    if matches!(var.ty(self.ctx.db), Type::Array { .. }) {
                        self.assign_whole_array(var, rhs);
                        return;
                    }
                }
                let val_ = self.lower_expr(rhs);
                match lhs {
                    hir::AssignmentLhs::ArrayElement { var, index } => {
                        self.assign_array_element(var, index, val_)
                    }
                    _ => self.ctx.def_place(lhs.into(), val_),
                }
            }
            Stmt::Contribute { kind, branch, rhs } => self.contribute(kind == ContributeKind::Potential, branch, rhs),
            Stmt::IndirectContribute { kind, branch, constraint_lhs, constraint_rhs } => self
                .indirect_contribute(
                    kind == ContributeKind::Potential,
                    branch,
                    constraint_lhs,
                    constraint_rhs,
                ),

            Stmt::Block { body } => {
                for stmt in body {
                    self.lower_stmt(*stmt)
                }
            }
            Stmt::If { cond, then_branch, else_branch } => {
                let cond_ = self.lower_expr(cond);

                self.ctx.make_cond(cond_, |ctx, branch| {
                    let stmt = if branch { then_branch } else { else_branch };
                    BodyLoweringCtx { body: self.body, path: self.path, ctx }.lower_stmt(stmt);
                });
            }
            Stmt::ForLoop { init, cond, incr, body } => {
                self.lower_stmt(init);
                if stmt_has_continue(self.body, body) {
                    self.lower_for_loop(cond, incr, body);
                } else {
                    // No `continue`: keep the classic body→incr→cond shape so MIR for
                    // ordinary analog for-loops stays unchanged.
                    self.lower_while_loop_with(cond, |s| {
                        s.lower_stmt(body);
                        s.lower_stmt(incr);
                    });
                }
            }
            Stmt::WhileLoop { cond, body } => {
                self.lower_while_loop_with(cond, |s| s.lower_stmt(body))
            }
            Stmt::Case { discr, case_arms } => self.lower_case(discr, case_arms),
            Stmt::Break => self.lower_break(),
            Stmt::Continue => self.lower_continue(),
            Stmt::Return { value } => self.lower_return(value),
        }
    }

    /// `@(initial_step(phases))` and `@(final_step(phases))` run their body only
    /// if the simulator sets the INITIAL_STEP / FINAL_STEP flag and (when phases
    /// are given) one of the phases is the current analysis.
    fn lower_global_event(&mut self, kind: GlobalEvent, phases: &[String], body: StmtId) {
        let flag = match kind {
            GlobalEvent::InitialStep => ParamKind::InitialStep,
            GlobalEvent::FinalStep => ParamKind::FinalStep,
        };
        let flag = self.ctx.use_param(flag);
        self.ctx.make_cond(flag, |ctx, is_set| {
            if !is_set {
                return;
            }
            if phases.is_empty() {
                BodyLoweringCtx { body: self.body, path: self.path, ctx }.lower_stmt(body);
                return;
            }
            let in_phase = Self::any_analysis(ctx, phases);
            ctx.make_cond(in_phase, |ctx, in_phase| {
                if in_phase {
                    BodyLoweringCtx { body: self.body, path: self.path, ctx }.lower_stmt(body);
                }
            });
        });
    }

    /// `analysis(phases[0]) || analysis(phases[1]) || ...`
    fn any_analysis(ctx: &mut LoweringCtx, phases: &[String]) -> Value {
        let (phase, rest) = phases.split_first().unwrap();
        let name = ctx.sconst(phase);
        let is_phase = ctx.call1(CallBackKind::Analysis, &[name]);
        let is_phase = ctx.insert_cast(is_phase, &Type::Integer, &Type::Bool);
        if rest.is_empty() {
            return is_phase;
        }
        ctx.make_select(
            is_phase,
            |ctx, is_phase| if is_phase { TRUE } else { Self::any_analysis(ctx, rest) },
        )
    }

    fn after_jump(&mut self) {
        // Terminal jump filled the current block; give any following statements an
        // unreachable block to lower into (mirrors `$fatal`).
        let unreachable_bb = self.ctx.create_block();
        self.ctx.switch_to_block(unreachable_bb);
        self.ctx.seal_block(unreachable_bb);
    }

    fn lower_break(&mut self) {
        let target =
            self.ctx.loop_stack.last().expect("break validated to be inside a loop").break_to;
        self.ctx.ins().jump(target);
        self.after_jump();
    }

    fn lower_continue(&mut self) {
        let target =
            self.ctx.loop_stack.last().expect("continue validated to be inside a loop").continue_to;
        self.ctx.ins().jump(target);
        self.after_jump();
    }

    fn lower_return(&mut self, value: Option<ExprId>) {
        let fun = self.ctx.function_return.expect("return validated to be inside a function");
        let exit = self.ctx.function_exit.expect("function exit block");
        if let Some(value) = value {
            let val = self.lower_expr(value);
            self.ctx.def_place(PlaceKind::FunctionReturn(fun), val);
        }
        self.ctx.ins().jump(exit);
        self.after_jump();
    }

    fn lower_case(&mut self, discr: ExprId, case_arms: &[Case]) {
        let discr_op = match self.body.expr_type(discr) {
            Type::Real => Opcode::Feq,
            Type::Integer => Opcode::Ieq,
            Type::Bool => Opcode::Beq,
            Type::String => Opcode::Seq,
            Type::Array { .. } => todo!(),
            ty => unreachable!("Invalid type {}", ty),
        };
        let discr = self.lower_expr(discr);
        let end = self.ctx.create_block();

        for Case { cond, body } in case_arms {
            // TODO does default mean that further cases are ignored?
            // standard seems to suggest that no matter where the default case is placed that all
            // other conditions are tested prior
            let vals = match cond {
                CaseCond::Vals(vals) => vals,
                CaseCond::Default => continue,
            };

            // Create the body block
            let body_head = self.ctx.create_block();

            for val in vals {
                self.ctx.ensured_sealed();

                // Lower the condition (val == discriminant)
                let val_ = self.lower_expr(*val);

                let old_loc = self.ctx.get_srcloc();
                self.ctx.set_srcloc(mir::SourceLoc::new(u32::from(*val) as i32 + 1));
                let cond = self.ctx.ins().binary1(discr_op, val_, discr);
                self.ctx.set_srcloc(old_loc);

                // Create the next block
                let next_block = self.ctx.create_block();
                self.ctx.ins().branch(cond, body_head, next_block, false);

                self.ctx.switch_to_block(next_block);
            }

            self.ctx.seal_block(body_head);

            // lower the body
            let next = self.ctx.current_block();
            self.ctx.switch_to_block(body_head);
            self.lower_stmt(*body);
            self.ctx.ins().jump(end);
            self.ctx.switch_to_block(next);
        }

        if let Some(default_case) =
            case_arms.iter().find(|arm| matches!(arm.cond, CaseCond::Default))
        {
            self.lower_stmt(default_case.body);
        }

        self.ctx.ensured_sealed();
        self.ctx.ins().jump(end);

        self.ctx.seal_block(end);
        self.ctx.switch_to_block(end);
    }

    /// Lower `arr = rhs` where `arr` is an array variable. The right-hand side can
    /// only be an array literal or another array variable (the type checker rejects
    /// everything else); both are written element by element.
    fn assign_whole_array(&mut self, var: hir::Variable, rhs: ExprId) {
        let len = self.array_len(var);
        // A cast recorded on the whole array expression (e.g. `'{0, 1}` assigned to
        // a real array) applies to every element.
        let elem_cast = self.body.needs_cast(rhs).and_then(|(src, dst)| match (src, dst.clone()) {
            (Type::Array { ty: src, .. }, Type::Array { ty: dst, .. }) => Some((*src, *dst)),
            _ => None,
        });
        match self.body.get_expr(rhs) {
            Expr::Array(vals) => {
                for (i, val) in vals.iter().enumerate().take(len as usize) {
                    let mut elem = self.lower_expr(*val);
                    if let Some((src, dst)) = &elem_cast {
                        elem = self.ctx.insert_cast(elem, src, dst);
                    }
                    self.ctx.def_place(PlaceKind::VarElement(var, i as u32), elem);
                }
            }
            Expr::Read(hir::Ref::Variable(src_var)) => {
                for i in 0..len.min(self.array_len(src_var)) {
                    let mut elem = self.ctx.use_place(PlaceKind::VarElement(src_var, i));
                    if let Some((src, dst)) = &elem_cast {
                        elem = self.ctx.insert_cast(elem, src, dst);
                    }
                    self.ctx.def_place(PlaceKind::VarElement(var, i), elem);
                }
            }
            _ => unreachable!("unsupported whole-array assignment source"),
        }
    }

    /// Lower `arr[index] = val`. A constant index writes the element place directly;
    /// a runtime index conditionally rewrites every element (`elem_i = (index==i) ?
    /// val : elem_i`), keeping the array in pure SSA.
    fn assign_array_element(&mut self, var: hir::Variable, index: ExprId, val: mir::Value) {
        let len = self.array_len(var);
        if len == 0 {
            return;
        }
        // Element positions are offset by the declared lower bound (see
        // `lower_index`): `real g[2:5]` stores g[2] in element 0.
        let lo = var.array_lo(self.ctx.db);
        if let Some(c) = self.body.as_literalint(&index) {
            let pos = c as i64 - lo as i64;
            if !(0..len as i64).contains(&pos) {
                // Out of the declared range: diagnosed during type checking.
                return;
            }
            self.ctx.def_place(PlaceKind::VarElement(var, pos as u32), val);
            return;
        }
        let idx_val = self.lower_expr(index);
        for i in 0..len {
            let current = self.ctx.use_place(PlaceKind::VarElement(var, i));
            let i_const = self.ctx.iconst(lo + i as i32);
            let cond = self.ctx.ins().ieq(idx_val, i_const);
            let new = self.ctx.make_select(cond, |_s, branch| if branch { val } else { current });
            self.ctx.def_place(PlaceKind::VarElement(var, i), new);
        }
    }

    fn lower_while_loop_with(&mut self, cond: ExprId, lower_body: impl FnOnce(&mut Self)) {
        let loop_cond_head = self.ctx.create_block();
        let loop_body_head = self.ctx.create_block();
        let loop_end = self.ctx.create_block();

        self.ctx.ins().jump(loop_cond_head);
        self.ctx.switch_to_block(loop_cond_head);

        let cond = self.lower_expr(cond);
        self.ctx.ins().br_loop(cond, loop_body_head, loop_end);
        // Body has only the loop-branch predecessor. Cond/end stay open until after
        // the body so `continue`/`break` can register additional predecessors.
        self.ctx.seal_block(loop_body_head);

        self.ctx.switch_to_block(loop_body_head);
        self.ctx
            .loop_stack
            .push(crate::ctx::LoopTargets { continue_to: loop_cond_head, break_to: loop_end });
        lower_body(self);
        self.ctx.loop_stack.pop();
        self.ctx.ensured_sealed();
        if !self.ctx.func.is_filled() {
            self.ctx.ins().jump(loop_cond_head);
        }

        self.ctx.seal_block(loop_cond_head);
        self.ctx.seal_block(loop_end);
        self.ctx.switch_to_block(loop_end);
    }

    fn lower_for_loop(&mut self, cond: ExprId, incr: StmtId, body: StmtId) {
        let loop_cond_head = self.ctx.create_block();
        let loop_body_head = self.ctx.create_block();
        let loop_continue = self.ctx.create_block();
        let loop_end = self.ctx.create_block();

        self.ctx.ins().jump(loop_cond_head);
        self.ctx.switch_to_block(loop_cond_head);

        let cond = self.lower_expr(cond);
        self.ctx.ins().br_loop(cond, loop_body_head, loop_end);
        self.ctx.seal_block(loop_body_head);

        self.ctx.switch_to_block(loop_body_head);
        self.ctx
            .loop_stack
            .push(crate::ctx::LoopTargets { continue_to: loop_continue, break_to: loop_end });
        self.lower_stmt(body);
        self.ctx.loop_stack.pop();
        self.ctx.ensured_sealed();
        if !self.ctx.func.is_filled() {
            self.ctx.ins().jump(loop_continue);
        }

        self.ctx.seal_block(loop_continue);
        self.ctx.switch_to_block(loop_continue);
        self.lower_stmt(incr);
        self.ctx.ensured_sealed();
        if !self.ctx.func.is_filled() {
            self.ctx.ins().jump(loop_cond_head);
        }

        self.ctx.seal_block(loop_cond_head);
        self.ctx.seal_block(loop_end);
        self.ctx.switch_to_block(loop_end);
    }

    fn contribute(&mut self, voltage_src: bool, write: BranchWrite, rhs: ExprId) {
        let is_zero = self.body.get_expr(rhs).is_zero();
        let rhs = self.lower_expr(rhs);
        self.contribute_value(voltage_src, write, rhs, is_zero)
    }

    /// Stamps `rhs_value` into `write`'s branch as a contribution, exactly like
    /// `V(write) <+ rhs_value` (or `I(write) <+ rhs_value` for current contributions),
    /// but taking an already-lowered MIR value instead of an `ExprId`. Used both by
    /// `contribute` (normal `<+` statements) and by indirect branch assignment, which
    /// contributes a fresh implicit unknown instead of an evaluated expression.
    fn contribute_value(
        &mut self,
        voltage_src: bool,
        mut write: BranchWrite,
        rhs: Value,
        is_zero: bool,
    ) {
        let mut negate = false;
        if let BranchWrite::Unnamed { hi, lo } = &mut write {
            self.lower_contribute_unnamed_branch(&mut negate, hi, lo, voltage_src)
        }
        self.ctx.def_place(PlaceKind::IsVoltageSrc(write), voltage_src.into());

        let (mut hi, mut lo) = write.nodes(self.ctx.db);
        if voltage_src && is_zero {
            if matches!(write, BranchWrite::Named(_)) {
                self.lower_contribute_unnamed_branch(&mut negate, &mut hi, &mut lo, voltage_src)
            }
            // TODO: make this a place instead?
            self.ctx.call(CallBackKind::CollapseHint(hi, lo), &[]);
        }

        self.ctx.def_place(
            PlaceKind::Contribute { dst: write, reactive: false, voltage_src: !voltage_src },
            F_ZERO,
        );

        if rhs == F_ZERO {
            return;
        }

        let place = PlaceKind::Contribute { dst: write, reactive: false, voltage_src };
        let old = self.ctx.use_place(place);
        let new = if negate {
            self.ctx.ins().fsub(old, rhs)
        } else if old == F_ZERO {
            rhs
        } else {
            self.ctx.ins().fadd(old, rhs)
        };
        self.ctx.def_place(place, new);
    }

    /// Lowers an indirect branch assignment `<dst> : <constraint_lhs> == <constraint_rhs>;`.
    ///
    /// Introduces one new free unknown `u`, contributes it into `branch` exactly like a
    /// normal `<+` contribution (reusing `contribute_value`, so the backend's existing
    /// voltage-src/current-src stamping - including automatic implicit current-unknown
    /// augmentation for voltage contributions - applies unchanged), and adds an implicit
    /// equation enforcing `constraint_lhs - constraint_rhs == 0`, which is solved for `u`.
    fn indirect_contribute(
        &mut self,
        voltage_src: bool,
        branch: BranchWrite,
        constraint_lhs: ExprId,
        constraint_rhs: ExprId,
    ) {
        let idx = self.ctx.intern.indirect_branch_equations.len() as u32;
        let (eq, u) = self.ctx.implicit_equation(crate::ImplicitEquationKind::IndirectBranch(idx));
        self.ctx.intern.indirect_branch_equations.push(eq);

        self.contribute_value(voltage_src, branch, u, false);

        let lhs = self.lower_expr(constraint_lhs);
        let rhs = self.lower_expr(constraint_rhs);
        let residual = self.ctx.ins().fsub(lhs, rhs);
        self.ctx.def_resist_residual(residual, eq);
    }

    fn lower_contribute_unnamed_branch(
        &mut self,
        negate: &mut bool,
        hi: &mut Node,
        lo: &mut Option<Node>,
        voltage_src: bool,
    ) {
        let hi_ = self.ctx.node(*hi);
        let lo_ = lo.and_then(|lo| self.ctx.node(lo));
        (*hi, *lo) = match (hi_, lo_) {
            (Some(hi), None) => (hi, None),
            (None, Some(lo)) => {
                *negate = true;
                (lo, None)
            }
            (Some(hi), Some(lo)) => {
                let negate_known = self
                    .ctx
                    .get_place(PlaceKind::Contribute {
                        dst: BranchWrite::Unnamed { hi: lo, lo: Some(hi) },
                        reactive: false,
                        voltage_src,
                    })
                    .is_some();
                if negate_known {
                    *negate = true;
                    (lo, Some(hi))
                } else {
                    let param_kind = if voltage_src {
                        ParamKind::Voltage { hi, lo: Some(lo) }
                    } else {
                        ParamKind::Current(CurrentKind::Unnamed { hi, lo: Some(lo) })
                    };
                    self.ctx.use_param(param_kind);
                    (hi, Some(lo))
                }
            }
            (None, None) => unreachable!(),
        };
    }
}

fn stmt_has_continue(body: hir::BodyRef<'_>, stmt: StmtId) -> bool {
    match body.get_stmt(stmt) {
        Some(Stmt::Continue) => true,
        Some(Stmt::Block { body: stmts }) => stmts.iter().any(|&s| stmt_has_continue(body, s)),
        Some(Stmt::If { then_branch, else_branch, .. }) => {
            stmt_has_continue(body, then_branch) || stmt_has_continue(body, else_branch)
        }
        Some(Stmt::WhileLoop { body: b, .. }) | Some(Stmt::EventControl { body: b, .. }) => {
            stmt_has_continue(body, b)
        }
        Some(Stmt::ForLoop { init, incr, body: b, .. }) => {
            stmt_has_continue(body, init)
                || stmt_has_continue(body, incr)
                || stmt_has_continue(body, b)
        }
        Some(Stmt::Case { case_arms, .. }) => {
            case_arms.iter().any(|arm| stmt_has_continue(body, arm.body))
        }
        _ => false,
    }
}
