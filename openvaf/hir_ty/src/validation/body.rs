use std::mem::replace;

use ahash::{HashMap, HashSet};
use hir_def::body::Body;
use hir_def::{
    expr::Event, expr::GlobalEvent, BranchId, BuiltIn, DefWithBodyId, DisciplineId, Expr, ExprId,
    FunctionArgLoc, Literal, Lookup, ModuleBodyKind, NatureId, NodeId, ParamId, Path, Stmt, StmtId,
    VarId,
};
use stdx::impl_display;
use syntax::ast::{AssignOp, UnaryOp};
use syntax::name::{AsIdent, Name};

use crate::builtin::{
    ABSDELAY_MAX, DDT_TOL, IDT_IC_ASSERT_TOL, NATURE_ACCESS_BRANCH, NATURE_ACCESS_NODES,
    NATURE_ACCESS_NODE_GND, NATURE_ACCESS_PORT_FLOW, NOISE_TABLE_INLINE, NOISE_TABLE_INLINE_NAME,
};
use crate::db::HirTyDB;
use crate::inference::{BranchWrite, InferenceResult, ResolvedFun};
use crate::lower::BranchKind;
use crate::scan::scan_conversions;
use crate::table_model;
use crate::types::{Signature, Ty};
use crate::zi_filter;
use hir_def::Type;

// `EventFun` is an analog event function used outside `@(...)` (VAMS-2023 5.10.3).
#[derive(PartialEq, Eq, Clone, Debug)]
pub enum IllegalCtxAccessKind {
    NatureAccess,
    AnalogOperator { name: Name, is_standard: bool, non_const_dominator: Box<[ExprId]> },
    AnalysisFun { name: Name },
    EventFun { name: Name },
    Var(VarId),
}

#[derive(PartialEq, Eq, Clone, Debug)]
pub struct IllegalCtxAccess {
    pub kind: IllegalCtxAccessKind,
    pub ctx: BodyCtx,
    pub expr: ExprId,
}

#[derive(PartialEq, Eq, Clone, Debug)]
pub enum BodyValidationDiagnostic {
    ExpectedPort {
        expr: ExprId,
        node: NodeId,
    },
    TrivialBranchAccess {
        branch: BranchWrite,
        expr: ExprId,
        stmt: StmtId,
    },
    PotentialOfPortFlow {
        expr: ExprId,
        branch: Option<BranchId>,
    },
    IllegalContribute {
        stmt: StmtId,
        ctx: BodyCtx,
    },

    /// VAMS-2023 5.10.4: in an analog context `-> ev;` is an
    /// `analog_event_statement`, so it may only appear under an event control.
    IllegalEventTrigger {
        stmt: StmtId,
        ctx: BodyCtx,
    },

    /// VAMS-2023 5.10.3: an analog event function is name-resolved and type-checked
    /// but takes no part in scheduling yet, so the guarded statement is evaluated on
    /// every evaluation of the analog block instead of only when the event occurs.
    UnscheduledEvent {
        stmt: StmtId,
        func: BuiltIn,
    },

    /// VAMS-2023 5.10.2: `@(final_step)` is "active during the solution of the last
    /// point", and nothing in OSDI tells a model which point that is, so the body is
    /// reached at every evaluation instead. An analysis list is honoured, so the
    /// body is at least confined to the analyses it names.
    UnconditionalFinalStep {
        stmt: StmtId,
    },

    /// VAMS-2023 9.5.4: a scan writes each conversion into an argument, so an
    /// argument has to name something that can be written to -- a variable, or one
    /// element of an array.
    ScanTargetNotAPlace {
        arg: ExprId,
    },

    /// VAMS-2023 9.5.4: ... and that variable has to be able to hold what its
    /// conversion produces. There is no conversion between a string and a number,
    /// so `%s` into a real variable, or `%e` into a string one, is a type error.
    ScanTargetTy {
        arg: ExprId,
        spec: char,
        produces: Type,
        found: Type,
    },

    /// VAMS-2023 9.17.1: `$discontinuity(n)` for a non-negative degree is accepted
    /// but announces nothing, because OSDI has no channel for it. Only the
    /// `$discontinuity(-1)` form that pairs with `$limit` (9.17.3) does anything.
    IgnoredDiscontinuity {
        stmt: StmtId,
        expr: ExprId,
    },

    WriteToInputArg {
        expr: ExprId,
        arg: FunctionArgLoc,
    },

    IllegalParamAccess {
        def: ParamId,
        expr: ExprId,
        param: ParamId,
    },

    IllegalCtxAccess(IllegalCtxAccess),

    ConstSimparam {
        known: bool,
        expr: ExprId,
        stmt: StmtId,
    },

    /// An analog operator whose constant arguments cannot be reduced at compile
    /// time: a `$table_model` table or a Z-transform filter.
    InvalidOperator {
        expr: ExprId,
        what: &'static str,
        err: String,
    },
    UnsupportedFunction {
        expr: ExprId,
        func: BuiltIn,
    },

    IncompatibleNatureAccess {
        candidates: [Option<(Name, Name)>; 2],
        access_nature: Option<NatureId>,
        access_expr: ExprId,
        branch: String,
    },

    IllegalNatureAccess {
        is_pot: bool,
        access_expr: ExprId,
    },

    IncompatibleImplicitBranch {
        access: ExprId,
        node1: NodeId,
        node2: NodeId,
    },

    /// `break`/`continue` outside any loop (VAMS-2023 §5.11).
    JumpOutsideLoop {
        stmt: StmtId,
        kind: JumpKind,
    },

    /// `break`/`continue` inside an analog `for` loop (VAMS-2023 §5.11 / §5.9.3).
    JumpInAnalogFor {
        stmt: StmtId,
        kind: JumpKind,
    },

    /// `return` outside an analog user-defined function (VAMS-2023 §5.11).
    ReturnOutsideFunction {
        stmt: StmtId,
    },

    /// `return;` without a value in a function that returns a value (VAMS-2023 §5.11).
    MissingReturnValue {
        stmt: StmtId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpKind {
    Break,
    Continue,
}

impl JumpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JumpKind::Break => "break",
            JumpKind::Continue => "continue",
        }
    }
}

impl BodyValidationDiagnostic {
    pub fn collect(db: &dyn HirTyDB, def: DefWithBodyId) -> Vec<BodyValidationDiagnostic> {
        let body = db.body(def);
        let infere = db.inference_result(def);

        let ctx = match def {
            DefWithBodyId::ModuleId { kind: ModuleBodyKind::Analog, .. } => BodyCtx::AnalogBlock,
            DefWithBodyId::ModuleId { kind: ModuleBodyKind::AnalogInitial, .. } => {
                BodyCtx::AnalogInitialBlock
            }
            DefWithBodyId::ModuleId { kind: ModuleBodyKind::Procedural, .. } => {
                BodyCtx::ProceduralBlock
            }
            DefWithBodyId::FunctionId(_) => BodyCtx::Function,
            _ => BodyCtx::Const,
        };

        let mut validator = BodyValidator {
            db,
            owner: def,
            body: &body,
            infer: &infere,
            diagnostics: Vec::new(),
            ctx,
            in_event_control: false,
            event_call: None,
            non_const_dominator: Box::default(),
            non_trivial_branches: HashSet::default(),
            trivial_probes: HashMap::default(),
            loop_stack: Vec::new(),
        };

        for stmt in &*body.entry_stmts {
            validator.validate_stmt(*stmt)
        }

        for (branch, exprs) in validator.trivial_probes {
            for (stmt, expr) in exprs {
                validator.diagnostics.push(BodyValidationDiagnostic::TrivialBranchAccess {
                    branch,
                    expr,
                    stmt,
                })
            }
        }

        validator.diagnostics
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum BodyCtx {
    AnalogBlock,
    AnalogInitialBlock,
    ProceduralBlock,
    Conditional,
    EventControl,
    Function,
    ConstOrAnalysis,
    Const,
}

impl BodyCtx {
    fn allow_nature_access(self) -> bool {
        matches!(self, Self::AnalogBlock | Self::Conditional | Self::EventControl)
    }

    fn allow_contribute(self) -> bool {
        matches!(self, Self::AnalogBlock | Self::Conditional)
    }

    fn allow_analog_operator(self) -> bool {
        matches!(self, Self::AnalogBlock)
    }

    fn allow_analysis_fun(self) -> bool {
        !matches!(self, Self::Const)
    }

    fn allow_var_ref(self) -> bool {
        !matches!(self, Self::Const | Self::ConstOrAnalysis)
    }
}

impl_display! {
    match BodyCtx{
       BodyCtx::AnalogBlock => "analog block";
       BodyCtx::AnalogInitialBlock => "analog initial block";
       BodyCtx::ProceduralBlock => "procedural block";
       BodyCtx::Conditional => "conditions";
       BodyCtx::EventControl => "events";
       BodyCtx::Function => "analog functions";
       BodyCtx::ConstOrAnalysis => "constant or analysis";
       BodyCtx::Const => "constants";
    }
}

struct BodyValidator<'a> {
    db: &'a dyn HirTyDB,
    owner: DefWithBodyId,
    body: &'a Body,
    infer: &'a InferenceResult,
    diagnostics: Vec<BodyValidationDiagnostic>,
    ctx: BodyCtx,
    /// Whether the statement being validated is (transitively) the body of an
    /// event control. Unlike `ctx` this survives entering a conditional.
    in_event_control: bool,
    /// The event-function call an event control is currently being validated for.
    /// An event function is legal at exactly this expression and nowhere else
    /// (VAMS-2023 5.10.3), so a nested `@(cross(cross(...)))` is still rejected.
    event_call: Option<ExprId>,
    non_const_dominator: Box<[ExprId]>,
    non_trivial_branches: HashSet<BranchWrite>,
    trivial_probes: HashMap<BranchWrite, Vec<(StmtId, ExprId)>>,
    /// Innermost loop first... actually push on enter so last is innermost.
    loop_stack: Vec<LoopKind>,
}

#[derive(Clone, Copy)]
enum LoopKind {
    While,
    For,
}

impl BodyValidator<'_> {
    fn validate_stmt(&mut self, stmt: StmtId) {
        let cond = match self.body.stmts[stmt] {
            Stmt::Assignment { dst, val, assignment_kind } => {
                self.validate_expr(val, stmt);

                if matches!(assignment_kind, AssignOp::Contribute | AssignOp::Indirect)
                    && !self.ctx.allow_contribute()
                {
                    self.diagnostics
                        .push(BodyValidationDiagnostic::IllegalContribute { stmt, ctx: self.ctx })
                }
                // avoid duplicate errors
                else if self.infer.assignment_destination.contains_key(&stmt) {
                    self.validate_assignment_dst(dst, stmt);
                }

                return;
            }
            Stmt::EventControl { ref events, body } => {
                // Each element of the event expression is validated on its own
                // (VAMS-2023 5.10.1).
                let calls: Vec<_> = events
                    .iter()
                    .filter_map(|event| match *event {
                        Event::Cross { call } => call,
                        _ => None,
                    })
                    .collect();
                // Mirror `hir_lower`'s `EventControl`: the body is guarded only if
                // *every* element of the event expression carries a runtime
                // condition -- `initial_step` its first-evaluation flag, a named
                // event its flag, `cross` its crossing. One element without one
                // (`final_step`, `timer`, ...) leaves the whole body unconditional,
                // however well the others schedule.
                let all_scheduled = !events.is_empty()
                    && events.iter().all(|event| match *event {
                        Event::Global { kind, .. } => kind == GlobalEvent::InitialStep,
                        Event::Named { event } => {
                            matches!(self.infer.expr_types[event], Ty::Event(_))
                        }
                        Event::Cross { call: Some(call) } => matches!(
                            self.infer.resolved_calls.get(&call),
                            Some(ResolvedFun::BuiltIn(func)) if func.schedules_event()
                        ),
                        _ => false,
                    });

                // 5.10.2 puts `final_step` at the last point, and an analysis list
                // narrows which analyses it applies to but says nothing about which
                // point is the last one -- which no part of OSDI does either. So the
                // body is reached at every evaluation, and that is worth saying: it
                // is where a model puts the summary it writes once, and `$fclose` in
                // it closes the file on the first evaluation instead of the last.
                for event in events {
                    if matches!(*event, Event::Global { kind: GlobalEvent::FinalStep, .. }) {
                        self.diagnostics
                            .push(BodyValidationDiagnostic::UnconditionalFinalStep { stmt });
                    }
                }

                let old = replace(&mut self.ctx, BodyCtx::EventControl);
                let old_event = replace(&mut self.in_event_control, true);
                // The event expression is validated in the event context too: it may
                // read natures (`@(cross(V(a)))`) but not use analog operators.
                for call in calls {
                    let old_call = replace(&mut self.event_call, Some(call));
                    self.validate_expr(call, stmt);
                    self.event_call = old_call;

                    // `cross` decides whether the body runs (VAMS-2023 5.10.3.1),
                    // so on its own it no longer warns; `above`, `timer` and
                    // `absdelta` still take no part in scheduling. Either way the
                    // warning stands if some other element of the same event
                    // expression leaves the body unconditional, because then this
                    // function does not end up scheduling anything either. Warn
                    // instead of silently accepting a model whose behaviour is not
                    // the one it describes.
                    if let Some(ResolvedFun::BuiltIn(func)) = self.infer.resolved_calls.get(&call) {
                        if func.is_event_fun() && !(func.schedules_event() && all_scheduled) {
                            self.diagnostics.push(BodyValidationDiagnostic::UnscheduledEvent {
                                stmt,
                                func: *func,
                            });
                        }
                    }
                }
                self.validate_stmt(body);
                self.in_event_control = old_event;
                self.ctx = old;
                return;
            }
            Stmt::Block { ref body } => {
                body.iter().for_each(|stmt| self.validate_stmt(*stmt));
                return;
            }

            Stmt::Missing | Stmt::Empty => return,

            // VAMS-2023 5.10.4: `-> ev;` is an `analog_event_statement`; it is not
            // part of `analog_statement`, so in the analog context it may only
            // appear inside an event control (`@(timer(1n)) -> ev;`).
            Stmt::EventTrigger { .. } => {
                if !self.in_event_control && self.ctx != BodyCtx::ProceduralBlock {
                    self.diagnostics
                        .push(BodyValidationDiagnostic::IllegalEventTrigger { stmt, ctx: self.ctx })
                }
                return;
            }

            Stmt::Expr(e) => {
                self.validate_expr(e, stmt);
                return;
            }

            Stmt::Break => {
                self.validate_jump(stmt, JumpKind::Break);
                return;
            }
            Stmt::Continue => {
                self.validate_jump(stmt, JumpKind::Continue);
                return;
            }
            Stmt::Return { value } => {
                if !matches!(self.owner, DefWithBodyId::FunctionId(_)) {
                    self.diagnostics.push(BodyValidationDiagnostic::ReturnOutsideFunction { stmt });
                } else if value.is_none() {
                    self.diagnostics.push(BodyValidationDiagnostic::MissingReturnValue { stmt });
                }
                if let Some(value) = value {
                    self.validate_expr(value, stmt);
                }
                return;
            }

            Stmt::WhileLoop { cond, body, .. } => {
                self.validate_condition(cond, stmt, |s| {
                    s.loop_stack.push(LoopKind::While);
                    s.validate_stmt(body);
                    s.loop_stack.pop();
                });
                return;
            }
            Stmt::ForLoop { cond, .. } => {
                self.validate_condition(cond, stmt, |s| {
                    s.loop_stack.push(LoopKind::For);
                    s.body.stmts[stmt].walk_child_stmts(|child| s.validate_stmt(child));
                    s.loop_stack.pop();
                });
                return;
            }

            Stmt::If { cond, .. } | Stmt::Case { discr: cond, .. } => cond,
        };

        self.validate_condition(cond, stmt, |s| {
            s.body.stmts[stmt].walk_child_stmts(|stmt| s.validate_stmt(stmt))
        });
    }

    fn validate_jump(&mut self, stmt: StmtId, kind: JumpKind) {
        match self.loop_stack.last() {
            None => {
                self.diagnostics.push(BodyValidationDiagnostic::JumpOutsideLoop { stmt, kind });
            }
            Some(LoopKind::For) => {
                self.diagnostics.push(BodyValidationDiagnostic::JumpInAnalogFor { stmt, kind });
            }
            Some(LoopKind::While) => {}
        }
    }

    fn validate_condition(
        &mut self,
        cond: ExprId,
        stmt: StmtId,
        f: impl FnOnce(&mut Self),
    ) -> Option<Box<[ExprId]>> {
        if self.ctx == BodyCtx::AnalogBlock || self.ctx == BodyCtx::Conditional {
            let mut non_const_access = Vec::new();
            ExprValidator {
                parent: self,
                cond_diagnostic_sink: Some(&mut non_const_access),
                write: false,
                stmt,
            }
            .validate_expr(cond);

            if !non_const_access.is_empty() {
                let non_const_dominator =
                    replace(&mut self.non_const_dominator, non_const_access.into_boxed_slice());
                let ctx = replace(&mut self.ctx, BodyCtx::Conditional);
                f(self);
                self.ctx = ctx;
                return Some(replace(&mut self.non_const_dominator, non_const_dominator));
            }
        } else {
            self.validate_expr(cond, stmt);
        }

        f(self);
        None
    }

    fn validate_expr(&mut self, expr: ExprId, stmt: StmtId) {
        ExprValidator { parent: self, cond_diagnostic_sink: None, write: false, stmt }
            .validate_expr(expr)
    }

    fn validate_assignment_dst(&mut self, expr: ExprId, stmt: StmtId) {
        ExprValidator { parent: self, cond_diagnostic_sink: None, write: true, stmt }
            .validate_expr(expr)
    }
}

struct ExprValidator<'a, 'b> {
    parent: &'a mut BodyValidator<'b>,
    cond_diagnostic_sink: Option<&'a mut Vec<ExprId>>,
    write: bool,
    stmt: StmtId,
}

impl ExprValidator<'_, '_> {
    fn report_illegal_access(&mut self, kind: IllegalCtxAccessKind, expr: ExprId) {
        let err = IllegalCtxAccess { kind, ctx: self.parent.ctx, expr };
        self.report(BodyValidationDiagnostic::IllegalCtxAccess(err));
    }

    fn check_access(
        &mut self,
        kind: impl FnOnce(&Self) -> IllegalCtxAccessKind,
        expr: ExprId,
        allowed: bool,
    ) {
        if let Some(sink) = &mut self.cond_diagnostic_sink {
            sink.push(expr)
        }

        if !allowed {
            self.report_illegal_access(kind(self), expr)
        }
    }

    fn report(&mut self, diagnostic: BodyValidationDiagnostic) {
        self.parent.diagnostics.push(diagnostic)
    }

    fn report_illegal_nature_access(
        &mut self,
        branch: String,
        discipline: DisciplineId,
        access_nature: Option<NatureId>,
        access_expr: ExprId,
    ) {
        let db = self.parent.db;
        let discipline = db.discipline_info(discipline);

        let nature_info = |nature: NatureId| {
            let nature = nature.lookup(db.upcast());
            let nature = &nature.item_tree(db.upcast())[nature.id];
            Some((nature.name.clone(), nature.access.clone()?.0))
        };
        let pot = discipline.potential.and_then(nature_info);
        let flow = discipline.flow.and_then(nature_info);
        self.parent.diagnostics.push(BodyValidationDiagnostic::IncompatibleNatureAccess {
            candidates: [pot, flow],
            access_nature,
            access_expr,
            branch,
        })
    }

    fn validate_implicit_branch(
        &mut self,
        expr: ExprId,
        node1: NodeId,
        node2: NodeId,
    ) -> Option<DisciplineId> {
        if let Some(discipline1) = self.parent.db.node_discipline(node1) {
            if let Some(discipline2) = self.parent.db.node_discipline(node2) {
                let discipline2 = self.parent.db.discipline_info(discipline2);
                if !discipline2.compatible(discipline1, self.parent.db) {
                    self.report(BodyValidationDiagnostic::IncompatibleImplicitBranch {
                        access: expr,
                        node1,
                        node2,
                    });
                } else {
                    return Some(discipline1);
                }
            }
        }

        None
    }

    fn lint_trivial_branch(&mut self, branch: BranchWrite, call: BuiltIn, expr: ExprId) {
        let is_flow = call == BuiltIn::flow;
        if self.write {
            self.parent.non_trivial_branches.insert(branch);
            self.parent.trivial_probes.remove(&branch);
        } else if is_flow && !self.parent.non_trivial_branches.contains(&branch) {
            self.parent.trivial_probes.entry(branch).or_default().push((self.stmt, expr))
        }
    }

    fn validate_flow_or_pot(&mut self, expr: ExprId, call: BuiltIn, discipline: DisciplineId) {
        let is_pot = call == BuiltIn::potential;
        let discipline_ = self.parent.db.discipline_info(discipline);
        if discipline_.potential.is_none() && is_pot || discipline_.flow.is_none() && !is_pot {
            self.report(BodyValidationDiagnostic::IllegalNatureAccess { is_pot, access_expr: expr })
        }
    }

    fn validate_nature_access(
        &mut self,
        access_nature: NatureId,
        access_expr: ExprId,
        args: &[ExprId],
    ) {
        match self.parent.infer.resolved_signatures.get(&access_expr).copied() {
            Some(NATURE_ACCESS_BRANCH) => {
                let branch = self.parent.infer.expr_types[args[0]].unwrap_branch();
                if let Some(branch_info) = self.parent.db.branch_info(branch) {
                    self.report_illegal_nature_access(
                        self.parent.db.branch_data(branch).name.to_string(),
                        branch_info.discipline,
                        Some(access_nature),
                        access_expr,
                    )
                }
            }

            Some(NATURE_ACCESS_NODE_GND) => {
                let node = self.parent.infer.expr_types[args[0]].unwrap_node();
                if let Some(discipline) = self.parent.db.node_discipline(node) {
                    let node = self.parent.db.node_data(node);
                    self.report_illegal_nature_access(
                        format!("({})", node.name),
                        discipline,
                        Some(access_nature),
                        access_expr,
                    )
                }
            }

            Some(NATURE_ACCESS_NODES) => {
                let node1 = self.parent.infer.expr_types[args[0]].unwrap_node();
                let node2 = self.parent.infer.expr_types[args[0]].unwrap_node();
                if let Some(discipline1) = self.parent.db.node_discipline(node1) {
                    if let Some(discipline2) = self.parent.db.node_discipline(node2) {
                        let discipline2 = self.parent.db.discipline_info(discipline2);
                        if discipline2.compatible(discipline1, self.parent.db) {
                            let node1 = self.parent.db.node_data(node1);
                            let node2 = self.parent.db.node_data(node2);
                            self.report_illegal_nature_access(
                                format!("({}, {})", node1.name, node2.name),
                                discipline1,
                                Some(access_nature),
                                access_expr,
                            )
                        } else {
                            self.report(BodyValidationDiagnostic::IncompatibleImplicitBranch {
                                access: access_expr,
                                node1,
                                node2,
                            })
                        }
                    }
                }
            }

            Some(NATURE_ACCESS_PORT_FLOW) => {
                let node = self.parent.infer.expr_types[args[0]].unwrap_port_flow();
                if let Some(discipline) = self.parent.db.node_discipline(node) {
                    let node = self.parent.db.node_data(node);
                    self.report_illegal_nature_access(
                        format!("(<{}>)", node.name),
                        discipline,
                        Some(access_nature),
                        access_expr,
                    )
                }
            }
            Some(_) => unreachable!(),
            None => (),
        };
    }

    fn validate_expr(&mut self, expr: ExprId) {
        match self.parent.body.exprs[expr] {
            Expr::Call { ref fun, ref args, .. } => {
                match self.parent.infer.resolved_calls.get(&expr) {
                    Some(ResolvedFun::BuiltIn(builtin)) => {
                        let signature = self.parent.infer.resolved_signatures.get(&expr);
                        self.validate_builtin(fun, expr, args, *builtin, signature.cloned());
                        return;
                    }
                    Some(ResolvedFun::InvalidNatureAccess(nature)) => {
                        self.validate_nature_access(*nature, expr, args);
                        return;
                    }
                    _ => (),
                }
            }

            Expr::Select { cond, then_val, else_val } => {
                if let Some(non_const_dominators) =
                    self.parent.validate_condition(cond, self.stmt, |s| {
                        let mut validator = ExprValidator {
                            parent: s,
                            cond_diagnostic_sink: self.cond_diagnostic_sink.as_deref_mut(),
                            write: false,
                            stmt: self.stmt,
                        };
                        validator.validate_expr(then_val);
                        validator.validate_expr(else_val);
                    })
                {
                    if let Some(sink) = &mut self.cond_diagnostic_sink {
                        sink.extend(non_const_dominators.to_vec())
                    }
                }
            }

            Expr::Path { port: false, .. } => {
                match self.parent.infer.expr_types[expr] {
                    Ty::FunctionVar { arg: Some(arg), fun, .. } => {
                        let is_output = self.parent.db.function_data(fun).args[arg].is_output;
                        if self.write && !is_output {
                            self.report(BodyValidationDiagnostic::WriteToInputArg {
                                expr,
                                arg: FunctionArgLoc { fun, id: arg },
                            })
                        }
                    }

                    Ty::Var(_, var) => {
                        self.check_access(
                            |__| IllegalCtxAccessKind::Var(var),
                            expr,
                            self.parent.ctx.allow_var_ref(),
                        );
                    }
                    Ty::Param(_, param) => {
                        if let DefWithBodyId::ParamId(def) = self.parent.owner {
                            if def.lookup(self.parent.db.upcast()).id
                                < param.lookup(self.parent.db.upcast()).id
                            {
                                self.report(BodyValidationDiagnostic::IllegalParamAccess {
                                    def,
                                    expr,
                                    param,
                                })
                            }
                        }
                    }
                    _ => (),
                };
                return;
            }

            _ => (),
        }

        self.parent.body.exprs[expr].walk_child_exprs(|child| self.validate_expr(child))
    }

    /// An integer literal, or a negated one. Mirrors `hir::Body::as_literalsignedint`,
    /// which is not reachable from validation.
    fn as_signed_int(&self, expr: ExprId) -> Option<i32> {
        match &self.parent.body.exprs[expr] {
            Expr::Literal(Literal::Int(val)) => Some(*val),
            Expr::UnaryOp { expr, op: UnaryOp::Neg } => match &self.parent.body.exprs[*expr] {
                Expr::Literal(Literal::Int(val)) => Some(-val),
                _ => None,
            },
            _ => None,
        }
    }

    /// VAMS-2023 9.5.4: every argument after the format is written to, so every one
    /// of them has to be writable and has to fit what its conversion produces.
    ///
    /// Neither is something a signature can say. The arguments after the format are
    /// variadic because how many there are, and what each holds, is what the format
    /// says -- so the format is read here, as the lowering reads it, and the two
    /// agree because they call the same function.
    fn validate_scan(&mut self, args: &[ExprId]) {
        let fmt = match args.get(1).map(|&arg| &self.parent.body.exprs[arg]) {
            Some(Expr::Literal(Literal::String(fmt))) => fmt.clone(),
            // Not a literal, which the signature has already rejected.
            _ => return,
        };

        for (k, conv) in scan_conversions(&fmt).into_iter().enumerate() {
            let arg = match args.get(2 + k) {
                Some(&arg) if self.parent.body.exprs[arg] != Expr::Missing => arg,
                // Fewer arguments than conversions: the scan stops where they run
                // out, which is what the clause's return value is for.
                _ => break,
            };
            match self.scan_target_ty(arg) {
                Some(found) if conv.ty.is_assignable_to(&found) => (),
                Some(found) => self.report(BodyValidationDiagnostic::ScanTargetTy {
                    arg,
                    spec: conv.spec,
                    produces: conv.ty,
                    found,
                }),
                None => self.report(BodyValidationDiagnostic::ScanTargetNotAPlace { arg }),
            }
        }
    }

    /// What the variable a scan argument names is declared as, or `None` when it
    /// names no variable: a parameter, a probe, a literal, an expression.
    fn scan_target_ty(&self, arg: ExprId) -> Option<Type> {
        if let Expr::Index { base, .. } = self.parent.body.exprs[arg] {
            // One element of an array variable, whatever the index: an index the
            // model computes is a place too, written by choosing among the
            // elements, the same as an ordinary assignment to one.
            return match self.parent.infer.expr_types[base] {
                Ty::Var(Type::Array { ref ty, .. }, _) => Some((**ty).clone()),
                _ => None,
            };
        }
        match self.parent.infer.expr_types[arg] {
            Ty::Var(ref ty, _) => Some(ty.clone()),
            _ => None,
        }
    }

    fn operator_err(&mut self, expr: ExprId, what: &'static str, err: String) {
        self.parent.diagnostics.push(BodyValidationDiagnostic::InvalidOperator { expr, what, err });
    }

    fn table_err(&mut self, expr: ExprId, err: String) {
        self.operator_err(expr, "$table_model", err)
    }

    fn const_real(&self, expr: ExprId) -> Option<f64> {
        match self.parent.body.exprs[expr] {
            Expr::Literal(Literal::Float(f)) => Some(f.into()),
            Expr::Literal(Literal::Int(i)) => Some(f64::from(i)),
            Expr::UnaryOp { expr, op: UnaryOp::Neg } => Some(-self.const_real(expr)?),
            Expr::UnaryOp { expr, op: UnaryOp::Identity } => self.const_real(expr),
            _ => None,
        }
    }

    /// One data column of the array form, which has to be an array of constants.
    fn const_real_column(&self, arg: ExprId) -> Option<Vec<f64>> {
        let elems = match self.parent.body.exprs[arg] {
            Expr::Array(ref elems) => elems.clone(),
            _ => return None,
        };
        elems.iter().map(|&e| self.const_real(e)).collect()
    }

    /// A table data file, resolved beside the compilation's root source file, the
    /// way a `noise_table` data file is.
    fn read_data_file(&self, name: &str) -> Option<String> {
        let root = self.parent.owner.file(self.parent.db.upcast());
        let dir = self.parent.db.file_path(root).parent()?;
        let path = dir.join(name)?;
        let abs = path.as_path()?;
        std::fs::read_to_string(abs).ok()
    }

    /// VAMS-2023 9.21: `$table_model`.
    ///
    /// The table is built into the model at compile time, which is what lets the
    /// lookup be ordinary arithmetic over the lookup expressions and so carry a
    /// derivative into the Jacobian. Everything the table needs therefore has to
    /// be knowable here, and whatever is not becomes a diagnostic. The lowering
    /// repeats this build and can then assume it succeeds.
    fn validate_table_model(&mut self, expr: ExprId, args: &[ExprId]) {
        // The lookup inputs come first and the data source follows, so the inputs
        // are the leading arguments that are neither a string nor an array.
        let mut ndims = 0;
        while ndims < args.len() {
            match self.parent.infer.expr_types[args[ndims]].to_value() {
                Some(Type::String | Type::Array { .. } | Type::EmptyArray) => break,
                _ => ndims += 1,
            }
        }
        if ndims == 0 || ndims >= args.len() {
            // The signature table has already reported the shape of the call.
            return;
        }
        let from_file =
            matches!(self.parent.infer.expr_types[args[ndims]].to_value(), Some(Type::String));

        let (rows, after) = if from_file {
            let name = match self.parent.body.exprs[args[ndims]] {
                Expr::Literal(Literal::String(ref name)) => name.to_string(),
                _ => {
                    self.table_err(
                        expr,
                        "the data file name has to be a string literal. 9.21 also allows a string \
                         parameter, but the table is compiled into the model and a parameter's \
                         value belongs to the simulator, so its default cannot be read here"
                            .to_owned(),
                    );
                    return;
                }
            };
            let text = match self.read_data_file(&name) {
                Some(text) => text,
                None => {
                    self.table_err(
                        expr,
                        format!("cannot read the table data file '{name}' beside the source file"),
                    );
                    return;
                }
            };
            match table_model::Rows::parse(&text) {
                Ok(rows) => (rows, ndims + 1),
                Err(err) => {
                    self.table_err(expr, format!("{name}: {err}"));
                    return;
                }
            }
        } else {
            // `table_model_array`: N independent columns and then the output.
            let end = 2 * ndims + 1;
            if args.len() < end {
                return;
            }
            let mut cols = Vec::with_capacity(ndims + 1);
            for (i, &arg) in args[ndims..end].iter().enumerate() {
                match self.const_real_column(arg) {
                    Some(col) => cols.push(col),
                    None => {
                        self.table_err(
                            expr,
                            format!(
                                "data column {} has to be an array of constants. 9.21.1 captures \
                                 the data source on the first call and ignores later changes, and \
                                 the table is compiled into the model, so an array the module \
                                 fills in at run time cannot be read here: write the samples as an \
                                 array literal, or put them in a data file",
                                i + 1
                            ),
                        );
                        return;
                    }
                }
            }
            match table_model::Rows::from_columns(&cols) {
                Ok(rows) => (rows, end),
                Err(err) => {
                    self.table_err(expr, err.0);
                    return;
                }
            }
        };

        let spec = match args.get(after) {
            Some(&arg) => match self.parent.body.exprs[arg] {
                Expr::Literal(Literal::String(ref spec)) => spec.to_string(),
                _ => {
                    self.table_err(
                        expr,
                        "the control string has to be a string literal".to_owned(),
                    );
                    return;
                }
            },
            None => String::new(),
        };

        let control = match table_model::Control::parse(&spec, ndims) {
            Ok(control) => control,
            Err(err) => {
                self.table_err(expr, err.0);
                return;
            }
        };
        if let Err(err) = table_model::Table::build(&rows, &control) {
            self.table_err(expr, err.0);
        }
    }

    /// VAMS-2023 4.5.12: the Z-transform filters.
    ///
    /// Table 4-20 makes the two vectors, `T` and `t0` constant expression
    /// arguments, so the filter is reduced to coefficients here and the lowering
    /// can assume it succeeds. An array whose contents the module computes at run
    /// time is not a constant expression, for the same reason a `$table_model`
    /// data column is not: the filter is compiled into the model.
    fn validate_zi_filter(&mut self, expr: ExprId, call: BuiltIn, args: &[ExprId]) {
        use zi_filter::Side::{Coeffs, Roots};
        let name = match call {
            BuiltIn::zi_nd => "$zi_nd",
            BuiltIn::zi_zd => "$zi_zd",
            BuiltIn::zi_np => "$zi_np",
            BuiltIn::zi_zp => "$zi_zp",
            _ => return,
        };
        let (num_side, den_side) = match call {
            BuiltIn::zi_nd => (Coeffs, Coeffs),
            BuiltIn::zi_zd => (Roots, Coeffs),
            BuiltIn::zi_np => (Coeffs, Roots),
            BuiltIn::zi_zp => (Roots, Roots),
            _ => return,
        };
        if args.len() < 4 {
            // The signature table has already reported the shape of the call.
            return;
        }

        // "The zeros argument may be represented as a null argument."
        let num = match args.get(1) {
            Some(&arg) if !matches!(self.parent.body.exprs[arg], Expr::Missing) => {
                match self.const_real_column(arg) {
                    Some(num) => Some(num),
                    None => {
                        self.zi_err(
                            expr,
                            name,
                            if num_side == Roots { "zeros" } else { "numerator" },
                        );
                        return;
                    }
                }
            }
            _ => None,
        };
        let den = match self.const_real_column(args[2]) {
            Some(den) => den,
            None => {
                self.zi_err(expr, name, if den_side == Roots { "poles" } else { "denominator" });
                return;
            }
        };

        // 4.5.14: a constant expression "remains static throughout an analysis",
        // which a parameter does, and `T` and `t0` only feed the scheduling
        // arithmetic rather than any coefficient, so they are lowered as values and
        // need not be known here. A literal period can still be checked.
        if let Some(period) = self.const_real(args[3]) {
            if period <= 0.0 {
                self.operator_err(
                    expr,
                    name,
                    format!("the sampling period is {period}; 4.5.12 requires T to be positive"),
                );
                return;
            }
        }

        if let Err(err) = zi_filter::Filter::build(num.as_deref(), num_side, &den, den_side) {
            self.operator_err(expr, name, err.0);
        }
    }

    fn zi_err(&mut self, expr: ExprId, name: &'static str, what: &str) {
        self.operator_err(
            expr,
            name,
            format!(
                "the {what} vector has to be an array of constants. Table 4-20 makes it a \
                 constant expression argument, and the filter is compiled into the model, so an \
                 array the module computes at run time cannot be read here: write it as an array \
                 literal"
            ),
        );
    }

    fn validate_builtin(
        &mut self,
        name: &Option<Path>,
        expr: ExprId,
        mut args: &[ExprId],
        call: BuiltIn,
        signature: Option<Signature>,
    ) {
        match call {
            _ if call.is_unsupported() => self
                .parent
                .diagnostics
                .push(BodyValidationDiagnostic::UnsupportedFunction { expr, func: call }),
            BuiltIn::table_model => self.validate_table_model(expr, args),
            BuiltIn::zi_nd | BuiltIn::zi_np | BuiltIn::zi_zd | BuiltIn::zi_zp => {
                self.validate_zi_filter(expr, call, args)
            }
            BuiltIn::sscanf | BuiltIn::fscanf => self.validate_scan(args),
            BuiltIn::discontinuity => {
                // The `$discontinuity(-1)` form is part of `$limit` (9.17.3) and is
                // lowered; every other degree is dropped, so say so rather than let
                // a model claim a discontinuity that never reaches the integrator.
                let is_limit_form =
                    args.first().is_some_and(|&arg| self.as_signed_int(arg) == Some(-1));
                if !is_limit_form {
                    self.parent.diagnostics.push(BodyValidationDiagnostic::IgnoredDiscontinuity {
                        stmt: self.stmt,
                        expr,
                    });
                }
            }
            BuiltIn::potential | BuiltIn::flow => self.check_access(
                |_| IllegalCtxAccessKind::NatureAccess,
                expr,
                self.parent.ctx.allow_nature_access(),
            ),

            _ if call.is_analog_operator() && call != BuiltIn::ddx
                || call.is_analog_operator_sysfun() =>
            {
                // let non_const_dominator = if self.cond_diagnostic_sink.is_none() {
                // self.parent.non_const_dominator.clone()
                // } else {
                // vec![].into_boxed_slice()
                // };

                self.check_access(
                    |sel| IllegalCtxAccessKind::AnalogOperator {
                        name: name.as_ref().and_then(|p| p.as_ident()).unwrap(),
                        is_standard: call.is_analog_operator(),
                        non_const_dominator: sel.parent.non_const_dominator.clone(),
                    },
                    expr,
                    self.parent.ctx.allow_analog_operator(),
                )
            }

            // VAMS-2023 5.10.3: event functions are not expressions; they may only
            // appear as the event expression of an event control.
            _ if call.is_event_fun() => {
                let allowed = self.parent.event_call == Some(expr);
                self.check_access(
                    |_| IllegalCtxAccessKind::EventFun {
                        name: name.as_ref().and_then(|p| p.as_ident()).unwrap(),
                    },
                    expr,
                    allowed,
                )
            }

            _ if call.is_analysis_var() && !self.parent.ctx.allow_analysis_fun() => self
                .report_illegal_access(
                    IllegalCtxAccessKind::AnalysisFun {
                        name: name.as_ref().and_then(|p| p.as_ident()).unwrap(),
                    },
                    expr,
                ),
            _ => (),
        }

        match (call, signature) {
            (BuiltIn::potential | BuiltIn::flow, Some(NATURE_ACCESS_NODES)) => {
                let hi = self.parent.infer.expr_types[args[0]].unwrap_node();
                let lo = self.parent.infer.expr_types[args[1]].unwrap_node();
                let branch = if hi >= lo {
                    BranchWrite::Unnamed { hi, lo: Some(lo) }
                } else {
                    BranchWrite::Unnamed { hi: lo, lo: Some(hi) }
                };
                self.lint_trivial_branch(branch, call, expr);
                if let Some(discipline) = self.validate_implicit_branch(expr, hi, lo) {
                    self.validate_flow_or_pot(expr, call, discipline)
                }
            }

            (BuiltIn::potential | BuiltIn::flow, Some(NATURE_ACCESS_NODE_GND)) => {
                let node = self.parent.infer.expr_types[args[0]].unwrap_node();
                if let Some(discipline) = self.parent.db.node_discipline(node) {
                    self.lint_trivial_branch(
                        BranchWrite::Unnamed { hi: node, lo: None },
                        call,
                        expr,
                    );
                    self.validate_flow_or_pot(expr, call, discipline)
                }
            }

            (BuiltIn::flow, Some(NATURE_ACCESS_PORT_FLOW)) => {
                let node = self.parent.infer.expr_types[args[0]].unwrap_port_flow();
                let node_data = self.parent.db.node_data(node);
                if !(node_data.is_input | node_data.is_output) {
                    self.report(BodyValidationDiagnostic::ExpectedPort { node, expr })
                }

                if let Some(discipline) = self.parent.db.node_discipline(node) {
                    self.validate_flow_or_pot(expr, BuiltIn::flow, discipline)
                }
            }

            (BuiltIn::potential, Some(NATURE_ACCESS_PORT_FLOW)) => {
                self.report(BodyValidationDiagnostic::PotentialOfPortFlow { expr, branch: None })
            }

            (BuiltIn::potential | BuiltIn::flow, Some(NATURE_ACCESS_BRANCH)) => {
                let branch = self.parent.infer.expr_types[args[0]].unwrap_branch();

                if let Some(branch_info) = self.parent.db.branch_info(branch) {
                    match branch_info.kind {
                        BranchKind::PortFlow(_) => {
                            if call == BuiltIn::potential {
                                self.report(BodyValidationDiagnostic::PotentialOfPortFlow {
                                    expr,
                                    branch: Some(branch),
                                })
                            } else if !self.write {
                                self.validate_flow_or_pot(
                                    expr,
                                    BuiltIn::flow,
                                    branch_info.discipline,
                                )
                            }
                        }
                        BranchKind::NodeGnd(node) => {
                            self.lint_trivial_branch(
                                BranchWrite::Unnamed { hi: node, lo: None },
                                call,
                                expr,
                            );
                            self.validate_flow_or_pot(expr, call, branch_info.discipline)
                        }
                        BranchKind::Nodes(hi, lo) => {
                            let branch = if hi >= lo {
                                BranchWrite::Unnamed { hi, lo: Some(lo) }
                            } else {
                                BranchWrite::Unnamed { hi: lo, lo: Some(hi) }
                            };
                            self.lint_trivial_branch(branch, call, expr);
                            self.validate_flow_or_pot(expr, call, branch_info.discipline)
                        }
                    }
                }
            }

            (BuiltIn::port_connected, _) => {
                let node = self.parent.infer.expr_types[args[0]].unwrap_node();
                let node_data = self.parent.db.node_data(node);
                if !(node_data.is_input | node_data.is_output) {
                    self.report(BodyValidationDiagnostic::ExpectedPort { node, expr })
                }
            }

            (
                BuiltIn::noise_table | BuiltIn::noise_table_log,
                Some(NOISE_TABLE_INLINE | NOISE_TABLE_INLINE_NAME),
            ) => self.validate_const_expr(args[0]),
            (func @ (BuiltIn::simparam | BuiltIn::simparam_str), _) => {
                if self.parent.ctx == BodyCtx::Const {
                    let known = if let Expr::Literal(Literal::String(name)) =
                        &self.parent.body.exprs[args[0]]
                    {
                        matches!(
                            (func, &**name),
                            (
                                BuiltIn::simparam,
                                "minr"
                                    | "imelt"
                                    | "shrink"
                                    | "imax"
                                    | "rthresh"
                                    | "scale"
                                    | "simulatorSubversion"
                                    | "simulatorVersion"
                                    | "tnom"
                            ) | (BuiltIn::simparam_str, "cwd" | "module" | "instance" | "path")
                        )
                    } else {
                        false
                    };

                    self.report(BodyValidationDiagnostic::ConstSimparam {
                        known,
                        expr,
                        stmt: self.stmt,
                    });
                }
            }

            // NOTE: `transition` is deliberately absent. VAMS-2023 Table 4-20
            // (Mantis 7810) lists all of its arguments - including `time_tol` -
            // as dynamic expressions; only `absdelay`'s `maxdelay`, `ddt`'s and
            // `idt`/`idtmod`'s `abstol` are still constant expressions.
            (BuiltIn::absdelay, Some(ABSDELAY_MAX))
            | (BuiltIn::ddt, Some(DDT_TOL))
            | (BuiltIn::idt | BuiltIn::idtmod, Some(IDT_IC_ASSERT_TOL)) => {
                if let [other_args @ .., const_expr] = args {
                    // Do not type check const expr twice
                    args = other_args;
                    self.validate_const_expr(*const_expr);
                };
            }

            (
                // laplace_nd accepts runtime-computed coefficient arrays (realized as
                // a state-space filter), so its coefficient args are not const-checked.
                BuiltIn::laplace_np
                | BuiltIn::laplace_zp
                | BuiltIn::laplace_zd
                | BuiltIn::zi_nd
                | BuiltIn::zi_np
                | BuiltIn::zi_zd
                | BuiltIn::zi_zp,
                Some(_),
            ) => {
                if let [_expr, const_args @ ..] = args {
                    args = &args[..1];
                    for arg in const_args {
                        self.validate_const_expr(*arg)
                    }
                }
            }

            _ => (),
        }

        for arg in args {
            self.validate_expr(*arg)
        }
    }

    fn validate_const_expr(&mut self, expr: ExprId) {
        let old = replace(&mut self.parent.ctx, BodyCtx::Const);
        let sink = self.cond_diagnostic_sink.take();
        self.validate_expr(expr);
        self.cond_diagnostic_sink = sink;
        self.parent.ctx = old;
    }
}
