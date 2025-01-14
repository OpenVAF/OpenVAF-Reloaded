use std::mem::replace;
use std::vec;

use ahash::AHashMap;
use bitset::BitSet;
use hir::{BranchWrite, CompilationDB, Node, ParamSysFun};
use hir_lower::{CurrentKind, HirInterner, ImplicitEquation, ParamKind};
use indexmap::IndexSet;
use mir::builder::InstBuilder;
use mir::cursor::{Cursor, FuncCursor};
use mir::{
    strip_optbarrier, Block, ControlFlowGraph, DominatorTree, Inst, KnownDerivatives, Unknown,
    Value, FALSE, F_ONE, F_ZERO, TRUE,
};
use mir_autodiff::auto_diff;
use typed_index_collections::TiVec;

use crate::context::Context;
use crate::dae::{DaeSystem, MatrixEntry, Residual, SimUnknown};
use crate::noise::NoiseSource;
use crate::topology::{BranchInfo, Contribution};
use crate::util::{add, is_op_dependent, update_optbarrier};
use crate::SimUnknownKind;

impl Residual {
    fn add(&mut self, cursor: &mut FuncCursor, negate: bool, mut val: Value) {
        // Cursor points at MIR function
        // Go back and skip all optbarriers to get the first actual instruction producing val
        val = strip_optbarrier(&cursor, val);
        // Add or subtract val to resistive residual value, replace resistive value by result
        add(cursor, &mut self.resist, val, negate);
    }

    fn add_contribution(&mut self, contrib: &Contribution, cursor: &mut FuncCursor, negate: bool) {
        let mut add = |residual: &mut Value, contrib| {
            // Cursor points at MIR function
            // Go back and skip all optbarriers to get the first actual instruction producing contrib
            let contrib = strip_optbarrier(&mut *cursor, contrib);
            // Add/subtract contrib to/from residual, replace residual with result
            add(cursor, residual, contrib, negate)
        };
        add(&mut self.resist, contrib.resist);
        add(&mut self.react, contrib.react);
        add(&mut self.resist_small_signal, contrib.resist_small_signal);
        add(&mut self.react_small_signal, contrib.react_small_signal);
    }
}

macro_rules! get_residual {
    ($self: ident, $unknown: expr) => {{
        let unknown = $self.ensure_unknown($unknown);
        &mut $self.system.residual[unknown]
    }};
}

pub(super) struct Builder<'a> {
    pub(super) system: DaeSystem,
    pub(super) cursor: FuncCursor<'a>,
    pub(super) db: &'a CompilationDB,
    pub(super) intern: &'a mut HirInterner,
    pub(super) cfg: &'a mut ControlFlowGraph,
    pub(super) dom_tree: &'a mut DominatorTree,
    pub(super) op_dependent_insts: &'a BitSet<Inst>,
    pub(super) output_values: &'a mut BitSet<Value>,
}

impl<'a> Builder<'a> {
    pub(super) fn new(ctx: &'a mut Context) -> Self {
        ctx.compute_outputs(false);
        let mut builder = Self {
            system: DaeSystem::default(),
            cursor: FuncCursor::new(&mut ctx.func).at_exit(),
            db: ctx.db,
            intern: &mut ctx.intern,
            cfg: &mut ctx.cfg,
            dom_tree: &mut ctx.dom_tree,
            op_dependent_insts: &ctx.op_dependent_insts,
            output_values: &mut ctx.output_values,
        };

        // ensure ports are the first unknowns and always have an unknown
        for port in ctx.module.module.ports(builder.db) {
            builder.build_node(port)
        }

        for node in ctx.module.module.internal_nodes(builder.db) {
            builder.build_node(node)
        }

        builder
    }

    pub(super) fn finish(mut self) -> DaeSystem {
        let sim_unknown_reads = self.sim_unknown_reads();
        let derivative_info = self.intern.unknowns(&self.cursor, true);
        let extra_derivatives = self
            .jacobian_derivatives(sim_unknown_reads.iter().map(|&(_, val)| val), &derivative_info);
        // TODO(pref): incrementially update dom_tree (for switch branches) instead
        self.dom_tree.compute(self.cursor.func, self.cfg, true, false, true);
        let derivatives =
            auto_diff(&mut *self.cursor.func, self.dom_tree, &derivative_info, &extra_derivatives);
        drop(extra_derivatives);
        // auto_diff may in an unlikely case add extra bb at the end, ensure we are building everything at the end
        self.cursor.goto_exit();

        self.build_jacobian(&sim_unknown_reads, &derivative_info, &derivatives);
        self.build_lim_rhs(&derivative_info, derivatives);
        self.ensure_optbarriers();

        self.build_input_unknown_pairs();

        let (nres, nreact) = self.count_jacobian_entries();
        self.system.num_resistive = nres;
        self.system.num_reactive = nreact;

        self.system
    }

    pub(super) fn build_node(&mut self, node: Node) {
        self.ensure_unknown(SimUnknownKind::KirchoffLaw(node));
    }

    pub(super) fn with_small_signal_network(
        mut self,
        small_signal_parameters: IndexSet<Value, ahash::RandomState>,
    ) -> Self {
        self.system.small_signal_parameters = small_signal_parameters;
        self
    }

    /// Return a list of all parameters that read from one of the simulation
    /// unknowns and therefore need to be considered during matrix construction.
    /// These need to be constructed from the list of parameters instead of the list
    /// of sim unknowns because voltage probes access two node voltages at the same time:
    ///
    /// V(x, y) = V(x) - V(y)
    ///
    /// We derive by these voltage differences to reduce the number of generated derivatives.
    fn sim_unknown_reads(&self) -> Vec<(ParamKind, Value)> {
        self.intern
            .live_params(&self.cursor.func.dfg)
            .filter_map(move |(_, &kind, param)| {
                if matches!(
                    kind,
                    ParamKind::Voltage { .. }
                        | ParamKind::Current(_)
                        | ParamKind::ImplicitUnknown(_)
                ) {
                    Some((kind, param))
                } else {
                    None
                }
            })
            .collect()
    }

    // Create a list of input node pairs corresponding to all model inputs
    fn build_input_unknown_pairs(&mut self) {
        self.system.model_inputs.clear();
        for (_, &kind, _) in self.intern.live_params(&self.cursor.func.dfg) {
            match kind {
                ParamKind::Voltage { hi, lo } => {
                    let mut ih = std::u32::MAX;
                    let mut il = std::u32::MAX;
                    let uh = SimUnknownKind::KirchoffLaw(hi);
                    if let Some(uh) = self.system.unknowns.index(&uh) {
                        ih = u32::from(uh);
                    }
                    if let Some(lo) = lo {
                        let ul = SimUnknownKind::KirchoffLaw(lo);
                        if let Some(ul) = self.system.unknowns.index(&ul) {
                            il = u32::from(ul);
                        }
                    }
                    if ih != std::u32::MAX && il != std::u32::MAX {
                        self.system.model_inputs.push((ih, il));
                    }
                }
                ParamKind::Current(cur_kind) => {
                    match cur_kind {
                        CurrentKind::Port(_) => {
                            // TODO?
                        }
                        _ => {
                            let u = SimUnknownKind::Current(cur_kind);
                            if let Some(u) = self.system.unknowns.index(&u) {
                                self.system.model_inputs.push((u32::from(u), std::u32::MAX));
                            }
                        }
                    }
                }
                ParamKind::ImplicitUnknown(ieq_kind) => {
                    let u = SimUnknownKind::Implicit(ieq_kind);
                    if let Some(u) = self.system.unknowns.index(&u) {
                        self.system.model_inputs.push((u32::from(u), std::u32::MAX));
                    }
                }
                _ => {}
            }
        }
    }

    fn count_jacobian_entries(&mut self) -> (u32, u32) {
        // Count resistive and reactive Jacobian entries
        let mut nres: u32 = 0;
        let mut nreact: u32 = 0;
        for key in self.system.jacobian.keys() {
            if self.system.jacobian[key].resist != F_ZERO {
                nres = nres + 1;
            }

            if self.system.jacobian[key].react != F_ZERO {
                nreact = nreact + 1;
            }
        }
        (nres, nreact)
    }

    fn build_lim_rhs(
        &mut self,
        derivative_info: &KnownDerivatives,
        derivatives: AHashMap<(Value, Unknown), Value>,
    ) {
        for residual in &mut self.system.residual {
            for (state, (unchanged, lim_vals)) in self.intern.lim_state.iter_enumerated() {
                for &(val, neg) in lim_vals {
                    let unknown = if let Some(unknown) = derivative_info.unknowns.index(&val) {
                        unknown
                    } else {
                        continue;
                    };
                    let changed = HirInterner::ensure_param_(
                        &mut self.intern.params,
                        &mut self.cursor,
                        ParamKind::NewState(state),
                    );

                    let delta = if neg {
                        self.cursor.ins().fadd(changed, *unchanged)
                    } else {
                        self.cursor.ins().fsub(changed, *unchanged)
                    };
                    let mut add_lim_rhs = |dst, residual, residual_small_signal| {
                        let mut ddx =
                            derivatives.get(&(residual, unknown)).copied().unwrap_or(F_ZERO);
                        let ddx_small_signal = derivatives
                            .get(&(residual_small_signal, unknown))
                            .copied()
                            .unwrap_or(F_ZERO);
                        add(&mut self.cursor, &mut ddx, ddx_small_signal, false);
                        if ddx != F_ZERO && delta != F_ZERO {
                            let rhs = self.cursor.ins().fmul(ddx, delta);
                            add(&mut self.cursor, dst, rhs, false);
                        }
                    };
                    add_lim_rhs(
                        &mut residual.resist_lim_rhs,
                        residual.resist,
                        residual.resist_small_signal,
                    );
                    add_lim_rhs(
                        &mut residual.react_lim_rhs,
                        residual.react,
                        residual.react_small_signal,
                    );
                }
            }
        }
    }

    fn build_jacobian(
        &mut self,
        sim_unknown_reads: &[(ParamKind, Value)],
        derivative_info: &KnownDerivatives,
        derivatives: &AHashMap<(Value, Unknown), Value>,
    ) {
        self.system.jacobian =
            TiVec::with_capacity(self.system.unknowns.len() * self.system.unknowns.len());

        //  construct the matrix by creating a dense row and then sparsifying
        let mut dense_row = TiVec::from(vec![(F_ZERO, F_ZERO); self.system.unknowns.len()]);
        let mut add = |matrix_entry: &mut Value, residual, unknown, negate| {
            if let Some(ddx) = derivatives.get(&(residual, unknown)).copied() {
                add(&mut self.cursor, matrix_entry, ddx, negate)
            }
        };

        for (row, residual) in self.system.residual.iter_enumerated() {
            // construct the dense row
            let mut add_residual = |sim_unknown: SimUnknownKind, unknown, negate| {
                let sim_unknown = if let Some(unknown) = self.system.unknowns.index(&sim_unknown) {
                    unknown
                } else {
                    return;
                };
                let (resist, react) = &mut dense_row[sim_unknown];
                if let Some(lim_vals) = self.intern.lim_state.raw.get(&unknown) {
                    for (val, negate_lim) in lim_vals {
                        let lim_unknown = if let Some(it) = derivative_info.unknowns.index(val) {
                            it
                        } else {
                            continue;
                        };
                        add(resist, residual.resist, lim_unknown, negate != *negate_lim);
                        add(
                            resist,
                            residual.resist_small_signal,
                            lim_unknown,
                            negate != *negate_lim,
                        );
                        add(react, residual.react, lim_unknown, negate != *negate_lim);
                        add(react, residual.react_small_signal, lim_unknown, negate != *negate_lim);
                    }
                }

                if let Some(unknown) = derivative_info.unknowns.index(&unknown) {
                    add(resist, residual.resist, unknown, negate);
                    add(resist, residual.resist_small_signal, unknown, negate);
                    add(react, residual.react, unknown, negate);
                    add(react, residual.react_small_signal, unknown, negate);
                }
            };
            for &(kind, val) in sim_unknown_reads {
                let unknown = match kind {
                    ParamKind::Voltage { hi, lo } => {
                        if let Some(lo) = lo {
                            add_residual(SimUnknownKind::KirchoffLaw(lo), val, true);
                        }
                        SimUnknownKind::KirchoffLaw(hi)
                    }
                    ParamKind::ImplicitUnknown(equation) => SimUnknownKind::Implicit(equation),
                    ParamKind::Current(kind) => SimUnknownKind::Current(kind),
                    _ => continue,
                };
                add_residual(unknown, val, false);
            }

            // sparsify the row
            for (col, (resist, react)) in &mut dense_row.iter_mut_enumerated() {
                if *resist == F_ZERO && *react == F_ZERO {
                    continue;
                }
                self.system.jacobian.push(MatrixEntry {
                    row,
                    col,
                    resist: replace(resist, F_ZERO),
                    react: replace(react, F_ZERO),
                });
            }
        }
    }

    pub fn jacobian_derivatives(
        &self,
        simulation_unknown: impl Iterator<Item = Value>,
        derivatives: &KnownDerivatives,
    ) -> Vec<(Value, Unknown)> {
        let mut params: Vec<_> =
            simulation_unknown.filter_map(|param| derivatives.unknowns.index(&param)).collect();
        let lim_derivatives = self.intern.lim_state.raw.values().flat_map(|vals| {
            vals.iter().filter_map(|(val, _)| {
                if self.cursor.func.dfg.value_dead(*val) {
                    return None;
                }
                derivatives.unknowns.index(val)
            })
        });
        params.extend(lim_derivatives);

        let small_signal_params = self
            .system
            .small_signal_parameters
            .iter()
            .filter_map(|&param| derivatives.unknowns.index(&param));

        let num_unknowns = params.len() * self.system.residual.len() * 2;
        let mut res = Vec::with_capacity(num_unknowns);
        for residual in &self.system.residual {
            if self.cursor.func.dfg.value_def(residual.resist).as_const().is_none() {
                res.extend(params.iter().map(|unknown| (residual.resist, *unknown)))
            }
            if self.cursor.func.dfg.value_def(residual.react).as_const().is_none() {
                res.extend(params.iter().map(|unknown| (residual.react, *unknown)))
            }
            if self.cursor.func.dfg.value_def(residual.resist_small_signal).as_const().is_none() {
                res.extend(
                    small_signal_params
                        .clone()
                        .map(|unknown| (residual.resist_small_signal, unknown)),
                )
            }
            if self.cursor.func.dfg.value_def(residual.react_small_signal).as_const().is_none() {
                res.extend(
                    small_signal_params
                        .clone()
                        .map(|unknown| (residual.react_small_signal, unknown)),
                )
            }
        }
        res
    }

    pub(super) fn build_branch(&mut self, branch: BranchWrite, contributions: &BranchInfo) {
        let current = branch.into();
        // contributions.is_voltage_src is a Value that is used for choosing the branch type (voltage, current)
        match contributions.is_voltage_src {
            // If it is constant FALSE; this is a current branch
            FALSE => {
                // if the current of the branch is probed we need to create an extra
                // branch
                let requires_unknown =
                    self.intern.is_param_live(&self.cursor, &ParamKind::Current(current));
                let contrib = self.current_branch(contributions);
                if requires_unknown {
                    self.add_source_equation(
                        &contrib,
                        // &contributions.current_src,
                        contributions.current_src.unknown.unwrap(),
                        branch,
                    );
                } else {
                    self.add_kirchoff_law(&contrib, branch);
                    // self.add_kirchoff_law(&contributions.current_src, branch);
                }
            }
            // If it is constant TRUE; this is a voltage branch
            TRUE => {
                // branches only used for node collapsing look like pure current
                // sources, make sure to ignore these branches
                let requires_unknown =
                    self.intern.is_param_live(&self.cursor, &ParamKind::Current(current));
                if requires_unknown || !contributions.voltage_src.is_trivial() {
                    let contrib = self.voltage_branch(contributions);
                    self.add_source_equation(
                        &contrib,
                        contributions.current_src.unknown.unwrap(),
                        branch,
                    );
                }
            }

            // Otherwise this is a switch branch
            _ => {
                let requires_current_unknown = !self
                    .cursor
                    .as_ref()
                    .dfg
                    .value_dead(contributions.current_src.unknown.unwrap());
                let op_dependent = is_op_dependent(
                    &self.cursor,
                    contributions.is_voltage_src,
                    self.op_dependent_insts,
                    self.intern,
                );
                // most cases that look like switch branches are just node collapsing
                // so make sure we don't crate switch branches when they aren't needed
                if op_dependent
                    || requires_current_unknown
                    || !contributions.voltage_src.is_trivial()
                {
                    // An actual switch branch
                    let start_bb = self.cursor.current_block().unwrap();
                    let voltage_src_bb = self.cursor.layout_mut().append_new_block();
                    let next_block = self.cursor.layout_mut().append_new_block();
                    self.cfg.ensure_bb(next_block);
                    self.cfg.add_edge(start_bb, voltage_src_bb);
                    self.cfg.add_edge(start_bb, next_block);
                    self.cfg.add_edge(voltage_src_bb, next_block);

                    // Debugging
                    // println!("start bb {:?}", start_bb);
                    // println!("voltage src bb {:?}", voltage_src_bb);
                    // println!("next block {:?}", next_block);
                    // println!("cursor at {:?}", self.cursor.position());

                    // Get expression (condition) that determines if branch acts as a voltage source
                    // Skip trailing optbarriers
                    let is_voltage_src =
                        strip_optbarrier(&self.cursor, contributions.is_voltage_src);
                    // Insert branch command (after?) condition
                    // If condition is true, jump to voltage_src_bb block
                    // If false go to next_block
                    self.cursor.ins().br(is_voltage_src, voltage_src_bb, next_block);
                    // Go to the end of voltage_src_bb block
                    self.cursor.goto_bottom(voltage_src_bb);
                    // Insert jump command to next_block
                    self.cursor.ins().jump(next_block);
                    // Go to the end of next_block
                    self.cursor.goto_bottom(next_block);
                    let contrib = self.switch_branch(contributions, voltage_src_bb, start_bb);
                    self.add_source_equation(
                        &contrib,
                        contributions.current_src.unknown.unwrap(),
                        branch,
                    )
                } else {
                    // Not a real switch branch
                    let contrib = self.current_branch(contributions);
                    self.add_kirchoff_law(&contrib, branch);
                }
            }
        };
    }

    pub(super) fn build_implicit_equation(&mut self, eq: ImplicitEquation, contrib: &Contribution) {
        get_residual!(self, SimUnknownKind::Implicit(eq)).add_contribution(
            contrib,
            &mut self.cursor,
            false,
        );
    }

    fn mfactor_multiply(&mut self, mfactor: Value, srcfactor: Value) -> Value {
        match (mfactor, srcfactor) {
            // Leave srcfactor unchanged if mfactor is 1
            (F_ONE, fac) => fac,
            // mfactor is not 1
            // Note that srcfactor is the signal scaling factor.
            // Because power scales with mfactor the signal scales with
            // sqrt(mfactor).
            (mfactor, srcfactor) => {
                let sqrt_mfactor = self.cursor.ins().sqrt(mfactor);
                if srcfactor == F_ONE {
                    // Old factor is 1, replace it with sqrt(mfactor)
                    sqrt_mfactor
                } else {
                    // Multiply old factor with sqrt(mfactor)
                    self.cursor.ins().fmul(srcfactor, sqrt_mfactor)
                }
            }
        }
    }

    fn mfactor_divide(&mut self, mfactor: Value, srcfactor: Value) -> Value {
        match (mfactor, srcfactor) {
            // Leave srcfactor unchanged if mfactor is 1
            (F_ONE, fac) => fac,
            // mfactor is not 1
            // Note that srcfactor is the signal scaling factor.
            // Because power scales with mfactor the signal scales with
            // sqrt(mfactor).
            (mfactor, srcfactor) => {
                let sqrt_mfactor = self.cursor.ins().sqrt(mfactor);
                self.cursor.ins().fdiv(srcfactor, sqrt_mfactor)
            }
        }
    }

    fn current_branch(&mut self, BranchInfo { current_src, .. }: &BranchInfo) -> Contribution {
        let mfactor = self
            .intern
            .ensure_param(&mut self.cursor, ParamKind::ParamSysFun(ParamSysFun::mfactor));
        let mut noise = Vec::with_capacity(current_src.noise.len());
        let current_noise = current_src.noise.iter().map(|src| {
            let mut src = src.clone();
            src.factor = self.mfactor_multiply(mfactor, src.factor);
            src
        });
        noise.extend(current_noise);

        Contribution {
            unknown: current_src.unknown,
            resist: current_src.resist,
            react: current_src.react,
            resist_small_signal: current_src.resist_small_signal,
            react_small_signal: current_src.react_small_signal,
            noise,
        }
    }

    fn voltage_branch(&mut self, BranchInfo { voltage_src, .. }: &BranchInfo) -> Contribution {
        let mfactor = self
            .intern
            .ensure_param(&mut self.cursor, ParamKind::ParamSysFun(ParamSysFun::mfactor));
        let mut noise = Vec::with_capacity(voltage_src.noise.len());
        let voltage_noise = voltage_src.noise.iter().map(|src| {
            let mut src = src.clone();
            src.factor = self.mfactor_divide(mfactor, src.factor);
            src
        });
        noise.extend(voltage_noise);

        Contribution {
            unknown: voltage_src.unknown,
            resist: voltage_src.resist,
            react: voltage_src.react,
            resist_small_signal: voltage_src.resist_small_signal,
            react_small_signal: voltage_src.react_small_signal,
            noise,
        }
    }

    fn switch_branch(
        &mut self,
        BranchInfo { voltage_src, current_src, .. }: &BranchInfo,
        voltage_bb: Block,
        current_bb: Block,
    ) -> Contribution {
        let mut select = |voltage_src_val, current_src_val| {
            let voltage_src_val = strip_optbarrier(&self.cursor, voltage_src_val);
            let current_src_val = strip_optbarrier(&self.cursor, current_src_val);
            if voltage_src_val == current_src_val {
                voltage_src_val
            } else {
                self.cursor
                    .ins()
                    .phi(&[(current_bb, current_src_val), (voltage_bb, voltage_src_val)])
            }
        };

        let voltage = voltage_src.unknown.unwrap();
        let current = current_src.unknown.unwrap();
        let unknown = select(voltage, current);
        // Build noise phi commands
        // Voltage noise, for each noise add a phi instruction that joins the values for
        // the case the switch branch behaves as a voltage source (source value) and as a current source (0)
        let mut noise = Vec::with_capacity(voltage_src.noise.len() + current_src.noise.len());
        let voltage_noise = voltage_src.noise.iter().map(|src| {
            let mut src = src.clone();
            src.factor = select(src.factor, F_ZERO);
            src
        });
        noise.extend(voltage_noise);
        // Current noise, for each noise add a phi instruction that joins the values for
        // the case the switch branch behaves as a voltage source (0) and as a current source (source value)
        let current_noise = current_src.noise.iter().map(|src| {
            let mut src = src.clone();
            src.factor = select(F_ZERO, src.factor);
            src
        });
        noise.extend(current_noise);
        // Build remaining phi commands
        let phi_resist = select(voltage_src.resist, current_src.resist);
        let phi_react = select(voltage_src.react, current_src.react);
        let phi_resist_ss =
            select(voltage_src.resist_small_signal, current_src.resist_small_signal);
        let phi_react_ss = select(voltage_src.react_small_signal, current_src.react_small_signal);
        // Scale noise
        // Must do this after all phi commands
        // because all phi commands must be listed at block beginning
        let mfactor = self
            .intern
            .ensure_param(&mut self.cursor, ParamKind::ParamSysFun(ParamSysFun::mfactor));
        for ii in 0..voltage_src.noise.len() + current_src.noise.len() {
            if ii < voltage_src.noise.len() {
                // Voltage noise
                noise[ii].factor = self.mfactor_divide(mfactor, noise[ii].factor);
            } else {
                // Current noise
                noise[ii].factor = self.mfactor_multiply(mfactor, noise[ii].factor);
            }
        }

        Contribution {
            unknown: Some(unknown),
            resist: phi_resist,
            react: phi_react,
            resist_small_signal: phi_resist_ss,
            react_small_signal: phi_react_ss,
            noise,
        }
    }

    fn ensure_unknown(&mut self, unknown: SimUnknownKind) -> SimUnknown {
        let (unknown, new) = self.system.unknowns.ensure(unknown);
        if new {
            self.system.residual.push(Residual::default());
        }
        unknown
    }

    fn add_noise(
        &mut self,
        contrib: &Contribution,
        hi: SimUnknownKind,
        lo: Option<SimUnknownKind>,
    ) {
        let hi = self.ensure_unknown(hi);
        let lo = lo.map(|lo| self.ensure_unknown(lo));
        self.system.noise_sources.extend(contrib.noise.iter().map(|src| {
            let factor = src.factor;
            NoiseSource { name: src.name, kind: src.kind.clone(), hi, lo, factor }
        }))
    }

    fn add_kirchoff_law(&mut self, contrib: &Contribution, dst: BranchWrite) {
        let (hi, lo) = dst.nodes(self.db);
        let hi = SimUnknownKind::KirchoffLaw(hi);
        let lo = lo.map(SimUnknownKind::KirchoffLaw);
        get_residual!(self, hi).add_contribution(contrib, &mut self.cursor, false);
        if let Some(lo) = lo {
            get_residual!(self, lo).add_contribution(contrib, &mut self.cursor, true);
        }
        // self.add_noise(contrib, hi, lo, true);
        self.add_noise(contrib, hi, lo);
    }

    fn add_source_equation(&mut self, contrib: &Contribution, eq_val: Value, dst: BranchWrite) {
        let residual = get_residual!(self, SimUnknownKind::Current(dst.into()));
        residual.add_contribution(contrib, &mut self.cursor, false);
        residual.add(&mut self.cursor, true, contrib.unknown.unwrap());
        // self.add_noise(contrib, SimUnknownKind::Current(dst.into()), None, false);
        self.add_noise(contrib, SimUnknownKind::Current(dst.into()), None);

        let (hi, lo) = dst.nodes(self.db);
        let hi = SimUnknownKind::KirchoffLaw(hi);
        let lo = lo.map(SimUnknownKind::KirchoffLaw);
        get_residual!(self, hi).add(&mut self.cursor, false, eq_val);
        if let Some(lo) = lo {
            get_residual!(self, lo).add(&mut self.cursor, true, eq_val);
        }
    }

    /// multiply each residual and matrix entry with mfactor and ensure it has
    /// a optbarrier
    pub(super) fn ensure_optbarriers(&mut self) {
        let mfactor = self
            .intern
            .ensure_param(&mut self.cursor, ParamKind::ParamSysFun(ParamSysFun::mfactor));
        let mut ensure_optbarrier = |mut val, is_kirchoff_law| {
            val = self.cursor.ins().ensure_optbarrier(val);
            if is_kirchoff_law && val != F_ZERO {
                update_optbarrier(self.cursor.func, &mut val, |val, cursor| {
                    cursor.ins().fmul(mfactor, val)
                })
            }
            self.output_values.ensure(self.cursor.func.dfg.num_values());
            self.output_values.insert(val);
            val
        };
        for (unknown, residual) in &mut self.system.residual.iter_mut_enumerated() {
            // we purpusfully ignore small signal values here since they never contribute the residual
            residual.react_small_signal = F_ZERO;
            residual.react_small_signal = F_ZERO;
            let is_kirchoff =
                matches!(self.system.unknowns[unknown], SimUnknownKind::KirchoffLaw(_));
            residual.map_vals(|val| ensure_optbarrier(val, is_kirchoff));
        }
        ensure_optbarrier(mfactor, false);

        for noise_src in &mut self.system.noise_sources {
            noise_src.map_vals(|val| ensure_optbarrier(val, false));
        }

        for entry in &mut self.system.jacobian {
            let is_kirchoff =
                matches!(self.system.unknowns[entry.row], SimUnknownKind::KirchoffLaw(_));
            entry.resist = ensure_optbarrier(entry.resist, is_kirchoff);
            entry.react = ensure_optbarrier(entry.react, is_kirchoff);
        }
    }
}
