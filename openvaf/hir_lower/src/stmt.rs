use hir::{BranchWrite, Case, CaseCond, ContributeKind, Expr, ExprId, Node, Stmt, StmtId, Type};
use mir::builder::InstBuilder;
use mir::{Opcode, Value, F_ZERO};
use syntax::ast::BinaryOp;

use crate::body::BodyLoweringCtx;
use crate::{CallBackKind, CurrentKind, ImplicitEquationKind, ParamKind, PlaceKind};

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
            Stmt::Contribute { kind, branch, rhs } => match kind {
                ContributeKind::Potential => self.contribute(true, branch, rhs),
                ContributeKind::Flow => self.contribute(false, branch, rhs),
                ContributeKind::IndirectPotential => self.indirect_contribute(true, branch, rhs),
                ContributeKind::IndirectFlow => self.indirect_contribute(false, branch, rhs),
            },

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
                self.lower_loop(cond, |s| {
                    s.lower_stmt(body);
                    s.lower_stmt(incr);
                });
            }
            Stmt::WhileLoop { cond, body } => self.lower_loop(cond, |s| s.lower_stmt(body)),
            Stmt::Case { discr, case_arms } => self.lower_case(discr, case_arms),
        }
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

    fn lower_loop(&mut self, cond: ExprId, lower_body: impl FnOnce(&mut Self)) {
        let loop_cond_head = self.ctx.create_block();
        let loop_body_head = self.ctx.create_block();
        let loop_end = self.ctx.create_block();

        self.ctx.ins().jump(loop_cond_head);
        self.ctx.switch_to_block(loop_cond_head);

        let cond = self.lower_expr(cond);
        self.ctx.ins().br_loop(cond, loop_body_head, loop_end);
        self.ctx.seal_block(loop_body_head);
        self.ctx.seal_block(loop_end);

        self.ctx.switch_to_block(loop_body_head);
        lower_body(self);
        self.ctx.ins().jump(loop_cond_head);

        self.ctx.seal_block(loop_cond_head);

        self.ctx.switch_to_block(loop_end);
    }

    fn contribute(&mut self, voltage_src: bool, write: BranchWrite, rhs: ExprId) {
        let is_zero = self.body.get_expr(rhs).is_zero();
        self.contribute_with(voltage_src, write, is_zero, |s| s.lower_expr(rhs));
    }

    /// Shared body of a branch contribution. `lower_rhs` is invoked to produce the
    /// contributed value at the exact point the old direct lowering did, so ordinary
    /// contributions keep byte-identical MIR; indirect assignments supply an
    /// already-computed implicit unknown instead.
    fn contribute_with(
        &mut self,
        voltage_src: bool,
        mut write: BranchWrite,
        rhs_is_zero: bool,
        lower_rhs: impl FnOnce(&mut Self) -> Value,
    ) {
        let mut negate = false;
        if let BranchWrite::Unnamed { hi, lo } = &mut write {
            self.lower_contribute_unnamed_branch(&mut negate, hi, lo, voltage_src)
        }
        self.ctx.def_place(PlaceKind::IsVoltageSrc(write), voltage_src.into());

        let (mut hi, mut lo) = write.nodes(self.ctx.db);
        if voltage_src && rhs_is_zero {
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

        let rhs = lower_rhs(self);
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

    /// Lower an indirect branch assignment `V(out) : f(...) == 0` (or the `I(out)`
    /// flow form). The target branch becomes a source whose value is a fresh implicit
    /// unknown `u`; an auxiliary equation pins `u` so the constraint residual is zero.
    /// This reuses exactly the implicit-equation/DAE machinery behind `idt`.
    fn indirect_contribute(&mut self, voltage_src: bool, write: BranchWrite, constraint: ExprId) {
        let (eq, unknown) = self.ctx.implicit_equation(ImplicitEquationKind::IndirectBranch);
        // Drive the branch as a source whose value is the implicit unknown.
        self.contribute_with(voltage_src, write, false, |_| unknown);
        // Residual of the auxiliary equation: `lhs - rhs` of the `==` constraint (== 0).
        let residual = self.lower_constraint_residual(constraint);
        self.ctx.def_resist_residual(residual, eq);
    }

    /// Lower the constraint of an indirect branch assignment to its residual value.
    /// The canonical form is `lhs == rhs`, whose residual is `lhs - rhs`; a bare
    /// expression is treated leniently as `expr == 0`.
    fn lower_constraint_residual(&mut self, constraint: ExprId) -> Value {
        if let Expr::BinaryOp { lhs, rhs, op: BinaryOp::EqualityTest } =
            self.body.get_expr(constraint)
        {
            let lhs = self.lower_real_operand(lhs);
            let rhs = self.lower_real_operand(rhs);
            self.ctx.ins().fsub(lhs, rhs)
        } else {
            self.lower_real_operand(constraint)
        }
    }

    /// Lower an expression and coerce the result to `Real` (integer/bool constraint
    /// operands are widened so the residual is a floating-point quantity).
    fn lower_real_operand(&mut self, expr: ExprId) -> Value {
        let val = self.lower_expr(expr);
        let ty = match self.body.needs_cast(expr) {
            Some((_, dst)) => dst.clone(),
            None => self.body.expr_type(expr),
        };
        match ty {
            Type::Real => val,
            Type::Integer | Type::Bool => self.ctx.insert_cast(val, &ty, &Type::Real),
            _ => val,
        }
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
