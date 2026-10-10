use hir::{
    BranchWrite, BuiltIn, Case, CaseCond, ContributeKind, Expr, ExprId, Node, ResolvedFun, Stmt,
    StmtId, Type,
};
use mir::builder::InstBuilder;
use mir::{Opcode, Value, FALSE, F_ZERO, TRUE};
use syntax::ast::BinaryOp;

use crate::body::BodyLoweringCtx;
use crate::{CallBackKind, CurrentKind, ImplicitEquationKind, ParamKind, PlaceKind};

impl BodyLoweringCtx<'_, '_, '_> {
    /// `a && b`, lowered the way `BinaryOp::BooleanAnd` is.
    pub(crate) fn and(&mut self, a: Value, b: Value) -> Value {
        self.lower_select_with(a, |_| b, |_| FALSE)
    }

    /// `a || b`, lowered the way `BinaryOp::BooleanOr` is.
    pub(crate) fn or(&mut self, a: Value, b: Value) -> Value {
        self.lower_select_with(a, |_| TRUE, |_| b)
    }

    /// VAMS-2023 5.10.3.3: whether `timer(start_time, period, time_tol, enable)`
    /// fires in this evaluation.
    ///
    /// Unlike `cross`, this one gets to *ask* for the timepoint. `$bound_step` is
    /// capped at the distance remaining to the next event, which lands ngspice on it
    /// exactly -- measured, not assumed: `$abstime - t_event` comes back as
    /// identically zero. A step the solver shortens for its own reasons costs
    /// nothing, since the cap is recomputed, closer, on the next evaluation.
    ///
    /// `time_tol` is what the clause calls the width of that landing: "the analog
    /// simulator places a time point within time_tol of an event". With none given
    /// the default is "at, or just beyond, the time of the event", so the window is
    /// closed on the early side and the cap lands the point exactly on it.
    fn lower_timer(&mut self, args: &[ExprId]) -> Option<Value> {
        let start = *args.first()?;
        if self.body.is_missing(start) {
            return None;
        }
        let start = self.lower_expr(start);
        let period = match args.get(1) {
            Some(&p) if !self.body.is_missing(p) => self.lower_expr(p),
            _ => F_ZERO,
        };
        let now = self.ctx.use_param(ParamKind::Abstime);

        // The next scheduled event. Negative means "not scheduled yet", which no real
        // event time can be, so the first evaluation picks up `start_time`.
        let state = self.ctx.alloc_retained_state(-1.0);
        let prev = self.ctx.retained_prev(state);
        let unscheduled = self.ctx.ins().flt(prev, F_ZERO);

        // "If the start_time or period expressions change value during the evaluation
        // of the analog block, the next event will be scheduled based on the latest
        // value of the start_time and period." So `start_time` is not read once and
        // kept: a second slot remembers the value the live schedule was built from,
        // and a different one reschedules onto the new time.
        //
        // That sentence is what makes an event-driven clock source writable, where
        // each event's handler names the time of the next one -- a DCO with
        // cycle-to-cycle jitter, a divider that stretches a period, a spread-spectrum
        // source. Without it such a model fires once and then stands still, because
        // its first `start_time` is the only one ever read.
        let last_start = self.ctx.alloc_retained_state(0.0);
        let scheduled_from = self.ctx.retained_prev(last_start);
        self.ctx.store_retained(last_start, start);
        let rescheduled = self.ctx.ins().fne(start, scheduled_from);
        let from_start = self.or(unscheduled, rescheduled);
        let next = self.ctx.make_select(from_start, |_s, taken| if taken { start } else { prev });

        // A model that computes its own event times is adding up a chain of
        // intervals while the solver adds up a chain of steps, and the two sums can
        // land a hair apart even when the arithmetic says they agree. `time_tol` is
        // the clause's own answer to how near is near enough, so it opens the window
        // by that much on the early side instead of being accepted and ignored.
        //
        // Zero is not a safe default. An instant missed by its last bit is an instant
        // still ahead, so the event does not fire and the cap below asks for the
        // attosecond in between, and then for the one in between that, until the
        // solver gives up with "timestep too small" -- which is what a reference
        // clock and an oscillator whose edges coincide used to do. So with none given
        // the tool picks one, as the clause allows: a part in 1e12 of the instant
        // itself, which is some thousands of times the precision the instant is held
        // to and a trillionth of the time it names.
        let tol = match args.get(2) {
            Some(&t) if !self.body.is_missing(t) => self.lower_expr(t),
            _ => {
                let scale = self.ctx.fconst(1e-12);
                let from_zero = self.fmax(next, F_ZERO);
                self.ctx.ins().fmul(from_zero, scale)
            }
        };
        let window = self.ctx.ins().fsub(next, tol);
        let reached = self.ctx.ins().fge(now, window);

        // "If the period expression evaluates to a value less than or equal to 0.0,
        // the timer shall trigger only once at the specified start_time." A
        // non-periodic timer that has fired is parked beyond any simulation time.
        let periodic = self.ctx.ins().fgt(period, F_ZERO);
        let never = self.ctx.fconst(f64::MAX);
        let one = self.ctx.fconst(1.0);
        // Pin the divisor when there is no period: the division is evaluated either
        // way and only the `periodic` arm keeps its result.
        let divisor = self.ctx.make_select(periodic, |_s, taken| if taken { period } else { one });
        // Skip whole periods in case the solver got past several at once:
        //   next + period * (floor((now - next) / period) + 1)
        // Clamped at zero, because an event taken inside `time_tol` is taken
        // *before* its instant: a negative elapsed time floors to -1, advances the
        // schedule by nothing and leaves the same instant due for ever.
        let elapsed = self.ctx.ins().fsub(now, next);
        let elapsed = self.fmax(elapsed, F_ZERO);
        let periods = self.ctx.ins().fdiv(elapsed, divisor);
        let periods = self.ctx.ins().floor(periods);
        let periods = self.ctx.ins().fadd(periods, one);
        let advance = self.ctx.ins().fmul(period, periods);
        let after = self.ctx.ins().fadd(next, advance);
        let after = self.ctx.make_select(periodic, |_s, taken| if taken { after } else { never });

        // The schedule advances whether or not `enable` lets the event through: "it
        // will start generating events once enable returns to being nonzero as if it
        // had never been disabled."
        let new_next = self.ctx.make_select(reached, |_s, taken| if taken { after } else { next });
        self.ctx.store_retained(state, new_next);

        // "If enable argument is specified and it is zero, then timer() is inactive,
        // meaning that it does not generate events as long as enable is zero."
        let enabled = match args.get(3) {
            Some(&en) if !self.body.is_missing(en) => {
                let en = self.lower_expr(en);
                let zero = self.ctx.iconst(0);
                Some(self.ctx.ins().ine(en, zero))
            }
            _ => None,
        };

        // Ask for a timepoint on the next event, but not while inactive -- a disabled
        // timer should not be steering the timestep either.
        // Nothing to ask for once the point already stands within tolerance of the
        // instant: it has been placed, which is all the clause asks of the simulator.
        let remaining = self.ctx.ins().fsub(new_next, now);
        let due = self.ctx.ins().fgt(remaining, tol);
        let due = match enabled {
            Some(en) => self.and(due, en),
            None => due,
        };
        let bound = self.ctx.make_select(due, |_s, taken| if taken { remaining } else { never });
        self.bound_step(bound);

        let fired = match enabled {
            Some(en) => self.and(reached, en),
            None => reached,
        };
        Some(fired)
    }

    /// VAMS-2023 5.10.3.1/5.10.3.2: whether `cross` or `above` fires in this
    /// evaluation.
    ///
    /// The crossing is detected between the previous *accepted* timestep and this
    /// one: the expression's value is kept in a retained state (the same mechanism
    /// that gives `@(cross)` variables their cross-timestep memory), and the event
    /// fires when the two straddle zero in the requested direction.
    ///
    /// `above` differs from `cross` in two ways the LRM is explicit about: it takes
    /// no `dir` argument and triggers only from below, and it also fires during
    /// initialization and dc -- "if the expression is positive at the conclusion of
    /// the initial condition analysis that precedes a transient analysis, the
    /// above() function shall generate an event", where `cross` stays quiet until
    /// time has advanced from zero.
    ///
    /// Neither yet controls the timestep, so the event lands on the first accepted
    /// point *after* the crossing rather than inside the `time_tol` / `expr_tol` box
    /// the LRM asks for -- see the tracking issue. Ordering is right, accuracy is
    /// bounded by the step the simulator happened to take.
    ///
    /// Returns `None` for an event function that is still unscheduled, which leaves
    /// its body unconditional as before.
    fn lower_monitored_event(&mut self, call: ExprId) -> Option<Value> {
        let (fun, args) = match self.body.get_expr(call) {
            Expr::Call { fun: ResolvedFun::BuiltIn(fun), args } => (fun, args),
            _ => return None,
        };
        // Argument layout per function: `cross(expr, dir, time_tol, expr_tol,
        // enable)` against `above(expr, time_tol, expr_tol, enable)` -- `above` has
        // no direction, so `enable` sits one place earlier. `timer` schedules on
        // absolute time instead of a crossing and has its own lowering; `absdelta`
        // stays unscheduled.
        let (dir_arg, tol_arg, enable_arg) = match fun {
            BuiltIn::cross => (Some(1), 2, 4),
            BuiltIn::above => (None, 1, 3),
            BuiltIn::timer => return self.lower_timer(args),
            _ => return None,
        };

        let expr = *args.first()?;
        if self.body.is_missing(expr) {
            return None;
        }
        let cur = self.lower_expr(expr);

        // The previous accepted value of the expression. Stored unconditionally, so
        // the comparison always refers to the last accepted timestep.
        let state = self.ctx.alloc_retained_state(0.0);
        let prev = self.ctx.retained_prev(state);
        self.ctx.store_retained(state, cur);

        let cur_ge = self.ctx.ins().fge(cur, F_ZERO);
        let prev_lt = self.ctx.ins().flt(prev, F_ZERO);
        let rising = self.and(prev_lt, cur_ge);

        let fired = match dir_arg {
            // `dir` is optional and defaults to "either direction". A value other
            // than -1, 0 or +1 generates no event at all, which falls out of the
            // comparisons.
            Some(dir_arg) => {
                let cur_le = self.ctx.ins().fle(cur, F_ZERO);
                let prev_gt = self.ctx.ins().fgt(prev, F_ZERO);
                let falling = self.and(prev_gt, cur_le);
                match args.get(dir_arg) {
                    Some(&dir) if !self.body.is_missing(dir) => {
                        let dir = self.lower_expr(dir);
                        let zero = self.ctx.iconst(0);
                        let one = self.ctx.iconst(1);
                        let minus_one = self.ctx.iconst(-1);
                        let both = self.ctx.ins().ieq(dir, zero);
                        let up = self.ctx.ins().ieq(dir, one);
                        let down = self.ctx.ins().ieq(dir, minus_one);
                        let want_rising = self.or(up, both);
                        let want_falling = self.or(down, both);
                        let up = self.and(want_rising, rising);
                        let down = self.and(want_falling, falling);
                        self.or(up, down)
                    }
                    _ => self.or(rising, falling),
                }
            }
            // `above` "generates a monitored analog event ... when the expression
            // crosses zero (0) from below" and takes no direction argument.
            None => rising,
        };

        let time = self.ctx.use_param(ParamKind::Abstime);
        let advanced = self.ctx.ins().fgt(time, F_ZERO);

        // Ask the solver to put the next point on the crossing. Nothing to steer
        // without equations (verilogae, the init function), where `$bound_step` has
        // no simulator to reach.
        if !self.ctx.no_equations {
            self.bound_step_to_crossing(args, tol_arg, cur, prev, time);
        }

        // `above` also fires wherever the expression is *already* positive before
        // time has moved, which is 5.10.3.2's whole point: "if the expression is
        // positive at the conclusion of the initial condition analysis that precedes
        // a transient analysis, the above() function shall generate an event".
        //
        // The test is on `$abstime`, not on the analysis flags, because those are not
        // stable across the Newton iterations of one timestep -- ngspice reports
        // ANALYSIS_STATIC only on the first iteration of the initial step and
        // ANALYSIS_TRAN on the rest, so a flag-gated event fires on one iteration and
        // is then overwritten by the others. `$abstime` is fixed for the whole step.
        //
        // In a dc sweep this fires at every point where the expression is positive
        // rather than only where it crosses, since nothing commits retained state
        // while time stands still. 5.10.3.2 asks for crossings there and does not
        // control the sweep step to resolve them; over-firing samples the same value
        // a crossing would.
        let fired = match fun {
            BuiltIn::above => {
                let not_advanced = self.ctx.ins().fle(time, F_ZERO);
                let positive = self.ctx.ins().fgt(cur, F_ZERO);
                let at_init = self.and(not_advanced, positive);
                self.or(fired, at_init)
            }
            _ => fired,
        };

        // "If enable is specified and it is zero, then cross()/above() is inactive",
        // which covers the initialization event too.
        let fired = match args.get(enable_arg) {
            Some(&en) if !self.body.is_missing(en) => {
                let en = self.lower_expr(en);
                let zero = self.ctx.iconst(0);
                let enabled = self.ctx.ins().ine(en, zero);
                self.and(fired, enabled)
            }
            _ => fired,
        };

        match fun {
            // "The cross() function can only generate an event after the simulation
            // time has advanced from zero", and it generates none for dc, ac or
            // noise. Both follow from requiring a positive time.
            BuiltIn::cross => Some(self.and(fired, advanced)),
            BuiltIn::above => Some(fired),
            _ => unreachable!("only cross and above take part in scheduling"),
        }
    }

    /// Steer the timestep towards a threshold crossing, so the event lands inside
    /// the box 5.10.3.1 Figure 5-6 draws around it rather than wherever the solver
    /// happened to step next. "In addition, cross() controls the timestep to
    /// accurately resolve the crossing", and 5.10.3.2 says the same of `above`.
    ///
    /// Detection alone cannot do this: by the time two accepted points straddle
    /// zero, the crossing is already behind us, and a model cannot ask for a step to
    /// be rejected. So this predicts instead. The expression's rate of change over
    /// the last accepted step extrapolates to the time it reaches zero, and
    /// `$bound_step` caps the next step there. Overshoot stops being a fraction of
    /// the solver's step and becomes the curvature error of that extrapolation, and
    /// since each capped step lands closer the estimate sharpens as it approaches.
    ///
    /// `time_tol` is the floor: never propose a step below it, which is both what
    /// stops the refinement and what "within time_tol of the crossing" buys. With
    /// none given the tool picks one, as the clause allows -- a thousandth of the
    /// step already being taken, which is relative to whatever scale the solver is
    /// working at, but not less than a part in 1e9 of the elapsed time, so that it
    /// cannot follow the step it is bounding down towards zero.
    fn bound_step_to_crossing(
        &mut self,
        args: &[ExprId],
        tol_arg: usize,
        cur: Value,
        prev: Value,
        now: Value,
    ) {
        let state = self.ctx.alloc_retained_state(0.0);
        let t_prev = self.ctx.retained_prev(state);
        self.ctx.store_retained(state, now);

        let one = self.ctx.fconst(1.0);
        let never = self.ctx.fconst(f64::MAX);

        // The step behind us, and the rate over it. Both meaningless before time has
        // moved, which the `stepping` guard covers; the divisions are evaluated
        // either way, so their divisors are pinned.
        let dt = self.ctx.ins().fsub(now, t_prev);
        let stepping = self.ctx.ins().fgt(dt, F_ZERO);
        let dt_safe = self.ctx.make_select(stepping, |_s, taken| if taken { dt } else { one });
        let change = self.ctx.ins().fsub(cur, prev);
        let rate = self.ctx.ins().fdiv(change, dt_safe);

        let rising = self.ctx.ins().fgt(rate, F_ZERO);
        let falling = self.ctx.ins().flt(rate, F_ZERO);
        let moving = self.or(rising, falling);
        let rate_safe = self.ctx.make_select(moving, |_s, taken| if taken { rate } else { one });

        // Time until the expression reaches zero at this rate. Positive exactly when
        // it is heading towards the threshold rather than away from it.
        let neg_cur = self.ctx.ins().fneg(cur);
        let togo = self.ctx.ins().fdiv(neg_cur, rate_safe);
        let approaching = self.ctx.ins().fgt(togo, F_ZERO);

        let tol = match args.get(tol_arg) {
            Some(&tol) if !self.body.is_missing(tol) => self.lower_expr(tol),
            _ => {
                // A thousandth of the step already being taken, which is relative to
                // whatever scale the solver is working at -- and never less than a
                // part in 1e9 of the time already elapsed, because the floor is only
                // worth anything if the step it proposes is one the solver can take.
                //
                // Each capped step is the next evaluation's yardstick, so without
                // that second term the tolerance follows the step down by a factor of
                // a thousand per evaluation, and an expression resting a hair short
                // of the threshold takes both of them past the point where a proposed
                // step still moves `$abstime` at all. A divider output caught half way
                // up its own transition ramp is resting exactly on a phase detector's
                // switching point, which in a PLL is routine rather than contrived: it
                // took the timestep to 1e-20 s and ngspice gave up with "timestep too
                // small".
                let fine = self.ctx.fconst(1e-3);
                let fine = self.ctx.ins().fmul(dt_safe, fine);
                let floor = self.ctx.fconst(1e-9);
                let floor = self.ctx.ins().fmul(now, floor);
                self.fmax(fine, floor)
            }
        };
        let too_fine = self.ctx.ins().flt(togo, tol);
        let aim = self.ctx.make_select(too_fine, |_s, taken| if taken { tol } else { togo });

        let usable = self.and(stepping, moving);
        let usable = self.and(usable, approaching);
        let bound = self.ctx.make_select(usable, |_s, taken| if taken { aim } else { never });
        self.bound_step(bound);
    }
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
            Stmt::EventControl { events, body } => {
                // VAMS-2023 5.10.1: the body runs when *any* of the ORed events
                // occurs.
                //
                // Track `@(initial_step)` so resets of retained (`@cross`) variables
                // inside it are treated as initial values (read from the retained
                // state) rather than per-evaluation resets. That only holds when the
                // initial step is the whole event expression: ORed with anything
                // else the body runs at other times too, so an assignment in it is
                // not just an initial value.
                let initial_step = !events.is_empty()
                    && events.iter().all(|event| {
                        matches!(
                            event,
                            hir::Event::Global { kind: hir::GlobalEvent::InitialStep, .. }
                        )
                    });

                if initial_step {
                    // Guard the body on a retained "first evaluation" flag so an
                    // initializer applies once and then lets retention carry it,
                    // instead of being re-applied on every evaluation. Without
                    // retained state (the init function, verilogae) there is no flag
                    // to key on, and the body stays unconditional as before.
                    if self.ctx.no_equations {
                        self.lower_stmt(body);
                    } else {
                        let first = self.ctx.first_eval();
                        self.ctx.make_cond(first, |ctx, branch| {
                            if branch {
                                BodyLoweringCtx { body: self.body, path: self.path, ctx }
                                    .lower_stmt(body)
                            }
                        });
                    }
                    return;
                }

                // Every element that carries a runtime condition contributes one:
                // a named event its flag (VAMS-2023 5.10.4), a monitored event its
                // crossing detection (5.10.3). The body is guarded by the
                // disjunction only if *every* element has one -- an element that is
                // still unscheduled, or an unresolved event, leaves the body
                // unconditional, which is how all of them behaved before.
                let mut conds = Vec::with_capacity(events.len());
                let mut all = !events.is_empty();
                for event in events {
                    let cond = match *event {
                        hir::Event::Named { event } => self
                            .body
                            .resolve_event(event)
                            .map(|event| self.ctx.use_place(PlaceKind::NamedEvent(event))),
                        hir::Event::Cross { call: Some(call) } => self.lower_monitored_event(call),
                        _ => None,
                    };
                    match cond {
                        Some(cond) => conds.push(cond),
                        // keep going: a monitored element still has to track its
                        // expression even when a sibling leaves the body unguarded
                        None => all = false,
                    }
                }

                if all {
                    let mut cond = conds[0];
                    for next in &conds[1..] {
                        cond = self.or(cond, *next);
                    }
                    self.ctx.make_cond(cond, |ctx, branch| {
                        if branch {
                            BodyLoweringCtx { body: self.body, path: self.path, ctx }
                                .lower_stmt(body)
                        }
                    });
                } else {
                    self.lower_stmt(body);
                }
            }
            // `-> ev;` records that the event occurred in this evaluation
            Stmt::EventTrigger { event } => {
                self.ctx.def_place(PlaceKind::NamedEvent(event), mir::TRUE);
            }
            Stmt::Assignment { lhs, rhs } => {
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
            // The default arm is lowered into the block the last condition falls
            // through to, and that block has to be sealed first: every arm above it
            // has had its chance, so no further predecessor can appear. An open block
            // answers a variable read with a placeholder phi to be filled in when it
            // is sealed, and a body that branches -- any analog operator with a
            // select in it -- reads through that placeholder from a *successor*
            // block, which is no longer the one that gets filled. The value then
            // reaches codegen undefined. Each arm body above is lowered into a
            // `body_head` that is sealed on purpose for the same reason; this one was
            // missed, and `transition()` in a default arm crashed the compiler.
            self.ctx.ensured_sealed();
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
