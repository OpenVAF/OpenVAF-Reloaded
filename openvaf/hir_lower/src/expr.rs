use hir::builtin::{
    FLICKER_NOISE_NAME, NOISE_TABLE_FILE, NOISE_TABLE_FILE_NAME, NOISE_TABLE_INLINE,
    NOISE_TABLE_INLINE_NAME, WHITE_NOISE_NAME,
};
use hir::signatures::{
    ABSDELAY_MAX, ABS_INT, ABS_REAL, BOOL_EQ, DDX_POT, IDTMOD_IC, IDTMOD_IC_MODULUS,
    IDTMOD_IC_MODULUS_OFFSET, IDTMOD_IC_MODULUS_OFFSET_NATURE, IDTMOD_IC_MODULUS_OFFSET_TOL,
    IDTMOD_NO_IC, IDT_IC, IDT_IC_ASSERT, IDT_IC_ASSERT_NATURE, IDT_IC_ASSERT_TOL, IDT_NO_IC,
    INT_EQ, INT_OP, LIMIT_BUILTIN_FUNCTION, MAX_INT, MAX_REAL, NATURE_ACCESS_BRANCH,
    NATURE_ACCESS_NODES, NATURE_ACCESS_NODE_GND, NATURE_ACCESS_PORT_FLOW, REAL_EQ, REAL_OP,
    SIMPARAM_DEFAULT, SIMPARAM_NO_DEFAULT, STR_EQ,
};
use hir::{Body, BuiltIn, Expr, ExprId, Literal, /*ParamSysFun,*/ Ref, ResolvedFun, Type};
use mir::builder::InstBuilder;
use mir::{Opcode, Value, FALSE, F_ZERO, GRAVESTONE, INFINITY, TRUE, ZERO};
use stdx::iter::zip;
use syntax::ast::{BinaryOp, UnaryOp};

use crate::body::BodyLoweringCtx;
use crate::fmt::DisplayKind;
use crate::{
    CallBackKind, CurrentKind, IdtKind, ImplicitEquationKind, NoiseTable, ParamKind, PlaceKind,
    RetFlag,
};

impl BodyLoweringCtx<'_, '_, '_> {
    pub fn lower_expr(&mut self, expr: ExprId) -> Value {
        let old_loc = self.ctx.get_srcloc();
        self.ctx.set_srcloc(mir::SourceLoc::new(u32::from(expr) as i32 + 1));

        let mut res = match self.body.get_expr(expr) {
            Expr::Read(Ref::Variable(var)) => self.ctx.read_variable(var),
            Expr::Read(Ref::ParamSysFun(param)) => {
                self.ctx.use_param(ParamKind::ParamSysFun(param))
            }
            Expr::Read(Ref::Parameter(param)) => self.ctx.use_param(ParamKind::Param(param)),
            Expr::Read(Ref::FunctionReturn(fun)) => {
                self.ctx.use_place(PlaceKind::FunctionReturn(fun))
            }
            Expr::Read(Ref::FunctionArg(fun)) => self.ctx.use_place(PlaceKind::FunctionArg(fun)),
            Expr::Read(Ref::NatureAttr(attr)) => self.lower_body(attr.value(self.ctx.db), 0),
            Expr::BinaryOp { lhs, rhs, op } => self.lower_bin_op(expr, lhs, rhs, op),
            Expr::UnaryOp { expr: arg, op } => self.lower_unary_op(expr, arg, op),
            Expr::Select { cond, then_val, else_val } => {
                let cond = self.lower_expr(cond);
                let (then_src, else_src) = self.lower_cond_with(cond, |mut ctx, then| {
                    let expr = if then { then_val } else { else_val };
                    ctx.lower_expr(expr)
                });

                self.ctx.ins().phi(&[then_src, else_src])
            }
            Expr::Index { base, index } => self.lower_index(base, index),
            Expr::Call { args, fun } => match fun {
                ResolvedFun::User { func, limit } => self.lower_user_fun(func, limit, args),
                ResolvedFun::BuiltIn(builtin) => self.lower_builtin(expr, builtin, args),
            },
            Expr::Array(vals) => self.lower_array(expr, vals),
            Expr::Literal(lit) => match *lit {
                Literal::String(ref str) => self.ctx.sconst(str),
                Literal::Int(val) => self.ctx.iconst(val),
                Literal::Float(val) => self.ctx.fconst(val.into()),
                Literal::Inf => {
                    self.ctx.set_srcloc(old_loc);
                    match self.body.expr_type(expr) {
                        Type::Real => return INFINITY,
                        Type::Integer => return self.ctx.iconst(i32::MAX),
                        _ => unreachable!(),
                    }
                }
            },
        };

        if let Some((src, dst)) = self.body.needs_cast(expr) {
            res = self.ctx.insert_cast(res, &src, dst)
        };
        self.ctx.set_srcloc(old_loc);
        res
    }

    fn lower_unary_op(&mut self, expr: ExprId, arg: ExprId, op: UnaryOp) -> Value {
        let is_inf = self.body.as_literal(arg) == Some(&Literal::Inf);
        let arg_ = self.lower_expr(arg);
        match op {
            UnaryOp::BitNegate => self.ctx.ins().ineg(arg_),
            UnaryOp::Not => self.ctx.ins().bnot(arg_),
            UnaryOp::Neg => {
                // Special case INFINITY
                if is_inf {
                    match self.body.expr_type(arg) {
                        Type::Real => return self.ctx.fconst(f64::NEG_INFINITY),
                        Type::Integer => return self.ctx.iconst(i32::MIN),
                        ty => unreachable!("{ty:?}"),
                    }
                }
                match self.body.get_call_signature(expr) {
                    REAL_OP => self.ctx.ins().fneg(arg_),
                    INT_OP => self.ctx.ins().ineg(arg_),
                    _ => unreachable!(),
                }
            }
            UnaryOp::Identity => arg_,
        }
    }

    fn lower_array(&mut self, _expr: ExprId, _args: &[ExprId]) -> Value {
        todo!("arrays")
    }
    fn lower_bin_op(&mut self, expr: ExprId, lhs: ExprId, rhs: ExprId, op: BinaryOp) -> Value {
        let signature = self.body.get_call_signature(expr);
        let op = match op {
            BinaryOp::BooleanOr => {
                // lhs || rhs if lhs { true } else { rhs }
                return self.lower_select(lhs, |_| TRUE, |mut s| s.lower_expr(rhs));
            }

            BinaryOp::BooleanAnd => {
                // lhs && rhs if lhs { rhs } else { false }
                return self.lower_select(lhs, |mut s| s.lower_expr(rhs), |_| FALSE);
            }

            BinaryOp::EqualityTest => match_signature! {
                signature:
                    BOOL_EQ => Opcode::Beq,
                    INT_EQ  => Opcode::Ieq,
                    REAL_EQ => Opcode::Feq,
                    STR_EQ  => Opcode::Seq
            },
            BinaryOp::NegatedEqualityTest => match_signature! {
                signature:
                    BOOL_EQ => Opcode::Bne,
                    INT_EQ  => Opcode::Ine,
                    REAL_EQ => Opcode::Fne,
                    STR_EQ  => Opcode::Sne
            },
            BinaryOp::GreaterEqualTest => {
                match_signature!(signature: INT_OP => Opcode::Ige, REAL_OP => Opcode::Fge)
            }
            BinaryOp::GreaterTest => {
                match_signature!(signature: INT_OP => Opcode::Igt, REAL_OP => Opcode::Fgt)
            }
            BinaryOp::LesserEqualTest => {
                match_signature!(signature: INT_OP => Opcode::Ile, REAL_OP => Opcode::Fle)
            }
            BinaryOp::LesserTest => {
                match_signature!(signature: INT_OP => Opcode::Ilt, REAL_OP => Opcode::Flt)
            }
            BinaryOp::Addition => {
                match_signature!(signature: INT_OP => Opcode::Iadd, REAL_OP => Opcode::Fadd)
            }
            BinaryOp::Subtraction => {
                match_signature!(signature: INT_OP => Opcode::Isub, REAL_OP => Opcode::Fsub)
            }
            BinaryOp::Multiplication => {
                match_signature!(signature: INT_OP => Opcode::Imul, REAL_OP => Opcode::Fmul)
            }
            BinaryOp::Division => {
                match_signature!(signature: INT_OP => Opcode::Idiv, REAL_OP => Opcode::Fdiv)
            }
            BinaryOp::Remainder => {
                match_signature!(signature: INT_OP => Opcode::Irem, REAL_OP => Opcode::Frem)
            }
            BinaryOp::Power => Opcode::Pow,

            BinaryOp::LeftShift => Opcode::Ishl,
            BinaryOp::RightShift => Opcode::Ishr,

            BinaryOp::BitwiseXor => Opcode::Ixor,
            BinaryOp::BitwiseEq => {
                let lhs = self.lower_expr(lhs);
                let rhs = self.lower_expr(rhs);
                let res = self.ctx.ins().ixor(lhs, rhs);
                return self.ctx.ins().inot(res);
            }
            BinaryOp::BitwiseOr => Opcode::Ior,
            BinaryOp::BitwiseAnd => Opcode::Iand,
        };

        let lhs_ = self.lower_expr(lhs);
        let rhs_ = self.lower_expr(rhs);
        self.ctx.ins().binary1(op, lhs_, rhs_)
    }

    fn lower_user_fun(&mut self, fun: hir::Function, lim: bool, args: &[ExprId]) -> Value {
        if lim {
            if self.ctx.no_equations {
                return self.lower_expr(args[0]);
            }
            let new_val = self.lower_expr(args[0]);
            let state = self.ctx.start_limit(new_val);
            let old_val = self.ctx.use_param(ParamKind::PrevState(state));
            let enable_lim = self.ctx.use_param(ParamKind::EnableLim);
            let res = self.lower_select_with(
                enable_lim,
                |mut cx| {
                    cx.ctx.def_place(PlaceKind::FunctionArg(fun.arg(0, self.ctx.db)), new_val);
                    cx.ctx.def_place(PlaceKind::FunctionArg(fun.arg(1, self.ctx.db)), old_val);
                    cx.lower_user_fun_impl(fun, args, true)
                },
                |_| new_val,
            );

            self.ctx.finish_limit(state, res)
        } else {
            self.lower_user_fun_impl(fun, args, false)
        }
    }

    fn lower_user_fun_impl(
        &mut self,
        fun: hir::Function,
        args: &[ExprId],
        inside_lim: bool,
    ) -> Value {
        // FIXME proper path for functions
        let mut path = self.path.to_owned();
        path.push_str(&fun.name(self.ctx.db));

        let mut args = zip(fun.args(self.ctx.db), args);
        // skip the first two arguments
        if inside_lim {
            args.next();
            args.next();
        }
        for (arg, expr) in args.clone() {
            let init = if arg.is_input(self.ctx.db) {
                self.lower_expr(*expr)
            } else {
                match &arg.ty(self.ctx.db) {
                    Type::Real => F_ZERO,
                    Type::Integer => ZERO,
                    ty => unreachable!("invalid function arg type {:?}", ty),
                }
            };

            self.ctx.def_place(PlaceKind::FunctionArg(arg), init);
        }

        let init = match &fun.return_ty(self.ctx.db) {
            Type::Real => F_ZERO,
            Type::Integer => ZERO,
            ty => unreachable!("invalid function return type {:?}", ty),
        };
        self.ctx.def_place(PlaceKind::FunctionReturn(fun), init);

        let body = fun.body(self.ctx.db);
        BodyLoweringCtx { body: body.borrow(), path: self.path, ctx: self.ctx }.lower_entry_stmts();

        // write outputs back to original (including possibly required cast)
        for (arg, &expr) in args {
            if arg.is_output(self.ctx.db) {
                let mut val = self.ctx.use_place(PlaceKind::FunctionArg(arg));
                // casting in reverse here since we write back
                if let Some((dst, src)) = self.body.needs_cast(expr) {
                    val = self.ctx.insert_cast(val, src, &dst)
                }
                let dst = self.body.get_expr(expr).as_assignment_lhs();
                self.ctx.def_place(dst.into(), val);
            }
        }

        self.ctx.use_place(PlaceKind::FunctionReturn(fun))
    }

    /// The number of elements of an array-typed variable (0 if not an array).
    pub(crate) fn array_len(&self, var: hir::Variable) -> u32 {
        match var.ty(self.ctx.db) {
            Type::Array { len, .. } => len,
            _ => 0,
        }
    }

    /// Lower an array element read `base[index]`. A fixed-size array is one MIR place
    /// per element; a constant index reads it directly, a runtime index builds a
    /// select chain over all elements.
    fn lower_index(&mut self, base: ExprId, index: ExprId) -> Value {
        let var = match self.body.get_expr(base) {
            Expr::Read(Ref::Variable(var)) => var,
            // only array-variable indexing is supported
            _ => return F_ZERO,
        };
        let len = self.array_len(var);
        if len == 0 {
            return F_ZERO;
        }
        // Element positions are offset by the declared lower bound: `real g[2:5]`
        // stores g[2] in element 0. Previously the raw index was used as the
        // position (g[3] read the wrong element) and constant out-of-range
        // indices were silently clamped instead of diagnosed.
        let lo = var.array_lo(self.ctx.db);
        if let Some(c) = self.body.as_literalint(&index) {
            let pos = c as i64 - lo as i64;
            if !(0..len as i64).contains(&pos) {
                // Out of the declared range: diagnosed during type checking;
                // lower to 0 so compilation can continue.
                return F_ZERO;
            }
            return self.ctx.use_place(PlaceKind::VarElement(var, pos as u32));
        }
        let idx_val = self.lower_expr(index);
        let mut res = self.ctx.use_place(PlaceKind::VarElement(var, 0));
        for i in 1..len {
            let elem = self.ctx.use_place(PlaceKind::VarElement(var, i));
            let i_const = self.ctx.iconst(lo + i as i32);
            let cond = self.ctx.ins().ieq(idx_val, i_const);
            let prev = res;
            res = self.ctx.make_select(cond, |_s, branch| if branch { elem } else { prev });
        }
        res
    }

    /// Evaluate a compile-time-constant real expression (literal, possibly
    /// with a leading unary `+`/`-`). Used to read the `noise_table` inline
    /// data array, whose elements must all be constants per the LRM.
    fn eval_const_real(&self, expr: ExprId) -> Option<f64> {
        if let Some(lit) = self.body.as_literal(expr) {
            return match lit {
                Literal::Float(f) => Some((*f).into()),
                Literal::Int(i) => Some(*i as f64),
                _ => None,
            };
        }
        match self.body.get_expr(expr) {
            Expr::UnaryOp { expr: inner, op } => match op {
                UnaryOp::Neg => Some(-self.eval_const_real(inner)?),
                UnaryOp::Identity => self.eval_const_real(inner),
                _ => None,
            },
            _ => None,
        }
    }

    /// Read a whitespace-separated two-column `<freq> <power>` noise table
    /// file, resolved relative to the directory of the compilation root file.
    /// Blank lines and `#`/`//`/`*`-prefixed comment lines are skipped.
    fn read_noise_table_file(&self, fname: &str) -> Vec<(f64, f64)> {
        let Some(dir) = self.ctx.db.root_file_dir() else { return Vec::new() };
        let Some(path) = dir.join(fname) else { return Vec::new() };
        let Some(abs) = path.as_path() else { return Vec::new() };
        let Ok(content) = std::fs::read_to_string(abs) else { return Vec::new() };
        let mut out = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty()
                || line.starts_with('#')
                || line.starts_with("//")
                || line.starts_with('*')
            {
                continue;
            }
            let mut it = line.split_whitespace();
            if let (Some(a), Some(b)) = (it.next(), it.next()) {
                if let (Ok(f), Ok(p)) = (a.parse::<f64>(), b.parse::<f64>()) {
                    out.push((f, p));
                }
            }
        }
        out
    }

    /// Gather the `(frequency, power)` pairs backing a `noise_table` /
    /// `noise_table_log` call, either from an inline real array
    /// `{f0, p0, f1, p1, ...}` or from a two-column data file.
    fn noise_table_data(&self, signature: hir::Signature, args: &[ExprId]) -> Vec<(f64, f64)> {
        match signature {
            NOISE_TABLE_INLINE | NOISE_TABLE_INLINE_NAME => {
                let elems = match self.body.get_expr(args[0]) {
                    Expr::Array(vals) => vals,
                    _ => return Vec::new(),
                };
                let nums: Vec<f64> =
                    elems.iter().map(|&e| self.eval_const_real(e).unwrap_or(0.0)).collect();
                nums.chunks_exact(2).map(|c| (c[0], c[1])).collect()
            }
            NOISE_TABLE_FILE | NOISE_TABLE_FILE_NAME => {
                let fname = self.body.as_literal(args[0]).unwrap().unwrap_str();
                self.read_noise_table_file(fname)
            }
            _ => Vec::new(),
        }
    }

    fn lower_builtin(&mut self, expr: ExprId, builtin: BuiltIn, args: &[ExprId]) -> Value {
        let signature = self.body.get_call_signature(expr);
        match builtin {
            BuiltIn::abs => {
                let (negate, comparison, zero) = match_signature!(signature:
                    ABS_REAL => (Opcode::Fneg, Opcode::Flt,  F_ZERO),
                    ABS_INT => (Opcode::Ineg, Opcode::Ilt, ZERO)
                );
                let val = self.lower_expr(args[0]);
                let (inst, dfg) = self.ctx.ins().binary(comparison, val, zero);
                let cond = dfg.first_result(inst);

                self.lower_select_with(
                    cond,
                    |sel| {
                        let (inst, dfg) = sel.ctx.ins().unary(negate, val);
                        dfg.first_result(inst)
                    },
                    |_| val,
                )
            }
            BuiltIn::acos => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().acos(arg0)
            }
            BuiltIn::acosh => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().acosh(arg0)
            }
            BuiltIn::asin => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().asin(arg0)
            }
            BuiltIn::asinh => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().asinh(arg0)
            }
            BuiltIn::atan => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().atan(arg0)
            }
            BuiltIn::atan2 => {
                let arg0 = self.lower_expr(args[0]);
                let arg1 = self.lower_expr(args[1]);
                self.ctx.ins().atan2(arg0, arg1)
            }
            BuiltIn::atanh => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().atanh(arg0)
            }
            BuiltIn::cos => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().cos(arg0)
            }
            BuiltIn::cosh => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().cosh(arg0)
            }
            // TODO implement limexp properly
            BuiltIn::exp => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().exp(arg0)
            }
            BuiltIn::expm1 => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().expm1(arg0)
            }

            BuiltIn::limexp => {
                let arg0 = self.lower_expr(args[0]);
                // let (state, store) = self.stateful_callback(CallBackKind::StoreState);
                let cut_off = self.ctx.fconst(1e30f64.ln());
                let off = self.ctx.fconst(1e30f64);

                // let change = self.ctx.ins().fsub(arg0, state);
                let linearize = self.ctx.ins().fgt(arg0, cut_off);
                self.ctx.make_select(linearize, |func, linearize| {
                    if linearize {
                        let delta = func.ins().fsub(arg0, cut_off);
                        let lin = func.ins().fmul(off, delta);
                        func.ins().fadd(off, lin)
                    } else {
                        func.ins().exp(arg0)
                    }
                })
            }
            BuiltIn::floor => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().floor(arg0)
            }
            BuiltIn::hypot => {
                let arg0 = self.lower_expr(args[0]);
                let arg1 = self.lower_expr(args[1]);
                self.ctx.ins().hypot(arg0, arg1)
            }
            BuiltIn::ln => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().ln(arg0)
            }
            BuiltIn::ln1p => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().ln1p(arg0)
            }
            BuiltIn::sin => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().sin(arg0)
            }
            BuiltIn::sinh => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().sinh(arg0)
            }
            BuiltIn::sqrt => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().sqrt(arg0)
            }
            BuiltIn::tan => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().tan(arg0)
            }
            BuiltIn::tanh => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().tanh(arg0)
            }
            BuiltIn::clog2 => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().clog2(arg0)
            }
            BuiltIn::log10 | BuiltIn::log => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().log(arg0)
            }
            BuiltIn::ceil => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().ceil(arg0)
            }

            BuiltIn::max => {
                let comparison = match_signature!(signature: MAX_REAL => InstBuilder::fgt, MAX_INT => InstBuilder::igt);
                let arg0 = self.lower_expr(args[0]);
                let arg1 = self.lower_expr(args[1]);
                let cond = comparison(self.ctx.ins(), arg0, arg1);
                self.lower_select_with(cond, |_| arg0, |_| arg1)
            }
            BuiltIn::min => {
                let comparison = match_signature!(signature: MAX_REAL => InstBuilder::flt, MAX_INT => InstBuilder::ilt);
                let arg0 = self.lower_expr(args[0]);
                let arg1 = self.lower_expr(args[1]);
                let cond = comparison(self.ctx.ins(), arg0, arg1);
                self.lower_select_with(cond, |_| arg0, |_| arg1)
            }
            BuiltIn::pow => {
                let arg0 = self.lower_expr(args[0]);
                let arg1 = self.lower_expr(args[1]);
                self.ctx.ins().pow(arg0, arg1)
            }

            BuiltIn::write => {
                self.ins_display(DisplayKind::Display, false, args);
                GRAVESTONE
            }
            BuiltIn::display | BuiltIn::strobe | BuiltIn::monitor => {
                self.ins_display(DisplayKind::Display, true, args);
                GRAVESTONE
            }
            BuiltIn::debug => {
                self.ins_display(DisplayKind::Debug, true, args);
                GRAVESTONE
            }

            BuiltIn::warning => {
                self.ins_display(DisplayKind::Warn, true, args);
                GRAVESTONE
            }
            BuiltIn::error => {
                self.ins_display(DisplayKind::Error, true, args);
                GRAVESTONE
            }
            BuiltIn::info => {
                self.ins_display(DisplayKind::Info, true, args);
                GRAVESTONE
            }

            BuiltIn::fatal => {
                self.ins_display(DisplayKind::Fatal, true, args);
                // Fatal code is 0 (used for translation MIR->IR)
                let call_args = vec![];
                self.ctx.call(CallBackKind::SetRetFlag(RetFlag::Abort), &call_args);
                self.ctx.ins().exit();

                // Create unreachable block for the remainder of iftrue (after $fatal).
                // Seal it (it is the replacement of the original iftrue block).
                // Because it has no incoming edges it will be removed from MIR.
                let unreachable_bb = self.ctx.create_block();
                self.ctx.switch_to_block(unreachable_bb);
                self.ctx.seal_block(unreachable_bb);

                GRAVESTONE
            }
            BuiltIn::analysis => {
                let arg = self.lower_expr(args[0]);
                self.ctx.call1(CallBackKind::Analysis, &[arg])
            }

            BuiltIn::noise_table
            | BuiltIn::noise_table_log
            | BuiltIn::white_noise
            | BuiltIn::ac_stim
            | BuiltIn::flicker_noise
                if self.ctx.no_equations =>
            {
                F_ZERO
            }

            BuiltIn::white_noise => {
                // we create a dedicated callback for each noise source
                // by giving every source a unique index. Kind of ineffcient
                // but necessary to avoid accidental correlation/opimization
                // (for example white_noise(x) - white_noise(x) is not zero)
                let idx = self.ctx.num_noise_sources;
                self.ctx.num_noise_sources += 1;
                let name = if signature == WHITE_NOISE_NAME {
                    let name = self.body.as_literal(args[1]).unwrap().unwrap_str();
                    self.ctx.func.interner.get_or_intern(name)
                } else {
                    let name = format!("unnamed{idx}");
                    self.ctx.func.interner.get_or_intern(name)
                };
                let pwr = self.lower_expr(args[0]);
                self.ctx.call1(CallBackKind::WhiteNoise { name, idx }, &[pwr])
            }
            BuiltIn::flicker_noise => {
                // see above
                let idx = self.ctx.num_noise_sources;
                self.ctx.num_noise_sources += 1;
                let name = if signature == FLICKER_NOISE_NAME {
                    let name = self.body.as_literal(args[2]).unwrap().unwrap_str();
                    self.ctx.func.interner.get_or_intern(name)
                } else {
                    let name = format!("unnamed{idx}");
                    self.ctx.func.interner.get_or_intern(name)
                };
                let pwr = self.lower_expr(args[0]);
                let exp = self.lower_expr(args[1]);
                self.ctx.call1(CallBackKind::FlickerNoise { name, idx }, &[pwr, exp])
            }
            BuiltIn::noise_table | BuiltIn::noise_table_log => {
                // see above
                let idx = self.ctx.num_noise_sources;
                self.ctx.num_noise_sources += 1;
                let name = if matches!(signature, NOISE_TABLE_INLINE_NAME | NOISE_TABLE_FILE_NAME) {
                    let name = self.body.as_literal(args[1]).unwrap().unwrap_str();
                    self.ctx.func.interner.get_or_intern(name)
                } else {
                    let name = format!("unnamed{idx}");
                    self.ctx.func.interner.get_or_intern(name)
                };
                let log = builtin == BuiltIn::noise_table_log;
                let table_vals = self.noise_table_data(signature, args);
                let noise_table = NoiseTable::new(table_vals, log, name, idx);
                self.ctx.call1(CallBackKind::NoiseTable(Box::new(noise_table)), &[])
            }

            BuiltIn::abstime => self.ctx.use_param(ParamKind::Abstime),

            BuiltIn::ddt => {
                if self.ctx.no_equations {
                    return F_ZERO;
                }
                let arg = self.lower_expr(args[0]);
                self.ctx.call1(CallBackKind::TimeDerivative, &[arg])
            }

            BuiltIn::idt | BuiltIn::idtmod if self.ctx.no_equations => {
                match signature {
                    IDT_NO_IC => F_ZERO, // fair enough approximation
                    _ => self.lower_expr(args[1]),
                }
            }

            // Without equation lowering (e.g. op-var contexts) a filter is a no-op.
            BuiltIn::laplace_nd if self.ctx.no_equations => F_ZERO,
            BuiltIn::laplace_nd => self.lower_laplace_nd(args),

            BuiltIn::idt => {
                let kind = match_signature! {
                    signature:
                        IDT_NO_IC => IdtKind::Basic,
                        IDT_IC => IdtKind::Ic,
                        // we currently do not support tolerance
                        IDT_IC_ASSERT | IDT_IC_ASSERT_TOL | IDT_IC_ASSERT_NATURE => IdtKind::Assert
                };

                self.lower_integral(kind, args)
            }

            BuiltIn::idtmod => {
                let kind = match_signature! {
                    signature:
                        IDTMOD_NO_IC => IdtKind::Basic,
                        IDTMOD_IC => IdtKind::Ic,
                        IDTMOD_IC_MODULUS => IdtKind::Modulus,
                        // we currently do not support tolerance
                        IDTMOD_IC_MODULUS_OFFSET
                        | IDTMOD_IC_MODULUS_OFFSET_TOL
                        | IDTMOD_IC_MODULUS_OFFSET_NATURE => IdtKind::ModulusOffset
                };

                self.lower_integral(kind, args)
            }

            BuiltIn::flow => {
                let res = match_signature! {
                    signature:
                        NATURE_ACCESS_NODES|NATURE_ACCESS_NODE_GND => self.nodes_from_args(
                            args,
                            |hi, lo| ParamKind::Current(CurrentKind::Unnamed{hi,lo})
                        ),
                        NATURE_ACCESS_BRANCH => self.ctx.use_param(ParamKind::Current(
                            CurrentKind::Branch(self.body.into_branch(args[0]))
                        )),
                        NATURE_ACCESS_PORT_FLOW => self.ctx.use_param(ParamKind::Current(
                            CurrentKind::Port(self.body.into_port_flow(args[0]))
                        ))
                };
                // AB: Do not divide flow probe.
                //     Flow unknowns correspond to the flow of a single parallel instance.
                //     HIR equation describes a single parallel instance.
                //     Handle $mfactor at a lower level.
                // let mfactor = self.ctx.use_param(ParamKind::ParamSysFun(ParamSysFun::mfactor));
                // return self.ctx.ins().fdiv(res, mfactor);
                return res;
            }
            BuiltIn::potential => {
                match_signature! {
                    signature:
                        NATURE_ACCESS_NODES|NATURE_ACCESS_NODE_GND => self.nodes_from_args( args, |hi,lo|ParamKind::Voltage{hi,lo}),
                        NATURE_ACCESS_BRANCH => {
                            let branch = self.body.into_branch(args[0]).kind(self.ctx.db);
                            self.ctx.nodes(branch.unwrap_hi_node(), branch.lo_node(), |hi, lo| ParamKind::Voltage{ hi, lo })
                        }
                }
            }
            BuiltIn::vt => {
                // TODO make this a database input
                const KB: f64 = 1.3806488e-23;
                const Q: f64 = 1.602176565e-19;

                let fac = self.ctx.fconst(KB / Q);
                let temp = match args.get(0) {
                    Some(temp) => self.lower_expr(*temp),
                    None => self.ctx.use_param(ParamKind::Temperature),
                };

                self.ctx.ins().fmul(fac, temp)
            }

            BuiltIn::ddx => {
                let val = self.lower_expr(args[0]);
                let unknown = self.lower_expr(args[1]);
                let call = if signature == DDX_POT {
                    // TODO how to handle gnd nodes?
                    let node = self.ctx.unwrap_node(unknown);
                    CallBackKind::NodeDerivative(node)
                } else {
                    CallBackKind::Derivative(self.ctx.dfg().value_def(unknown).unwrap_param())
                };
                self.ctx.call1(call, &[val])
            }
            BuiltIn::temperature => self.ctx.use_param(ParamKind::Temperature),
            BuiltIn::simparam => {
                let arg0 = self.lower_expr(args[0]);
                match_signature! {signature:
                    SIMPARAM_NO_DEFAULT => self.ctx.call1(CallBackKind::SimParam, &[arg0]),
                    SIMPARAM_DEFAULT => {
                        let arg1 = self.lower_expr(args[1]);
                        self.ctx.call1(CallBackKind::SimParamOpt, &[arg0, arg1])
                    }
                }
            }
            BuiltIn::simparam_str => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.call1(CallBackKind::SimParamStr, &[arg0])
            }
            BuiltIn::param_given => self
                .ctx
                .use_param(ParamKind::ParamGiven { param: self.body.into_parameter(args[0]) }),
            BuiltIn::port_connected => {
                self.ctx.use_param(ParamKind::PortConnected { port: self.body.into_node(args[0]) })
            }
            BuiltIn::bound_step => {
                let step_size = self.lower_expr(args[0]);
                self.ctx.def_place(PlaceKind::BoundStep, step_size);
                GRAVESTONE
            }

            BuiltIn::limit if signature == LIMIT_BUILTIN_FUNCTION && !self.ctx.no_equations => {
                let new_val = self.lower_expr(args[0]);
                let state = self.ctx.start_limit(new_val);
                let prev_val = self.ctx.use_param(ParamKind::PrevState(state));
                let name = self.body.as_literal(args[1]).unwrap().unwrap_str();
                let name = self.ctx.func.interner.get_or_intern(name);
                let mut call_args = vec![new_val, prev_val];
                call_args.extend(args[2..].iter().map(|arg| self.lower_expr(*arg)));

                let enable_lim = self.ctx.use_param(ParamKind::EnableLim);
                let res = self.ctx.make_select(enable_lim, |func, lim| {
                    if lim {
                        func.call1(
                            CallBackKind::BuiltinLimit { name, num_args: args.len() as u32 },
                            &call_args,
                        )
                    } else {
                        new_val
                    }
                });

                self.ctx.finish_limit(state, res)
            }
            BuiltIn::discontinuity => {
                // AB: Negative literals are represented as UnaryOp::Neg(Literal)
                //     We have a function for that now.
                if self.ctx.inside_lim && Some(-1) == self.body.as_literalsignedint(&args[0]) {
                    self.ctx.call(CallBackKind::LimDiscontinuity, &[]);
                } else {
                    // TODO implement support for discontinuity?
                }
                GRAVESTONE
            }
            BuiltIn::finish => {
                // Finish code is 1 (used for translation MIR->IR)
                let call_args = vec![];
                self.ctx.call(CallBackKind::SetRetFlag(RetFlag::Finish), &call_args);
                GRAVESTONE
            }

            BuiltIn::stop => {
                // Stop code is 2 (used for translation MIR->IR)
                let call_args = vec![];
                self.ctx.call(CallBackKind::SetRetFlag(RetFlag::Stop), &call_args);
                GRAVESTONE
            }

            BuiltIn::absdelay => {
                let source = self.lower_expr(args[0]);
                if self.ctx.no_equations {
                    return source;
                }
                let delay = self.lower_expr(args[1]);
                let max_delay = (signature == ABSDELAY_MAX).then(|| self.lower_expr(args[2]));

                let source_pair = match self.ctx.dfg().value_def(source) {
                    mir::ValueDef::Param(param) => {
                        match self.ctx.intern.params.get_index(param).unwrap().0 {
                            ParamKind::Voltage { hi, lo } => Some((*hi, *lo)),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let input = if let Some((hi, lo)) = source_pair {
                    crate::AbsDelayInput::Voltage { hi, lo }
                } else {
                    let (eq, x) = self.ctx.implicit_equation(ImplicitEquationKind::AbsDelayInput);
                    let residual = self.ctx.ins().fsub(source, x);
                    self.ctx.def_resist_residual(residual, eq);
                    crate::AbsDelayInput::Internal(eq)
                };

                let (output, y) = self.ctx.implicit_equation(ImplicitEquationKind::AbsDelayOutput);
                self.ctx.intern.absdelay.push(crate::AbsDelayInfo {
                    input,
                    output,
                    delay,
                    max_delay,
                });
                y
            }
            BuiltIn::transition => {
                // `transition` accepts an integer or real first argument; the
                // builtin signature types it as Real, so `lower_expr` already widens
                // an integer/bool argument to Real for us (the operator returns Real).
                let target = self.lower_expr(args[0]);
                if self.ctx.no_equations {
                    // No DAE context (AC/noise setup, op-vars): pass the target through.
                    target
                } else {
                    // Continuous (slew-limited) realization. The ideal `transition` is a
                    // piecewise-linear ramp from the old value to the new one over the
                    // rise/fall time; emitting it as an instantaneous jump produces a
                    // time discontinuity the transient integrator cannot step across
                    // ("timestep too small"). We realize it as a first-order lag whose
                    // time constant is the rise time when the target is increasing and
                    // the fall time when decreasing — a continuous output the solver
                    // integrates through, with the requested transition speed.
                    let eps = self.ctx.fconst(1e-12);
                    let rise = if args.len() > 2 { self.lower_expr(args[2]) } else { eps };
                    let fall = if args.len() > 3 { self.lower_expr(args[3]) } else { rise };
                    let (eq, x) =
                        self.ctx.implicit_equation(ImplicitEquationKind::Idt(IdtKind::Basic));
                    // tau = (target >= x) ? rise : fall, floored to eps to avoid /0.
                    let rising = self.ctx.ins().fge(target, x);
                    let tau = self.ctx.make_select(rising, |_s, b| if b { rise } else { fall });
                    let tau_ok = self.ctx.ins().fge(tau, eps);
                    let tau = self.ctx.make_select(tau_ok, |_s, b| if b { tau } else { eps });
                    // dx/dt = (target - x)/tau  ->  react = x, resist = (x - target)/tau.
                    let diff = self.ctx.ins().fsub(x, target);
                    let resist = self.ctx.ins().fdiv(diff, tau);
                    self.ctx.def_resist_residual(resist, eq);
                    self.ctx.def_react_residual(x, eq);
                    x
                }
            }
            BuiltIn::slew | BuiltIn::limit => self.lower_expr(args[0]),

            // `ac_stim` is an AC small-signal stimulus: it is defined to be zero in the
            // large-signal (DC/transient) domain, which is what a contribution lowers.
            // Previously only the `no_equations` guard above matched, so a contributing
            // use (`V(a,b) <+ ac_stim(...)`) fell through to `unreachable!()` and
            // crashed the compiler. Actual AC-analysis injection is not implemented yet.
            BuiltIn::ac_stim => F_ZERO,

            _ => unreachable!(),
        }
    }

    fn lower_integral(&mut self, kind: IdtKind, args: &[ExprId]) -> Value {
        let (equation, val) = self.ctx.implicit_equation(ImplicitEquationKind::Idt(kind));

        let mut enable_integral = self.ctx.use_param(ParamKind::EnableIntegration);
        let residual = if kind.has_ic() {
            if kind.has_assert() {
                enable_integral = self.lower_select_with(
                    enable_integral,
                    |mut s| {
                        let assert = s.lower_expr(args[2]);
                        s.ctx.ins().feq(assert, F_ZERO)
                    },
                    |_| FALSE,
                )
            }

            self.lower_multi_select(enable_integral, |mut ctx, branch| {
                if branch {
                    // Always integrate the DAE state unbounded; for `idtmod` the modulo
                    // wrap is applied to the *returned value* (below), not the state.
                    // Wrapping the state inside the residual makes the reactive residual
                    // jump by `modulus` at each wrap, so the transient integrator's d/dt
                    // term (based on the previous charge, ~modulus) diverges at the wrap.
                    let arg = ctx.lower_expr(args[0]);
                    [ctx.ctx.ins().fneg(arg), val]
                } else {
                    // During the IC/DC phase the stored charge (reactive residual) must
                    // be `ic`, not zero: `val - ic` pins `val = ic` at DC, but a zero
                    // charge makes the integrator restart from 0 once transient
                    // integration turns on, silently dropping the initial condition.
                    // Storing charge = `ic` lets the transient continue from `ic` (and
                    // an `assert` reset likewise restores the integrator to `ic`).
                    let ic = ctx.lower_expr(args[1]);
                    [ctx.ctx.ins().fsub(val, ic), ic]
                }
            })
        } else {
            let arg = self.lower_expr(args[0]);
            [self.ctx.ins().fneg(arg), val]
        };

        self.ctx.def_resist_residual(residual[0], equation);
        self.ctx.def_react_residual(residual[1], equation);

        // `idtmod` returns the (unbounded) integral wrapped into `[offset, offset+modulus)`:
        // offset + floor_mod(val - offset, modulus), where floor_mod(x, m) = x - m*floor(x/m)
        // stays in `[0, m)` even for negative x. Only the returned value wraps; the DAE state
        // keeps integrating smoothly (above). This also fixes the offset argument, which
        // previously read `args[2]` (the modulus) instead of `args[3]`.
        if kind.has_modulus() {
            let modulus = self.lower_expr(args[2]);
            let offset = if kind.has_offset() { self.lower_expr(args[3]) } else { F_ZERO };
            let shifted = self.ctx.ins().fsub(val, offset);
            let quot = self.ctx.ins().fdiv(shifted, modulus);
            let whole = self.ctx.ins().floor(quot);
            let whole_mod = self.ctx.ins().fmul(whole, modulus);
            let rem = self.ctx.ins().fsub(shifted, whole_mod);
            self.ctx.ins().fadd(rem, offset)
        } else {
            val
        }
    }

    /// Read the coefficient values of an array-valued argument (an array variable's
    /// elements or an array literal's entries), lowest index first.
    fn array_coeffs(&mut self, arg: ExprId) -> Vec<Value> {
        // Laplace coefficients feed real-valued state-space arithmetic, but an
        // anonymous array literal of integer constants (the LRM's own examples use
        // `'{-1,0,1}`) lowers to integer values. Widen each coefficient to real so
        // the residual math stays well-typed.
        match self.body.get_expr(arg) {
            Expr::Read(Ref::Variable(var)) => {
                let len = self.array_len(var);
                let elem_ty = match var.ty(self.ctx.db) {
                    Type::Array { ty, .. } => *ty,
                    other => other,
                };
                (0..len)
                    .map(|i| {
                        let v = self.ctx.use_place(PlaceKind::VarElement(var, i));
                        self.coeff_to_real(v, &elem_ty)
                    })
                    .collect()
            }
            Expr::Array(elems) => elems
                .iter()
                .map(|&e| {
                    let v = self.lower_expr(e);
                    // `lower_expr` already applies any inference-inserted cast
                    // (`needs_cast`), so consult the *resolved* type here — using the
                    // pre-cast type would insert a second `ifcast` on an already-real
                    // value, which the constant folder rejects.
                    let ty = self.resolved_ty(e);
                    self.coeff_to_real(v, &ty)
                })
                .collect(),
            _ => {
                let v = self.lower_expr(arg);
                let ty = self.resolved_ty(arg);
                vec![self.coeff_to_real(v, &ty)]
            }
        }
    }

    /// Widen an integer/bool coefficient value to real; reals pass through.
    fn coeff_to_real(&mut self, v: Value, ty: &Type) -> Value {
        match ty {
            Type::Integer | Type::Bool => self.ctx.insert_cast(v, ty, &Type::Real),
            _ => v,
        }
    }

    /// Lower `laplace_nd(input, num, den)` (coefficients in ascending powers of `s`)
    /// as a controllable-canonical-form state space using `den.len()-1` integrator
    /// states (implicit equations), reusing the existing DAE machinery. Coefficients
    /// may be runtime values.
    fn lower_laplace_nd(&mut self, args: &[ExprId]) -> Value {
        let input = self.lower_expr(args[0]);
        let num = self.array_coeffs(args[1]);
        let den = self.array_coeffs(args[2]);
        let n = den.len().saturating_sub(1); // filter order
        if n == 0 {
            if num.is_empty() || den.is_empty() {
                return input;
            }
            let g = self.ctx.ins().fdiv(num[0], den[0]);
            return self.ctx.ins().fmul(g, input);
        }

        // States x_0..x_{n-1} with x_i = s^i w where D(s) w = input.
        let mut states = Vec::with_capacity(n);
        for _ in 0..n {
            states.push(self.ctx.implicit_equation(ImplicitEquationKind::Idt(IdtKind::Basic)));
        }

        // dx_i/dt = x_{i+1} for i in 0..n-1.
        for i in 0..n - 1 {
            let eq = states[i].0;
            let next = states[i + 1].1;
            let neg = self.ctx.ins().fneg(next);
            self.ctx.def_resist_residual(neg, eq);
            self.ctx.def_react_residual(states[i].1, eq);
        }

        // dx_{n-1}/dt = (input - Σ_{i<n} den[i] x_i) / den[n].
        let mut acc = input;
        for i in 0..n {
            let term = self.ctx.ins().fmul(den[i], states[i].1);
            acc = self.ctx.ins().fsub(acc, term);
        }
        let rhs = self.ctx.ins().fdiv(acc, den[n]);
        let (eq_last, x_last) = states[n - 1];
        let neg = self.ctx.ins().fneg(rhs);
        self.ctx.def_resist_residual(neg, eq_last);
        self.ctx.def_react_residual(x_last, eq_last);

        // Direct feedthrough d = num[n]/den[n], present only when deg(num) == deg(den).
        // Since s^n w = (input - Σ_{i<n} den[i] x_i)/den[n], the exact output is
        //   y = Σ_{k<n} (num[k] - d·den[k]) x_k + d·input.
        // Previously num[n] was silently dropped, so any exactly-proper transfer
        // function (e.g. a high-pass or all-pass section) lost its feedthrough term.
        let d = if num.len() == n + 1 { Some(self.ctx.ins().fdiv(num[n], den[n])) } else { None };

        let mut out = F_ZERO;
        for (k, &nk) in num.iter().enumerate() {
            if k < n {
                let ck = match d {
                    Some(d) => {
                        let d_ak = self.ctx.ins().fmul(d, den[k]);
                        self.ctx.ins().fsub(nk, d_ak)
                    }
                    None => nk,
                };
                let term = self.ctx.ins().fmul(ck, states[k].1);
                out = self.ctx.ins().fadd(out, term);
            }
        }
        if let Some(d) = d {
            let du = self.ctx.ins().fmul(d, input);
            out = self.ctx.ins().fadd(out, du);
        }
        out
    }

    pub fn resolved_ty(&self, expr: ExprId) -> Type {
        self.body
            .needs_cast(expr)
            .map(|(_, dst)| dst.to_owned())
            .unwrap_or_else(|| self.body.expr_type(expr))
    }

    pub fn lower_body(&mut self, body: Body, i: usize) -> Value {
        let expr = body.borrow().get_entry_expr(i);
        BodyLoweringCtx { ctx: self.ctx, body: body.borrow(), path: self.path }.lower_expr(expr)
    }
}
