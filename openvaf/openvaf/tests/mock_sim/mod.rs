use std::cell::UnsafeCell;
use std::mem::swap;
use std::ptr;

use anyhow::Result;
use indexmap::IndexSet;
use libc::c_void;
use stdx::iter::zip;

pub const ALPHA: f64 = 0.172;

use crate::load::{
    osdi_str, EvalFlags, EvalRetFlags, OsdiInstance, OsdiModel, OsdiSimInfo, OsdiSimParas,
};

/// One `absdelay` as the descriptor describes it, together with the history the
/// protocol makes the simulator's business (VAMS-2023 4.5.7).
///
/// The model computes the delay and writes it into its instance data; everything
/// here -- remembering the input, reading it back at `t - td`, and stamping the
/// output row, which the model deliberately leaves empty -- is the simulator's half
/// of the contract. This is the smallest faithful implementation of it, so that the
/// compiler's half can be tested without a circuit simulator.
#[derive(Debug, Default)]
pub struct AbsDelay {
    input: (u32, u32),
    output: u32,
    delay_offset: u32,
    max_delay_offset: u32,
    /// Accepted timepoints, oldest first.
    history: Vec<(f64, f64)>,
    /// The point being evaluated. Not part of the history until time moves on,
    /// because until then the solver may still reject it.
    at: f64,
    input_now: f64,
    started: bool,
    /// What the last evaluation read out of the instance data.
    pub delay: f64,
    pub max_delay: Option<f64>,
    /// The delayed input the output row was stamped with.
    pub delayed: f64,
    /// `d(delayed)/d(input)`: the weight the live interval contributes.
    pub slope: f64,
}

impl AbsDelay {
    /// Record this evaluation and read the history back at `t - td`.
    fn update(&mut self, time: f64, input: f64, delay: f64, max_delay: Option<f64>) {
        self.delay = delay;
        self.max_delay = max_delay;
        if !self.started {
            self.started = true;
            self.at = time;
            self.input_now = input;
        } else if time != self.at {
            // Time moving on means the solver accepted the point that was being
            // evaluated, so it joins the history. Time moving *back* means it
            // rejected it, so everything from the new time onwards is dropped.
            self.history.retain(|&(at, _)| at < time);
            if self.at < time {
                self.history.push((self.at, self.input_now));
            }
            self.at = time;
        }
        self.input_now = input;

        // Nothing older than the window can be asked for again. This is what
        // `maxdelay` is for: without it the delay is fixed, so the window is too.
        let window = max_delay.unwrap_or(delay);
        while self.history.len() > 1 && self.history[1].0 <= time - window {
            self.history.remove(0);
        }

        let target = time - delay;
        self.slope = 0.0;
        if self.history.is_empty() || target >= self.at {
            // "In DC and operating point analyses, absdelay() returns the value of
            // its input", which is also the first point of a transient.
            self.delayed = input;
            self.slope = 1.0;
            return;
        }
        if target <= self.history[0].0 {
            // Older than anything recorded: "the output remains at input(0)".
            self.delayed = self.history[0].1;
            return;
        }
        let live = (self.at, self.input_now);
        for k in (0..self.history.len()).rev() {
            let (lo_at, lo_val) = self.history[k];
            if target < lo_at {
                continue;
            }
            let (hi_at, hi_val) = self.history.get(k + 1).copied().unwrap_or(live);
            let span = hi_at - lo_at;
            let frac = if span > 0.0 { (target - lo_at) / span } else { 0.0 };
            self.delayed = lo_val + frac * (hi_val - lo_val);
            // Only the live interval ends at the value being solved for.
            if k + 1 == self.history.len() {
                self.slope = frac;
            }
            return;
        }
        self.delayed = self.history[0].1;
    }
}

#[derive(Debug, Default)]
pub struct MockSimulation {
    /// reported to the model as `$abstime`
    pub time: f64,
    pub nodes: IndexSet<&'static str>,
    pub residual_resist: Vec<f64>,
    pub residual_react: Vec<f64>,
    pub solve: Vec<f64>,
    pub jacobian_info: IndexSet<(u32, u32)>,
    pub jacobian_resist: &'static [UnsafeCell<f64>],
    pub jacobian_react: &'static [UnsafeCell<f64>],
    pub state_1: Vec<f64>,
    pub state_2: Vec<f64>,
    pub noise_dense: Vec<f64>,
    pub absdelay: Vec<AbsDelay>,
}
impl MockSimulation {
    fn new() -> MockSimulation {
        MockSimulation {
            nodes: {
                let mut set = IndexSet::new();
                set.insert("gnd");
                set
            },
            residual_resist: vec![0.0],
            residual_react: vec![0.0],
            solve: vec![0.0],
            jacobian_info: {
                let mut set = IndexSet::new();
                set.insert((0, 0));
                set
            },
            jacobian_resist: &[],
            jacobian_react: &[],
            state_1: Vec::new(),
            state_2: Vec::new(),
            noise_dense: Vec::new(),
            absdelay: Vec::new(),
            time: 0.0,
        }
    }

    fn register_node(&mut self, name: &'static str) -> u32 {
        let (idx, changed) = self.nodes.insert_full(name);
        assert!(changed);
        self.residual_resist.push(0.0);
        self.residual_react.push(0.0);
        self.solve.push(0.0);
        idx as u32
    }

    fn register_jacobian_entry(&mut self, hi: u32, lo: u32) {
        if hi != 0 && lo != 0 {
            self.jacobian_info.insert((hi, lo));
        }
    }

    fn get_jacobian_entry(&mut self, hi: u32, lo: u32) -> usize {
        if hi == 0 || lo == 0 {
            0
        } else {
            self.jacobian_info.get_index_of(&(hi, lo)).unwrap()
        }
    }

    pub fn read_noise(&mut self, src: usize) -> f64 {
        self.noise_dense[src]
    }

    pub fn set_voltage(&mut self, node: &str, voltage: f64) {
        let i = self.nodes.get_index_of(node).unwrap();
        self.solve[i] = voltage
    }

    pub fn read_residual(&self, node: &str) -> (f64, f64) {
        let i = self.nodes.get_index_of(node).unwrap();
        (self.residual_resist[i], self.residual_react[i])
    }

    /// The `absdelay` whose output is this node.
    pub fn absdelay_at(&self, node: &str) -> &AbsDelay {
        let idx = self.nodes.get_index_of(node).unwrap() as u32;
        self.absdelay
            .iter()
            .find(|delay| delay.output == idx)
            .expect("no absdelay drives this node")
    }

    pub fn read_jacobian(&self, hi: &str, lo: &str) -> (f64, f64) {
        let hi = self.nodes.get_index_of(hi).unwrap() as u32;
        let lo = self.nodes.get_index_of(lo).unwrap() as u32;
        let i = self.jacobian_info.get_index_of(&(hi, lo)).unwrap();
        unsafe { (self.jacobian_resist[i].get().read(), self.jacobian_react[i].get().read()) }
    }

    fn build_jacobian(&mut self) {
        self.jacobian_resist =
            (0..self.jacobian_info.len()).map(|_| UnsafeCell::new(0.0)).collect::<Vec<_>>().leak();
        self.jacobian_react =
            (0..self.jacobian_info.len()).map(|_| UnsafeCell::new(0.0)).collect::<Vec<_>>().leak();
    }

    pub(crate) fn clear(&mut self) {
        self.residual_resist.fill(0.0);
        self.residual_react.fill(0.0);
        for entry in self.jacobian_resist {
            unsafe { entry.get().write(0.0) };
        }
        for entry in self.jacobian_react {
            unsafe { entry.get().write(0.0) };
        }
    }

    pub(crate) fn next_iter(&mut self) {
        self.solve.fill(0.0);
        swap(&mut self.state_1, &mut self.state_2);
        self.clear();
    }

    /// Advance the simulation time reported to the model as `$abstime`. Anything
    /// that only happens once time has moved -- a monitored event, for one -- needs
    /// this as well as `next_iter`.
    pub(crate) fn advance_time(&mut self, dt: f64) {
        self.time += dt;
    }
}

impl OsdiInstance {
    pub(super) fn mock_simulation(
        &mut self,
        model: &OsdiModel,
        connected_terminals: u32,
        temp: f64,
    ) -> Result<MockSimulation> {
        let mut internal_nodes = self.process_params(model, connected_terminals, temp)?;
        let mut sim = MockSimulation::new();
        // create internal nodes
        let terminals: Vec<_> = self.descriptor.nodes()[..connected_terminals as usize]
            .iter()
            .map(|node| unsafe { sim.register_node(osdi_str(node.name)) })
            .collect();

        for node_idx in &mut internal_nodes {
            let node = &self.descriptor.nodes()[*node_idx as usize];
            *node_idx = unsafe { sim.register_node(osdi_str(node.name)) };
        }

        let node_mapping = self.node_mapping();
        for node in node_mapping {
            let idx = node.get();
            if let Some(&terminal) = terminals.get(idx as usize) {
                node.set(terminal)
            } else if idx == u32::MAX {
                node.set(0)
            } else {
                node.set(internal_nodes[idx as usize - terminals.len()])
            }
        }

        // create jacobian
        for entry in self.descriptor.matrix_entries() {
            let column = node_mapping[entry.nodes.node_1 as usize].get();
            let row = node_mapping[entry.nodes.node_2 as usize].get();
            sim.register_jacobian_entry(row, column);
        }
        // An `absdelay` output row is the simulator's to stamp, so its matrix entries
        // are not in the descriptor's list and have to be made here.
        let resolve = |node: u32| -> u32 {
            if node == u32::MAX {
                0
            } else {
                node_mapping[node as usize].get()
            }
        };
        for delay in self.descriptor.absdelay_entries() {
            let output = resolve(delay.output_node);
            let input = (resolve(delay.input_node_1), resolve(delay.input_node_2));
            sim.register_jacobian_entry(output, output);
            sim.register_jacobian_entry(output, input.0);
            sim.register_jacobian_entry(output, input.1);
            sim.absdelay.push(AbsDelay {
                input,
                output,
                delay_offset: delay.delay_offset,
                max_delay_offset: delay.max_delay_offset,
                ..AbsDelay::default()
            });
        }
        sim.build_jacobian();

        // populate matrix ptrs
        for (entry, ptr_resist) in zip(self.descriptor.matrix_entries(), self.matrix_ptrs_resist())
        {
            let column = node_mapping[entry.nodes.node_1 as usize].get();
            let row = node_mapping[entry.nodes.node_2 as usize].get();
            let i = sim.get_jacobian_entry(row, column);
            ptr_resist.set(sim.jacobian_resist[i].get());
            if entry.react_ptr_off != u32::MAX {
                let data = self.data as *mut u8;
                unsafe {
                    let react_ptr_ptr: *mut *mut f64 =
                        data.add(entry.react_ptr_off as usize).cast();
                    *react_ptr_ptr = sim.jacobian_react[i].get();
                }
            }
        }
        sim.state_1.resize(self.descriptor.num_states as usize, 0.0);
        sim.state_2.resize(self.descriptor.num_states as usize, 0.0);
        sim.noise_dense.resize(self.descriptor.num_noise_src as usize, 0.0);

        // Initialize the per-instance state_idx map (logical limit-state -> physical
        // slot in prev_state/next_state). A real simulator assigns these; with a
        // single state the default 0 happens to work, but with several they would all
        // alias slot 0. Use the identity mapping.
        unsafe {
            let data = self.data as *mut u8;
            let state_idx = data.add(self.descriptor.state_idx_off as usize).cast::<u32>();
            for i in 0..self.descriptor.num_states {
                state_idx.add(i as usize).write(i);
            }
        }
        Ok(sim)
    }

    pub fn load_spice(&self, model: &OsdiModel, sim: &mut MockSimulation) {
        self.descriptor.load_spice_rhs_tran(
            self.data,
            model.data,
            sim.residual_resist.as_mut_ptr(),
            sim.solve.as_mut_ptr(),
            ALPHA,
        );
        self.descriptor.load_jacobian_tran(self.data, self.data, ALPHA);
    }

    pub fn load_noise(&self, model: &OsdiModel, sim: &mut MockSimulation, freq: f64) {
        self.descriptor.load_noise(self.data, model.data, freq, sim.noise_dense.as_mut_ptr())
    }

    pub fn load_dae(&self, model: &OsdiModel, sim: &mut MockSimulation) {
        self.descriptor.load_residual_resist(
            self.data,
            model.data,
            sim.residual_resist.as_mut_ptr(),
        );
        self.descriptor.load_limit_rhs_resist(
            self.data,
            model.data,
            sim.residual_resist.as_mut_ptr(),
        );

        self.descriptor.load_residual_react(self.data, model.data, sim.residual_react.as_mut_ptr());
        self.descriptor.load_limit_rhs_react(
            self.data,
            model.data,
            sim.residual_react.as_mut_ptr(),
        );

        self.descriptor.load_jacobian_resist(self.data, model.data);
        self.descriptor.load_jacobian_react(self.data, model.data, 1.0);

        // The delayed-output rows, in the same form the model gives its own implicit
        // equations: the residual is `f(x)` for an `f` the solution drives to zero,
        // here `input(t - td) - output`.
        for i in 0..sim.absdelay.len() {
            let (output, input, delayed, slope) = {
                let delay = &sim.absdelay[i];
                (delay.output, delay.input, delay.delayed, delay.slope)
            };
            if output == 0 {
                continue;
            }
            sim.residual_resist[output as usize] += delayed - sim.solve[output as usize];
            let entries =
                [((output, output), -1.0), ((output, input.0), slope), ((output, input.1), -slope)];
            for ((row, column), val) in entries {
                if column == 0 {
                    continue;
                }
                let idx = sim.get_jacobian_entry(row, column);
                unsafe {
                    let ptr = sim.jacobian_resist[idx].get();
                    ptr.write(ptr.read() + val);
                }
            }
        }
    }
    pub fn eval(
        &self,
        model: &OsdiModel,
        sim: &mut MockSimulation,
        mut flags: EvalFlags,
    ) -> EvalRetFlags {
        // always calculate everything
        flags |= EvalFlags::CALC_RESIST_JACOBIAN
            | EvalFlags::CALC_RESIST_RESIDUAL
            | EvalFlags::CALC_RESIST_LIM_RHS
            | EvalFlags::CALC_REACT_JACOBIAN
            | EvalFlags::CALC_REACT_RESIDUAL
            | EvalFlags::CALC_REACT_LIM_RHS
            | EvalFlags::CALC_NOISE;
        let sim_params = OsdiSimParas {
            names: &mut ptr::null_mut(),
            vals: ptr::null_mut(),
            names_str: &mut ptr::null_mut(),
            vals_str: ptr::null_mut(),
        };
        let mut sim_info = OsdiSimInfo {
            paras: sim_params,
            abstime: sim.time,
            prev_solve: sim.solve.as_ptr() as *mut f64,
            prev_state: sim.state_1.as_mut_ptr(),
            next_state: sim.state_2.as_mut_ptr(),
            flags: flags.bits(),
        };
        let flags = self.descriptor.eval(
            b"foo\0".as_ptr() as *mut c_void,
            self.data,
            model.data,
            &mut sim_info,
        );
        // The model has just written each delay into its instance data; reading it
        // back and keeping the history is the simulator's half of the protocol.
        for i in 0..sim.absdelay.len() {
            let (in_hi, in_lo) = sim.absdelay[i].input;
            let input = sim.solve[in_hi as usize] - sim.solve[in_lo as usize];
            let delay = unsafe { self.read_instance_f64(sim.absdelay[i].delay_offset) };
            let max_delay_offset = sim.absdelay[i].max_delay_offset;
            let max_delay = (max_delay_offset != u32::MAX)
                .then(|| unsafe { self.read_instance_f64(max_delay_offset) });
            sim.absdelay[i].update(sim.time, input, delay, max_delay);
        }
        EvalRetFlags::from_bits(flags).unwrap()
    }

    /// Read one `double` out of the instance data at a descriptor-given offset.
    ///
    /// # Safety
    ///
    /// `offset` must be an offset the descriptor reported for an instance field of
    /// type `double`.
    unsafe fn read_instance_f64(&self, offset: u32) -> f64 {
        let ptr = (self.data as *const u8).add(offset as usize) as *const f64;
        ptr.read()
    }
}
