use core::ffi::c_uint;
use std::ptr::NonNull;

use llvm_sys::core::{
    LLVMAppendBasicBlockInContext, LLVMBuildCall2, LLVMBuildFAdd, LLVMBuildFCmp, LLVMBuildFDiv,
    LLVMBuildFMul, LLVMBuildFSub, LLVMBuildGEP2, LLVMBuildRetVoid, LLVMBuildSelect, LLVMBuildStore,
    LLVMCreateBuilderInContext, LLVMDisposeBuilder, LLVMGetParam, LLVMPositionBuilderAtEnd,
};
use llvm_sys::LLVMRealPredicate;
use mir_llvm::UNNAMED;
use sim_back::dae::NoiseSourceKind;
use stdx::iter::zip;
use typed_index_collections::TiVec;

use crate::compilation_unit::OsdiCompilationUnit;
#[derive(Debug, Clone, Copy)]
pub enum JacobianLoadType {
    Tran,
    Resist,
    React,
}

impl JacobianLoadType {
    const fn dst_reactive(self) -> bool {
        matches!(self, JacobianLoadType::React)
    }

    const fn read_resistive(self) -> bool {
        matches!(self, JacobianLoadType::Resist | JacobianLoadType::Tran)
    }

    const fn read_reactive(self) -> bool {
        matches!(self, JacobianLoadType::React | JacobianLoadType::Tran)
    }

    const fn name(self) -> &'static str {
        match self {
            JacobianLoadType::Tran => "tran",
            JacobianLoadType::Resist => "resist",
            JacobianLoadType::React => "react",
        }
    }
}

impl<'ll> OsdiCompilationUnit<'_, '_, 'll> {
    /// Emit the LLVM IR that evaluates a `noise_table`/`noise_table_log` power
    /// spectral density at run time for a given `freq`.
    ///
    /// `vals` are the sorted `(x, power)` pairs produced by
    /// `hir_lower::NoiseTable::new`, where `x` is already in `log10(frequency)`
    /// space (linear-input tables are `log10`-ed at build time; `_log` tables
    /// are stored as-is). The lookup key is therefore `log10(freq)`, and the
    /// power is obtained by piecewise-linear interpolation over `x`, clamped to
    /// the table's endpoints outside `[x[0], x[n-1]]`.
    ///
    /// The interpolation is fully unrolled into `select`s: every segment's
    /// slope/intercept is a compile-time constant, so each segment costs one
    /// `fmul`, one `fadd`, one `fcmp` and one `select`.
    unsafe fn build_noise_table_interp(
        &self,
        llbuilder: llvm_sys::prelude::LLVMBuilderRef,
        freq: llvm_sys::prelude::LLVMValueRef,
        vals: &[(stdx::Ieee64, stdx::Ieee64)],
    ) -> &'ll llvm_sys::LLVMValue {
        let cx = self.cx;
        let n = vals.len();
        if n == 0 {
            return cx.const_real(0.0);
        }
        let x: Vec<f64> = vals.iter().map(|v| f64::from(v.0)).collect();
        let y: Vec<f64> = vals.iter().map(|v| f64::from(v.1)).collect();
        if n == 1 {
            return cx.const_real(y[0]);
        }

        // lx = log10(freq)
        let (log_ty, log_fn) = self
            .cx
            .intrinsic("llvm.log10.f64")
            .unwrap_or_else(|| unreachable!("intrinsic llvm.log10.f64 not found"));
        let mut log_args: [llvm_sys::prelude::LLVMValueRef; 1] = [freq];
        let lx = LLVMBuildCall2(
            llbuilder,
            NonNull::from(log_ty).as_ptr(),
            NonNull::from(log_fn).as_ptr(),
            log_args.as_mut_ptr(),
            1,
            UNNAMED,
        );

        // Default (lx >= x[n-1]): clamp to the last point.
        let mut result = NonNull::from(cx.const_real(y[n - 1])).as_ptr();

        // Walk the segments from the top down. After the loop, the surviving
        // `select` is the one for the lowest segment whose upper bound exceeds
        // `lx`, i.e. exactly the bracketing segment.
        for i in (0..n - 1).rev() {
            let slope = (y[i + 1] - y[i]) / (x[i + 1] - x[i]);
            let intercept = y[i] - slope * x[i];
            // seg = slope * lx + intercept
            let seg =
                LLVMBuildFMul(llbuilder, NonNull::from(cx.const_real(slope)).as_ptr(), lx, UNNAMED);
            let seg = LLVMBuildFAdd(
                llbuilder,
                seg,
                NonNull::from(cx.const_real(intercept)).as_ptr(),
                UNNAMED,
            );
            let cond = LLVMBuildFCmp(
                llbuilder,
                LLVMRealPredicate::LLVMRealOLT,
                lx,
                NonNull::from(cx.const_real(x[i + 1])).as_ptr(),
                UNNAMED,
            );
            result = LLVMBuildSelect(llbuilder, cond, seg, result, UNNAMED);
        }

        // Clamp below x[0] to the first point (otherwise segment 0 would
        // extrapolate below the table).
        let cond0 = LLVMBuildFCmp(
            llbuilder,
            LLVMRealPredicate::LLVMRealOLT,
            lx,
            NonNull::from(cx.const_real(x[0])).as_ptr(),
            UNNAMED,
        );
        result = LLVMBuildSelect(
            llbuilder,
            cond0,
            NonNull::from(cx.const_real(y[0])).as_ptr(),
            result,
            UNNAMED,
        );

        &*result
    }

    pub fn load_noise(&self) -> &'ll llvm_sys::LLVMValue {
        let OsdiCompilationUnit { cx, module, .. } = self;
        let void_ptr = cx.ty_ptr();
        let f64_ptr_ty = cx.ty_ptr();
        let fun_ty = cx.ty_func(&[void_ptr, void_ptr, cx.ty_double(), f64_ptr_ty], cx.ty_void());
        let name = &format!("load_noise_{}", module.sym);
        let llfunc = cx.declare_int_c_fn(name, fun_ty);

        unsafe {
            let entry = LLVMAppendBasicBlockInContext(
                NonNull::from(cx.llcx).as_ptr(),
                NonNull::from(llfunc).as_ptr(),
                UNNAMED,
            );
            let llbuilder = LLVMCreateBuilderInContext(NonNull::from(cx.llcx).as_ptr());
            LLVMPositionBuilderAtEnd(llbuilder, entry);
            let inst = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 0);
            let model = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 1);
            let freq = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 2);
            let dst = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 3);

            for (i, (src, eval_outputs)) in
                zip(&module.dae_system.noise_sources, &self.inst_data.noise).enumerate()
            {
                let fac = self.load_eval_output(eval_outputs.factor, &*inst, &*model, &*llbuilder);
                let mut pwr = match src.kind {
                    NoiseSourceKind::WhiteNoise { .. } => {
                        self.load_eval_output(eval_outputs.args[0], &*inst, &*model, &*llbuilder)
                    }
                    NoiseSourceKind::FlickerNoise { .. } => {
                        let mut pwr = self.load_eval_output(
                            eval_outputs.args[0],
                            &*inst,
                            &*model,
                            &*llbuilder,
                        );
                        let exp = &*self.load_eval_output(
                            eval_outputs.args[1],
                            &*inst,
                            &*model,
                            &*llbuilder,
                        );
                        let (ty, fun) = self
                            .cx
                            .intrinsic("llvm.pow.f64")
                            .unwrap_or_else(|| unreachable!("intrinsic {} not found", name));

                        let freq_val = freq as *const llvm_sys::LLVMValue as *mut _;
                        let exp_val = &*exp as *const llvm_sys::LLVMValue as *mut _;
                        let mut call_args: [llvm_sys::prelude::LLVMValueRef; 2] =
                            [freq_val, exp_val];
                        let args_ptr = call_args.as_mut_ptr();

                        let freq_exp = LLVMBuildCall2(
                            llbuilder,
                            NonNull::from(ty).as_ptr(),
                            NonNull::from(fun).as_ptr(),
                            args_ptr,
                            2,
                            UNNAMED,
                        );
                        let fast_math_flags: c_uint = 0x01 | 0x02 | 0x10; // Reassoc | Reciprocal | Contract
                        llvm_sys::core::LLVMSetFastMathFlags(freq_exp, fast_math_flags);

                        pwr = &*LLVMBuildFDiv(
                            llbuilder,
                            NonNull::from(pwr).as_ptr(),
                            freq_exp,
                            UNNAMED,
                        );
                        let fast_math_flags: c_uint = 0x1F; // This represents all flags set
                        llvm_sys::core::LLVMSetFastMathFlags(
                            NonNull::from(pwr).as_ptr(),
                            fast_math_flags,
                        );

                        pwr
                    }
                    NoiseSourceKind::NoiseTable { ref vals, .. } => {
                        let freq_ptr = freq as *const llvm_sys::LLVMValue as *mut _;
                        self.build_noise_table_interp(llbuilder, freq_ptr, vals)
                    }
                };

                // Multiply with squared factor because factor is in terms of signal, but
                // we are computing the power, which is scaled by factor**2.
                pwr = &*LLVMBuildFMul(
                    llbuilder,
                    NonNull::from(pwr).as_ptr(),
                    NonNull::from(fac).as_ptr(),
                    UNNAMED,
                );
                let fast_math_flags: c_uint = 0x1F; // This represents all flags set
                llvm_sys::core::LLVMSetFastMathFlags(NonNull::from(pwr).as_ptr(), fast_math_flags);
                pwr = &*LLVMBuildFMul(
                    llbuilder,
                    NonNull::from(pwr).as_ptr(),
                    NonNull::from(fac).as_ptr(),
                    UNNAMED,
                );
                llvm_sys::core::LLVMSetFastMathFlags(NonNull::from(pwr).as_ptr(), fast_math_flags);
                let index_val =
                    cx.const_unsigned_int(i as u32) as *const llvm_sys::LLVMValue as *mut _;
                let mut gep_indices: [llvm_sys::prelude::LLVMValueRef; 1] = [index_val];
                let gep_ptr = gep_indices.as_mut_ptr();

                let dst = LLVMBuildGEP2(
                    llbuilder,
                    NonNull::from(cx.ty_double()).as_ptr(),
                    dst,
                    gep_ptr,
                    1,
                    UNNAMED,
                );
                LLVMBuildStore(llbuilder, NonNull::from(pwr).as_ptr(), dst);
            }

            // TODO noise
            LLVMBuildRetVoid(llbuilder);
            LLVMDisposeBuilder(llbuilder);
        }

        llfunc
    }

    pub fn load_noise_params(&self) -> &'ll llvm_sys::LLVMValue {
        let OsdiCompilationUnit { cx, module, .. } = self;
        let void_ptr = cx.ty_ptr();
        let f64_ptr_ty = cx.ty_ptr();
        let fun_ty = cx.ty_func(&[void_ptr, void_ptr, f64_ptr_ty, f64_ptr_ty], cx.ty_void());
        let name = &format!("load_noise_params_{}", module.sym);
        let llfunc = cx.declare_int_c_fn(name, fun_ty);

        unsafe {
            let entry = LLVMAppendBasicBlockInContext(
                NonNull::from(cx.llcx).as_ptr(),
                NonNull::from(llfunc).as_ptr(),
                UNNAMED,
            );
            let llbuilder = LLVMCreateBuilderInContext(NonNull::from(cx.llcx).as_ptr());
            LLVMPositionBuilderAtEnd(llbuilder, entry);
            let inst = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 0);
            let model = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 1);
            let dst_dens = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 2);
            let dst_exp = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 3);

            for (i, (src, eval_outputs)) in
                zip(&module.dae_system.noise_sources, &self.inst_data.noise).enumerate()
            {
                // Factor and power
                let fac = self.load_eval_output(eval_outputs.factor, &*inst, &*model, &*llbuilder);
                let mut pwr = match src.kind {
                    NoiseSourceKind::WhiteNoise { .. } => {
                        self.load_eval_output(eval_outputs.args[0], &*inst, &*model, &*llbuilder)
                    }
                    NoiseSourceKind::FlickerNoise { .. } => {
                        self.load_eval_output(eval_outputs.args[0], &*inst, &*model, &*llbuilder)
                    }
                    // A frequency-dependent table has no single scalar power; the
                    // per-frequency `load_noise` entry point is the real evaluator
                    // (this ABI slot is unused by ngspice's OSDI noise path).
                    NoiseSourceKind::NoiseTable { .. } => cx.const_real(0.0),
                };

                // Multiply with squared factor because factor is in terms of signal, but
                // we are computing the power, which is scaled by factor**2.
                pwr = &*LLVMBuildFMul(
                    llbuilder,
                    NonNull::from(pwr).as_ptr(),
                    NonNull::from(fac).as_ptr(),
                    UNNAMED,
                );
                let fast_math_flags: c_uint = 0x1F; // This represents all flags set
                llvm_sys::core::LLVMSetFastMathFlags(NonNull::from(pwr).as_ptr(), fast_math_flags);
                pwr = &*LLVMBuildFMul(
                    llbuilder,
                    NonNull::from(pwr).as_ptr(),
                    NonNull::from(fac).as_ptr(),
                    UNNAMED,
                );
                llvm_sys::core::LLVMSetFastMathFlags(NonNull::from(pwr).as_ptr(), fast_math_flags);

                // Exponent
                let exp = match src.kind {
                    NoiseSourceKind::WhiteNoise { .. } => cx.const_real(0.0),
                    NoiseSourceKind::FlickerNoise { .. } => {
                        self.load_eval_output(eval_outputs.args[1], &*inst, &*model, &*llbuilder)
                    }
                    NoiseSourceKind::NoiseTable { .. } => cx.const_real(0.0),
                };

                // Store power
                let index_val =
                    cx.const_unsigned_int(i as u32) as *const llvm_sys::LLVMValue as *mut _;
                let mut gep_indices: [llvm_sys::prelude::LLVMValueRef; 1] = [index_val];
                let gep_ptr = gep_indices.as_mut_ptr();

                let dst_dens = LLVMBuildGEP2(
                    llbuilder,
                    NonNull::from(cx.ty_double()).as_ptr(),
                    dst_dens,
                    gep_ptr,
                    1,
                    UNNAMED,
                );
                LLVMBuildStore(llbuilder, NonNull::from(pwr).as_ptr(), dst_dens);

                // Store exponent
                let index_val =
                    cx.const_unsigned_int(i as u32) as *const llvm_sys::LLVMValue as *mut _;
                let mut gep_indices: [llvm_sys::prelude::LLVMValueRef; 1] = [index_val];
                let gep_ptr = gep_indices.as_mut_ptr();

                let dst_exp = LLVMBuildGEP2(
                    llbuilder,
                    NonNull::from(cx.ty_double()).as_ptr(),
                    dst_exp,
                    gep_ptr,
                    1,
                    UNNAMED,
                );
                LLVMBuildStore(llbuilder, NonNull::from(exp).as_ptr(), dst_exp);
            }

            // TODO noise
            LLVMBuildRetVoid(llbuilder);
            LLVMDisposeBuilder(llbuilder);
        }

        llfunc
    }

    pub fn load_residual(&self, reactive: bool) -> &'ll llvm_sys::LLVMValue {
        let OsdiCompilationUnit { inst_data, cx, module, .. } = self;
        let ptr_ty = cx.ty_ptr();
        let fun_ty = cx.ty_func(&[ptr_ty, ptr_ty, ptr_ty], cx.ty_void());
        let name =
            &format!("load_residual_{}_{}", if reactive { "react" } else { "resist" }, module.sym);
        let llfunc = cx.declare_int_c_fn(name, fun_ty);

        unsafe {
            let entry = LLVMAppendBasicBlockInContext(
                NonNull::from(cx.llcx).as_ptr(),
                NonNull::from(llfunc).as_ptr(),
                UNNAMED,
            );
            let llbuilder = LLVMCreateBuilderInContext(NonNull::from(cx.llcx).as_ptr());

            LLVMPositionBuilderAtEnd(llbuilder, entry);

            // get params
            let inst = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 0);
            let dst = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 2);

            for node in module.dae_system.unknowns.indices() {
                if let Some(contrib) = inst_data.read_residual(node, inst, &*llbuilder, reactive) {
                    inst_data.store_contrib(cx, node, inst, dst, contrib, &*llbuilder, false);
                }
            }

            LLVMBuildRetVoid(llbuilder);
            LLVMDisposeBuilder(llbuilder);
        }

        llfunc
    }

    pub fn load_lim_rhs(&self, reactive: bool) -> &'ll llvm_sys::LLVMValue {
        let OsdiCompilationUnit { inst_data, cx, module, .. } = self;
        let void_ptr = cx.ty_ptr();
        let f64_ptr_ty = cx.ty_ptr();
        let fun_ty = cx.ty_func(&[void_ptr, void_ptr, f64_ptr_ty], cx.ty_void());
        let name =
            &format!("load_lim_rhs_{}_{}", if reactive { "react" } else { "resist" }, module.sym);
        let llfunc = cx.declare_int_c_fn(name, fun_ty);

        unsafe {
            let entry = LLVMAppendBasicBlockInContext(
                NonNull::from(cx.llcx).as_ptr(),
                NonNull::from(llfunc).as_ptr(),
                UNNAMED,
            );
            let llbuilder = LLVMCreateBuilderInContext(NonNull::from(cx.llcx).as_ptr());

            LLVMPositionBuilderAtEnd(llbuilder, entry);

            // get params
            let inst = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 0);
            let dst = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 2);

            for node in module.dae_system.unknowns.indices() {
                if let Some(contrib) = inst_data.read_lim_rhs(node, inst, &*llbuilder, reactive) {
                    inst_data.store_contrib(cx, node, inst, dst, contrib, &*llbuilder, true);
                }
            }

            LLVMBuildRetVoid(llbuilder);
            LLVMDisposeBuilder(llbuilder);
        }

        llfunc
    }

    #[allow(clippy::too_many_arguments)]
    pub fn load_spice_rhs_(
        &self,
        tran: bool,
        llbuilder: &llvm_sys::LLVMBuilder,
        inst: &'ll llvm_sys::LLVMValue,
        model: &'ll llvm_sys::LLVMValue,
        dst: &'ll llvm_sys::LLVMValue,
        prev_solve: &'ll llvm_sys::LLVMValue,
        alpha: &'ll llvm_sys::LLVMValue,
    ) {
        let dae_system = &self.module.dae_system;
        let mut node_derivatives = TiVec::from(vec![Vec::new(); dae_system.unknowns.len()]);
        for (id, entry) in dae_system.jacobian.iter_enumerated() {
            node_derivatives[entry.row].push(id)
        }

        unsafe {
            for node in dae_system.unknowns.indices() {
                let mut res = None;
                for &entry in &node_derivatives[node] {
                    let node_deriv = dae_system.jacobian[entry].col;
                    let ddx = if let Some(ddx) =
                        self.load_jacobian_entry(entry, inst, model, llbuilder, tran)
                    {
                        ddx
                    } else {
                        continue;
                    };

                    let voltage = self
                        .inst_data
                        .read_node_voltage(self.cx, node_deriv, inst, prev_solve, llbuilder);
                    let val = LLVMBuildFMul(
                        NonNull::from(llbuilder).as_ptr(),
                        NonNull::from(ddx).as_ptr(),
                        NonNull::from(voltage).as_ptr(),
                        UNNAMED,
                    );
                    let fast_math_flags: c_uint = 0x1F; // This represents all flags set
                    llvm_sys::core::LLVMSetFastMathFlags(val, fast_math_flags);

                    res = match res {
                        Some(old) => {
                            let val =
                                LLVMBuildFAdd(NonNull::from(llbuilder).as_ptr(), old, val, UNNAMED);
                            let fast_math_flags: c_uint = 0x1F; // This represents all flags set
                            llvm_sys::core::LLVMSetFastMathFlags(val, fast_math_flags);

                            Some(val)
                        }
                        None => Some(val),
                    }
                }

                let OsdiCompilationUnit { inst_data, cx, .. } = self;
                if !tran {
                    if let Some(contrib) = inst_data.read_residual(node, inst, llbuilder, false) {
                        let val = LLVMBuildFSub(
                            NonNull::from(llbuilder).as_ptr(),
                            res.unwrap_or_else(|| NonNull::from(cx.const_real(0.0)).as_ptr()),
                            NonNull::from(contrib).as_ptr(),
                            UNNAMED,
                        );
                        let fast_math_flags: c_uint = 0x1F; // This represents all flags set
                        llvm_sys::core::LLVMSetFastMathFlags(val, fast_math_flags);
                        res = Some(val);
                    }
                }
                if let Some(mut res) = res {
                    if let Some(lim_rhs) = inst_data.read_lim_rhs(node, inst, llbuilder, tran) {
                        res = LLVMBuildFAdd(
                            NonNull::from(llbuilder).as_ptr(),
                            res,
                            NonNull::from(lim_rhs).as_ptr(),
                            UNNAMED,
                        );
                    }
                    if tran {
                        res = LLVMBuildFMul(
                            NonNull::from(llbuilder).as_ptr(),
                            res,
                            NonNull::from(alpha).as_ptr(),
                            UNNAMED,
                        );
                        let fast_math_flags: c_uint = 0x1F; // This represents all flags set
                        llvm_sys::core::LLVMSetFastMathFlags(res, fast_math_flags);
                    }
                    inst_data.store_contrib(cx, node, inst, dst, &*res, llbuilder, false);
                }
            }
        }
    }

    pub fn load_spice_rhs(&self, tran: bool) -> &'ll llvm_sys::LLVMValue {
        let OsdiCompilationUnit { cx, module, .. } = self;
        let f64_ty = cx.ty_double();
        let ptr_ty = cx.ty_ptr();
        let mut args = vec![ptr_ty, ptr_ty, ptr_ty, ptr_ty];
        if tran {
            args.push(f64_ty);
        }
        let fun_ty = cx.ty_func(&args, cx.ty_void());
        let name = &format!("load_spice_rhs_{}_{}", if tran { "tran" } else { "dc" }, &module.sym);
        let llfunc = cx.declare_int_c_fn(name, fun_ty);

        unsafe {
            let entry = LLVMAppendBasicBlockInContext(
                NonNull::from(cx.llcx).as_ptr(),
                NonNull::from(llfunc).as_ptr(),
                UNNAMED,
            );
            let llbuilder = LLVMCreateBuilderInContext(NonNull::from(cx.llcx).as_ptr());
            LLVMPositionBuilderAtEnd(llbuilder, entry);

            // get params
            let inst = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 0);
            let model = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 1);
            let dst = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 2);
            let prev_solve = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 3);
            let alpha =
                if tran { &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 4) } else { prev_solve };

            self.load_spice_rhs_(false, &*llbuilder, inst, model, dst, prev_solve, alpha);
            if tran {
                self.load_spice_rhs_(true, &*llbuilder, inst, model, dst, prev_solve, alpha);
            }

            LLVMBuildRetVoid(llbuilder);
            LLVMDisposeBuilder(llbuilder);
        }

        llfunc
    }

    pub fn load_jacobian(
        &self,
        kind: JacobianLoadType,
        with_offset: bool,
    ) -> &'ll llvm_sys::LLVMValue {
        let OsdiCompilationUnit { cx, module, .. } = *self;
        let fun_ty = if !with_offset {
            if kind.read_reactive() {
                cx.ty_func(&[cx.ty_ptr(), cx.ty_ptr(), cx.ty_double()], cx.ty_void())
            } else {
                cx.ty_func(&[cx.ty_ptr(), cx.ty_ptr()], cx.ty_void())
            }
        } else {
            // with_offset assumes alpha=1 for the reactive Jacobian loader
            cx.ty_func(&[cx.ty_ptr(), cx.ty_ptr(), cx.ty_size()], cx.ty_void())
        };
        let name = if with_offset {
            format!("load_jacobian_with_offset_{}_{}", kind.name(), &module.sym,)
        } else {
            format!("load_jacobian_{}_{}", kind.name(), &module.sym,)
        };
        let llfunc = cx.declare_int_c_fn(&name, fun_ty);

        unsafe {
            let entry = LLVMAppendBasicBlockInContext(
                NonNull::from(cx.llcx).as_ptr(),
                NonNull::from(llfunc).as_ptr(),
                UNNAMED,
            );
            let llbuilder = LLVMCreateBuilderInContext(NonNull::from(cx.llcx).as_ptr());

            LLVMPositionBuilderAtEnd(llbuilder, entry);
            // Get params
            let inst = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 0);
            let model = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 1);
            let alpha = if !with_offset && kind.read_reactive() {
                // Reactive part
                LLVMGetParam(NonNull::from(llfunc).as_ptr(), 2)
            } else {
                // Some dummy
                inst
            };
            let offset = if with_offset {
                LLVMGetParam(NonNull::from(llfunc).as_ptr(), 2)
            } else {
                // Some dummy
                inst
            };

            for entry in module.dae_system.jacobian.keys() {
                let mut res = None;
                if kind.read_resistive() {
                    res = self.load_jacobian_entry(entry, &*inst, &*model, &*llbuilder, false);
                }

                if kind.read_reactive() {
                    if let Some(mut val) =
                        self.load_jacobian_entry(entry, &*inst, &*model, &*llbuilder, true)
                    {
                        // with_offset assumes alpha=1
                        if !with_offset {
                            val = &*LLVMBuildFMul(
                                llbuilder,
                                NonNull::from(val).as_ptr(),
                                alpha,
                                UNNAMED,
                            );
                            let fast_math_flags: c_uint = 0x1F; // This represents all flags set
                            llvm_sys::core::LLVMSetFastMathFlags(
                                NonNull::from(val).as_ptr(),
                                fast_math_flags,
                            );
                        }
                        val = match res {
                            Some(resist) => {
                                let val = LLVMBuildFAdd(
                                    llbuilder,
                                    NonNull::from(resist).as_ptr(),
                                    NonNull::from(val).as_ptr(),
                                    UNNAMED,
                                );
                                let fast_math_flags: c_uint = 0x1F; // This represents all flags set
                                llvm_sys::core::LLVMSetFastMathFlags(val, fast_math_flags);

                                &*val
                            }
                            None => val,
                        };
                        res = Some(val)
                    }
                }

                if let Some(res) = res {
                    self.inst_data.store_jacobian_contrib(
                        self.cx,
                        entry,
                        &*inst,
                        &*llbuilder,
                        kind.dst_reactive(),
                        with_offset,
                        &*offset,
                        res,
                    );
                }
            }

            LLVMBuildRetVoid(llbuilder);
            LLVMDisposeBuilder(llbuilder);
        }

        llfunc
    }

    // write_jacobian_array_{resist|react|tran}(void* instance, void* model, double* destination [, alpha])
    // Writes Jacobian entries into a double array of size num_jacobian_entries
    // If a particular entry is not present, nothing is loaded.
    // Array of doubles need not be zeroed before calling this function.
    pub fn write_jacobian_array(&self, kind: JacobianLoadType) -> &'ll llvm_sys::LLVMValue {
        let OsdiCompilationUnit { cx, module, .. } = *self;
        let args = [cx.ty_ptr(), cx.ty_ptr(), cx.ty_ptr()];
        let fun_ty = cx.ty_func(&args, cx.ty_void());
        let name = &format!("write_jacobian_array_{}_{}", kind.name(), &module.sym,);
        let llfunc = cx.declare_int_c_fn(name, fun_ty);

        unsafe {
            let entry = LLVMAppendBasicBlockInContext(
                NonNull::from(cx.llcx).as_ptr(),
                NonNull::from(llfunc).as_ptr(),
                UNNAMED,
            );
            let llbuilder = LLVMCreateBuilderInContext(NonNull::from(cx.llcx).as_ptr());

            LLVMPositionBuilderAtEnd(llbuilder, entry);
            // get params
            let inst = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 0);
            let model = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 1);
            let dest_array = LLVMGetParam(NonNull::from(llfunc).as_ptr(), 2);

            // Destination array type
            let len = {
                if kind.read_resistive() {
                    module.dae_system.num_resistive
                } else {
                    module.dae_system.num_reactive
                }
            };
            let dest_ty = cx.ty_array(cx.ty_double(), len as u32);

            let mut pos: u32 = 0;
            for entry in module.dae_system.jacobian.keys() {
                let res = {
                    if kind.read_resistive() {
                        // Load resistive Jacobian value from instance structure
                        self.load_jacobian_entry(entry, &*inst, &*model, &*llbuilder, false)
                    } else {
                        // Load reactive Jacobian value from instance structure
                        self.load_jacobian_entry(entry, &*inst, &*model, &*llbuilder, true)
                    }
                };

                // Do we have any result in res
                if let Some(res) = res {
                    // Store it in array pointed to by ptr
                    self.inst_data.write_jacobian_contrib(
                        self.cx,
                        pos,
                        dest_ty,
                        NonNull::new_unchecked(dest_array).as_ref(),
                        NonNull::new_unchecked(llbuilder).as_ref(),
                        res,
                    );
                    pos = pos + 1;
                }
            }

            LLVMBuildRetVoid(llbuilder);
            LLVMDisposeBuilder(llbuilder);
        }

        llfunc
    }
}
