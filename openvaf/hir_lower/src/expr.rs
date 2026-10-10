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
use hir::table_model;
use hir::zi_filter;
use hir::{
    Body, BodyRef, BuiltIn, Expr, ExprId, Literal, /*ParamSysFun,*/ Ref, ResolvedFun, Stmt,
    Type,
};
use mir::builder::InstBuilder;
use mir::{Opcode, Value, FALSE, F_ZERO, GRAVESTONE, INFINITY, TRUE, ZERO};
use stdx::iter::zip;
use syntax::ast::{BinaryOp, UnaryOp};

use crate::body::BodyLoweringCtx;

use crate::fmt::DisplayKind;
use crate::{
    AbsDelayMode, CallBackKind, CurrentKind, IdtKind, ImplicitEquationKind, NoiseTable, ParamKind,
    PlaceKind, RetFlag, RngDist,
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
                match arg.ty(self.ctx.db) {
                    Type::Real => F_ZERO,
                    Type::Integer => ZERO,
                    // VAMS-2023 4.7.1: string-typed function arguments start empty
                    Type::String => self.ctx.sconst(""),
                    ty => unreachable!("invalid function arg type {:?}", ty),
                }
            };

            self.ctx.def_place(PlaceKind::FunctionArg(arg), init);
        }

        let init = match fun.return_ty(self.ctx.db) {
            Type::Real => F_ZERO,
            Type::Integer => ZERO,
            // VAMS-2023 4.7.1: `analog function string` - the return value is
            // initialised to the empty string
            Type::String => self.ctx.sconst(""),
            ty => unreachable!("invalid function return type {:?}", ty),
        };
        self.ctx.def_place(PlaceKind::FunctionReturn(fun), init);

        let body = fun.body(self.ctx.db);
        let body_ref = body.borrow();
        let needs_exit = body_has_return(&body_ref);
        let (prev_exit, prev_fun, exit) = if needs_exit {
            let exit = self.ctx.create_block();
            let prev_exit = self.ctx.function_exit.replace(exit);
            let prev_fun = self.ctx.function_return.replace(fun);
            (prev_exit, prev_fun, Some(exit))
        } else {
            (None, None, None)
        };

        BodyLoweringCtx { body: body_ref, path: self.path, ctx: self.ctx }.lower_entry_stmts();

        if let Some(exit) = exit {
            self.ctx.ensured_sealed();
            if !self.ctx.func.is_filled() {
                self.ctx.ins().jump(exit);
            }
            self.ctx.seal_block(exit);
            self.ctx.switch_to_block(exit);
            self.ctx.function_exit = prev_exit;
            self.ctx.function_return = prev_fun;
        }

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

    /// The constant contents of an array argument to `$table_model`.
    ///
    /// 9.21.1: "The state of the data source is captured on the first call to the
    /// table model function. Any change after this point is ignored." The table is
    /// compiled into the model, so the contents have to be known here. An array
    /// literal is; an array variable the module fills in at run time is not, and
    /// `hir_ty` turns that into a diagnostic rather than letting it arrive here.
    fn const_real_array(&self, arg: ExprId) -> Option<Vec<f64>> {
        let elems = match self.body.get_expr(arg) {
            Expr::Array(vals) => vals,
            _ => return None,
        };
        elems.iter().map(|&e| self.eval_const_real(e)).collect()
    }

    /// A table data file, resolved beside the root source file the way
    /// `noise_table`'s data file is.
    fn read_table_file(&self, fname: &str) -> Option<String> {
        let dir = self.ctx.db.root_file_dir()?;
        let path = dir.join(fname)?;
        let abs = path.as_path()?;
        std::fs::read_to_string(abs).ok()
    }

    /// VAMS-2023 9.21:
    ///
    ///   $table_model ( table_inputs , table_data_source [, table_control_string] )
    ///
    /// The table is built here, at compile time, and emitted as the piecewise
    /// polynomial `crate::table_model` reduces it to. That is what lets the
    /// Jacobian fall out: the lookup is ordinary arithmetic over the lookup
    /// expressions, so the existing autodiff differentiates it. A callback could
    /// not be differentiated at all, since `mir_autodiff` treats `Opcode::Call` as
    /// having no derivative.
    ///
    /// Anything this cannot build returns zero, which `hir_ty` makes unreachable by
    /// rejecting the call first; the arms are here so that a malformed table cannot
    /// panic the compiler.
    fn lower_table_model(&mut self, args: &[ExprId]) -> Value {
        // The lookup inputs come first and the data source follows them, so the
        // inputs are the leading arguments that are neither a string nor an array.
        // Reading the shape back off the types keeps this in step with the
        // signature table without a sixteen-way match on it.
        let ndims = args
            .iter()
            .position(|&a| {
                matches!(
                    self.body.expr_type(a),
                    Type::String | Type::Array { .. } | Type::EmptyArray
                )
            })
            .unwrap_or(args.len());
        if ndims == 0 || ndims >= args.len() {
            return F_ZERO;
        }

        let (rows, after) = if matches!(self.body.expr_type(args[ndims]), Type::String) {
            let fname = self.body.as_literal(args[ndims]).unwrap().unwrap_str().to_owned();
            let text = match self.read_table_file(&fname) {
                Some(text) => text,
                None => return F_ZERO,
            };
            match table_model::Rows::parse(&text) {
                Ok(rows) => (rows, ndims + 1),
                Err(_) => return F_ZERO,
            }
        } else {
            // `table_model_array` is N independent columns followed by the output.
            let end = 2 * ndims + 1;
            if args.len() < end {
                return F_ZERO;
            }
            let mut cols = Vec::with_capacity(ndims + 1);
            for &arg in &args[ndims..end] {
                match self.const_real_array(arg) {
                    Some(col) => cols.push(col),
                    None => return F_ZERO,
                }
            }
            match table_model::Rows::from_columns(&cols) {
                Ok(rows) => (rows, end),
                Err(_) => return F_ZERO,
            }
        };

        let spec = match args.get(after) {
            Some(&arg) => self.body.as_literal(arg).unwrap().unwrap_str().to_owned(),
            None => String::new(),
        };
        let control = match table_model::Control::parse(&spec, ndims) {
            Ok(control) => control,
            Err(_) => return F_ZERO,
        };
        let table = match table_model::Table::build(&rows, &control) {
            Ok((table, _warnings)) => table,
            Err(_) => return F_ZERO,
        };

        let mut inputs = Vec::with_capacity(ndims);
        for &arg in &args[..ndims] {
            inputs.push(self.lower_expr(arg));
        }

        let (value, outside) = self.emit_table(&table.root, &table.dims, &inputs);

        // 9.21.2's `E` method: "an extrapolation error is reported if the
        // $table_model function is requested to evaluate a point beyond the
        // interpolation region", and 9.21 adds that it "results in a fatal error
        // being raised".
        if outside != FALSE {
            self.ctx.make_select(outside, |ctx, taken| {
                if taken {
                    ctx.call(CallBackKind::SetRetFlag(RetFlag::Abort), &[]);
                }
                F_ZERO
            });
        }

        value
    }

    /// Emit a lookup over the isoline tree as `(value, outside)`.
    ///
    /// Each child is emitted once and the pieces select between the results, so
    /// the code grows with the number of samples rather than with the product of
    /// the dimensions. The `outside` flag travels beside the value so that an `E`
    /// end deeper in the table fires only on the path the lookup actually took,
    /// and it collapses to a constant `false` for the overwhelming majority of
    /// tables, which have no `E` end at all.
    fn emit_table(
        &mut self,
        node: &table_model::Node,
        dims: &[table_model::DimControl],
        inputs: &[Value],
    ) -> (Value, Value) {
        let kids = match node {
            table_model::Node::Leaf(val) => return (self.ctx.fconst(*val), FALSE),
            table_model::Node::Branch(kids) => kids,
        };

        let mut vals = Vec::with_capacity(kids.len());
        let mut outs = Vec::with_capacity(kids.len());
        for (_, kid) in kids {
            let (val, out) = self.emit_table(kid, &dims[1..], &inputs[1..]);
            vals.push(val);
            outs.push(out);
        }

        let xs: Vec<f64> = kids.iter().map(|(ordinate, _)| *ordinate).collect();
        let segs = table_model::segments(&xs, dims[0]);
        let x = inputs[0];
        let track = segs.iter().any(|seg| seg.error) || outs.iter().any(|out| *out != FALSE);

        // Built from the last piece backwards, so each comparison only has to
        // decide between this piece and everything above it.
        let mut chain: Option<(Value, Value)> = None;
        for seg in segs.iter().rev() {
            let val = self.emit_segment(seg, &vals, x);
            let out = if !track {
                FALSE
            } else if seg.error {
                TRUE
            } else {
                let mut acc = FALSE;
                for (j, _) in &seg.terms {
                    acc = if acc == FALSE { outs[*j] } else { self.or(acc, outs[*j]) };
                }
                acc
            };
            chain = Some(match chain {
                None => (val, out),
                Some((above_val, above_out)) => {
                    let (bound, inclusive) =
                        seg.upper.expect("only the last piece runs to infinity");
                    let bound = self.ctx.fconst(bound);
                    let cond = if inclusive {
                        self.ctx.ins().fle(x, bound)
                    } else {
                        self.ctx.ins().flt(x, bound)
                    };
                    let val = self.ctx.make_select(cond, |_s, t| if t { val } else { above_val });
                    let out = if track {
                        self.ctx.make_select(cond, |_s, t| if t { out } else { above_out })
                    } else {
                        FALSE
                    };
                    (val, out)
                }
            });
        }
        chain.expect("a dimension always has at least one piece")
    }

    /// One piece of a dimension: `sum over j of sample_j * P_j(x - origin)`.
    fn emit_segment(&mut self, seg: &table_model::Segment, vals: &[Value], x: Value) -> Value {
        if seg.terms.is_empty() {
            return F_ZERO;
        }
        let origin = self.ctx.fconst(seg.origin);
        let u = self.ctx.ins().fsub(x, origin);
        let mut acc: Option<Value> = None;
        for (j, coeffs) in &seg.terms {
            // A weight of exactly one is what constant extrapolation and a closest
            // point lookup produce, and they are common enough to be worth not
            // multiplying by.
            let term = if *coeffs == [1.0, 0.0, 0.0, 0.0] {
                vals[*j]
            } else {
                let weight = self.emit_poly(u, *coeffs);
                self.ctx.ins().fmul(vals[*j], weight)
            };
            acc = Some(match acc {
                None => term,
                Some(sum) => self.ctx.ins().fadd(sum, term),
            });
        }
        acc.unwrap_or(F_ZERO)
    }

    /// `c0 + u*(c1 + u*(c2 + u*c3))` by Horner, with the zero terms left out.
    fn emit_poly(&mut self, u: Value, coeffs: [f64; 4]) -> Value {
        let top = match coeffs.iter().rposition(|c| *c != 0.0) {
            Some(top) => top,
            None => return F_ZERO,
        };
        let mut acc = self.ctx.fconst(coeffs[top]);
        for k in (0..top).rev() {
            acc = self.ctx.ins().fmul(acc, u);
            if coeffs[k] != 0.0 {
                let c = self.ctx.fconst(coeffs[k]);
                acc = self.ctx.ins().fadd(acc, c);
            }
        }
        acc
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

            // `$rtoi`: IEEE 1364 truncates toward zero. Must not reuse `FIcast`,
            // which rounds (language real→integer conversion / `llvm.lround`).
            BuiltIn::rtoi => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().fitrunc(arg0)
            }
            // `$itor`: integer→real; same semantics as the language `IFcast`.
            BuiltIn::itor => {
                let arg0 = self.lower_expr(args[0]);
                self.ctx.ins().ifcast(arg0)
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
            BuiltIn::laplace_nd
            | BuiltIn::laplace_zp
            | BuiltIn::laplace_zd
            | BuiltIn::laplace_np
                if self.ctx.no_equations =>
            {
                F_ZERO
            }
            BuiltIn::laplace_nd => self.lower_laplace_nd(args),
            // VAMS-2023 4.5.11.1-4.5.11.3: the same filter with its numerator,
            // denominator or both given as roots instead of coefficients.
            BuiltIn::laplace_zp => self.lower_laplace_roots(args, true, true),
            BuiltIn::laplace_zd => self.lower_laplace_roots(args, true, false),
            BuiltIn::laplace_np => self.lower_laplace_roots(args, false, true),

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
            // VAMS-2023 9.13. `$random`/`$arandom` are an inclusive draw over the
            // whole 32-bit range; the rest name their distribution. The integer
            // forms round half away from zero, the `$rdist_*` forms do not.
            BuiltIn::random | BuiltIn::arandom => {
                let lo = self.ctx.fconst(-2147483648.0);
                let hi = self.ctx.fconst(2147483647.0);
                let val = self.lower_rng(RngDist::UniformInt, args, lo, hi);
                self.ctx.insert_cast(val, &Type::Real, &Type::Integer)
            }
            // `$dist_uniform` is an inclusive draw over the integers in [start, end],
            // not the real draw rounded -- a different function, not a different
            // result type.
            BuiltIn::dist_uniform => self.lower_rng_args(RngDist::UniformInt, args, false),
            BuiltIn::rdist_uniform => self.lower_rng_args(RngDist::Uniform, args, false),
            BuiltIn::dist_normal | BuiltIn::rdist_normal => {
                self.lower_rng_args(RngDist::Normal, args, builtin == BuiltIn::dist_normal)
            }
            BuiltIn::dist_erlang | BuiltIn::rdist_erlang => {
                self.lower_rng_args(RngDist::Erlang, args, builtin == BuiltIn::dist_erlang)
            }
            BuiltIn::dist_exponential | BuiltIn::rdist_exponential => self.lower_rng_args(
                RngDist::Exponential,
                args,
                builtin == BuiltIn::dist_exponential,
            ),
            BuiltIn::dist_poisson | BuiltIn::rdist_poisson => {
                self.lower_rng_args(RngDist::Poisson, args, builtin == BuiltIn::dist_poisson)
            }
            BuiltIn::dist_chi_square | BuiltIn::rdist_chi_square => {
                self.lower_rng_args(RngDist::ChiSquare, args, builtin == BuiltIn::dist_chi_square)
            }
            BuiltIn::dist_t | BuiltIn::rdist_t => {
                self.lower_rng_args(RngDist::T, args, builtin == BuiltIn::dist_t)
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
                self.bound_step(step_size);
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
                    // 4.5.7: "In DC and operating point analyses, absdelay() returns
                    // the value of its input", and an op-var or small-signal setup has
                    // no time axis to delay along either.
                    return source;
                }
                let raw_delay = self.lower_expr(args[1]);
                let raw_max = (signature == ABSDELAY_MAX).then(|| self.lower_expr(args[2]));
                let (delay, window) = self.absdelay_window(raw_delay, raw_max);
                // The history has to come from somewhere. Either the simulator keeps
                // it, which is exact but needs the descriptor protocol implemented, or
                // the model keeps it, which runs anywhere.
                if let AbsDelayMode::InModel { depth } = self.ctx.absdelay {
                    return self.emit_absdelay(source, delay, window, depth);
                }
                let max_delay = raw_max.map(|_| window);

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
                // The simulator reads these two out of the instance data, which makes
                // them outputs of the evaluation even though no place points at them.
                // Barriers mark them as such, so the pass that moves op-independent
                // work into instance setup caches them instead of leaving the eval
                // function holding a value that is no longer computed there. Without
                // this a delay that is anything but a parameter or a literal --
                // `absdelay(x, 2 * td)`, or the frozen delay above -- crashes the
                // backend with "attempted to read undefined value".
                let delay = self.ctx.ins().ensure_optbarrier(delay);
                let max_delay = max_delay.map(|val| self.ctx.ins().ensure_optbarrier(val));
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
                    self.lower_transition(target, args)
                }
            }
            BuiltIn::slew => {
                // VAMS-2023 4.5.9: `slew` bounds the rate of change of its argument.
                //
                //   slew ( expr [ , max_pos_slew_rate [ , max_neg_slew_rate ] ] )
                //
                // With no rates given the LRM passes the signal through unchanged,
                // and in DC it passes the value through as well.
                let target = self.lower_expr(args[0]);
                if self.ctx.no_equations || args.len() == 1 {
                    target
                } else {
                    let max_pos = self.lower_expr(args[1]);
                    // "If the max_neg_slew_rate is not specified, it defaults to the
                    // opposite of the max_pos_slew_rate."
                    let max_neg = if args.len() > 2 {
                        self.lower_expr(args[2])
                    } else {
                        self.ctx.ins().fneg(max_pos)
                    };

                    // Realized as a continuous rate limiter, the same shape as the
                    // `transition` lag below: a state whose derivative chases the
                    // target with a very large gain, clamped to the two rates. While
                    // the input changes more slowly than the limits the state follows
                    // it to within `eps * rate`, which is what the LRM asks for
                    // ("returns the value of expr"); once a limit is reached the state
                    // moves at exactly that rate. A clamped derivative is continuous
                    // in time, so the transient integrator can step across it.
                    let eps = self.ctx.fconst(1e-12);
                    let (eq, x) =
                        self.ctx.implicit_equation(ImplicitEquationKind::Idt(IdtKind::Basic));
                    let diff = self.ctx.ins().fsub(target, x);
                    let rate = self.ctx.ins().fdiv(diff, eps);
                    // rate = min(max(rate, max_neg), max_pos)
                    let too_fast = self.ctx.ins().fgt(rate, max_pos);
                    let rate =
                        self.ctx.make_select(too_fast, |_s, b| if b { max_pos } else { rate });
                    let too_slow = self.ctx.ins().flt(rate, max_neg);
                    let rate =
                        self.ctx.make_select(too_slow, |_s, b| if b { max_neg } else { rate });
                    // dx/dt = rate  ->  react = x, resist = -rate.
                    let resist = self.ctx.ins().fneg(rate);
                    self.ctx.def_resist_residual(resist, eq);
                    self.ctx.def_react_residual(x, eq);
                    x
                }
            }
            BuiltIn::last_crossing => {
                // VAMS-2023 4.5.10: the simulation time at which `expr` last crossed
                // zero, in the requested direction.
                //
                //   last_crossing ( expr [ , direction ] )
                //
                // "does not control the timestep to get accurate results; it uses
                // linear interpolation to estimate the time of the last crossing",
                // so this needs no breakpoint machinery -- only the expression and
                // the time at the previous accepted timestep, both of which the
                // retained-state slots already provide.
                let cur = self.lower_expr(args[0]);
                if self.ctx.no_equations {
                    // Nothing steps time here, so nothing can have crossed.
                    return self.ctx.fconst(-1.0);
                }
                let now = self.ctx.use_param(ParamKind::Abstime);

                let s_prev = self.ctx.alloc_retained_state(0.0);
                let s_time = self.ctx.alloc_retained_state(0.0);
                // "Before the expression crosses zero (0) for the first time, the
                // last_crossing() function returns a negative value."
                let s_last = self.ctx.alloc_retained_state(-1.0);

                let prev = self.ctx.retained_prev(s_prev);
                let t_prev = self.ctx.retained_prev(s_time);
                let last = self.ctx.retained_prev(s_last);

                self.ctx.store_retained(s_prev, cur);
                self.ctx.store_retained(s_time, now);

                let cur_ge = self.ctx.ins().fge(cur, F_ZERO);
                let cur_le = self.ctx.ins().fle(cur, F_ZERO);
                let prev_lt = self.ctx.ins().flt(prev, F_ZERO);
                let prev_gt = self.ctx.ins().fgt(prev, F_ZERO);
                let rising = self.and(prev_lt, cur_ge);
                let falling = self.and(prev_gt, cur_le);

                // "If it is set to 0, the last_crossing() will return the most
                // recent time the input expression had either a rise or falling edge
                // transition. If direction is +1 (-1), [...] rising (falling)."
                // Omitted behaves as 0.
                let crossed = match args.get(1) {
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
                };

                // Two distinct time points are needed to interpolate between, so
                // nothing is detected until time has moved. This also keeps dc, ac
                // and noise -- where `$abstime` stands still -- returning the
                // initial negative value.
                let advanced = self.ctx.ins().fgt(now, t_prev);
                let crossed = self.and(crossed, advanced);

                // Linear interpolation between the two straddling points:
                //   t = t_prev + (now - t_prev) * (0 - prev) / (cur - prev)
                // A crossing implies `cur != prev`, but the division is evaluated
                // either way, so pin the denominator to 1 when it is not.
                let dt = self.ctx.ins().fsub(now, t_prev);
                let den = self.ctx.ins().fsub(cur, prev);
                let one = self.ctx.fconst(1.0);
                let den = self.ctx.make_select(crossed, |_s, taken| if taken { den } else { one });
                let frac = self.ctx.ins().fdiv(prev, den);
                let step = self.ctx.ins().fmul(dt, frac);
                // t_prev + dt * (-prev / den), written as a subtraction to keep the
                // negation out of the way.
                let t_cross = self.ctx.ins().fsub(t_prev, step);

                let last =
                    self.ctx.make_select(crossed, |_s, taken| if taken { t_cross } else { last });
                self.ctx.store_retained(s_last, last);
                last
            }
            BuiltIn::limit => self.lower_expr(args[0]),

            // `ac_stim` is an AC small-signal stimulus: it is defined to be zero in the
            // large-signal (DC/transient) domain, which is what a contribution lowers.
            // Previously only the `no_equations` guard above matched, so a contributing
            // use (`V(a,b) <+ ac_stim(...)`) fell through to `unreachable!()` and
            // crashed the compiler. Actual AC-analysis injection is not implemented yet.
            BuiltIn::ac_stim => F_ZERO,

            BuiltIn::table_model => self.lower_table_model(args),

            BuiltIn::zi_nd | BuiltIn::zi_np | BuiltIn::zi_zd | BuiltIn::zi_zp => {
                self.lower_zi_filter(builtin, args)
            }

            _ => unreachable!(),
        }
    }

    /// A distribution call whose parameters follow the seed: `f(seed, a [, b])`,
    /// with an optional trailing `"global"`/`"instance"` string this does not need.
    /// `round` picks the integer form, which rounds half away from zero.
    fn lower_rng_args(&mut self, dist: RngDist, args: &[ExprId], round: bool) -> Value {
        let real = |this: &mut Self, i: usize| match args.get(i) {
            Some(&arg) if !this.body.is_missing(arg) => {
                let ty = this.body.expr_type(arg);
                let val = this.lower_expr(arg);
                match ty {
                    Type::Real => val,
                    ref other => this.ctx.insert_cast(val, other, &Type::Real),
                }
            }
            _ => F_ZERO,
        };
        let a = real(self, 1);
        let b = real(self, 2);
        let val = self.lower_rng(dist, args, a, b);
        if round {
            // The `$dist_*` forms are specified to round half away from zero rather
            // than truncate. The result stays a real -- that is what these are
            // declared to return in the analog context (DIST_*_ARG -> Real); only
            // `$random` hands back an integer.
            let half = self.ctx.fconst(0.5);
            let negative = self.ctx.ins().flt(val, F_ZERO);
            let up = self.ctx.ins().fadd(val, half);
            let down = self.ctx.ins().fsub(val, half);
            let rounded = self.ctx.make_select(negative, |_s, taken| if taken { down } else { up });
            // `fitrunc`, not a real-to-integer cast: that cast rounds, and rounding
            // a value that has already had the half added lands one out.
            let as_int = self.ctx.ins().fitrunc(rounded);
            self.ctx.insert_cast(as_int, &Type::Integer, &Type::Real)
        } else {
            val
        }
    }

    /// Draw from `dist`, and leave the seed where the draw left it.
    ///
    /// 9.13.1: "If the random_seed argument is specified it is an inout argument;
    /// that is, a value is passed to the function and a different value is
    /// returned." When it names a variable that is what happens, and the variable
    /// keeps its value between evaluations because a read that precedes its write
    /// earns retention -- which is also what makes this converge: every Newton
    /// iteration of one timestep reads the same committed seed and so draws the
    /// same number, and only the accepted step advances it.
    ///
    /// A seed that is a parameter, a constant or absent gets a retained slot of its
    /// own instead, seeded from the expression on the first evaluation, per "an
    /// internal seed is created which is assigned the initial value".
    fn lower_rng(&mut self, dist: RngDist, args: &[ExprId], a: Value, b: Value) -> Value {
        let seed_var = args.first().and_then(|&arg| {
            if self.body.is_missing(arg) {
                return None;
            }
            match self.body.try_get_expr(arg) {
                Some(Expr::Read(Ref::Variable(var))) => Some(var),
                _ => None,
            }
        });

        // `Some(slot)` when the seed has nowhere of its own to live and needs a
        // retained slot instead of a variable to be written back to.
        let (seed, slot) = match seed_var {
            Some(var) => {
                let seed = self.ctx.use_place(PlaceKind::Var(var));
                (self.ctx.insert_cast(seed, &Type::Integer, &Type::Real), None)
            }
            None => {
                // Whatever the call names, taken once: a parameter, a constant, or
                // an arbitrary starting point when the seed was left out entirely.
                let initial = match args.first() {
                    Some(&arg) if !self.body.is_missing(arg) => {
                        let ty = self.body.expr_type(arg);
                        let val = self.lower_expr(arg);
                        match ty {
                            Type::Real => val,
                            ref other => self.ctx.insert_cast(val, other, &Type::Real),
                        }
                    }
                    _ => self.ctx.fconst(259341593.0),
                };
                let slot = self.ctx.alloc_retained_state(0.0);
                let carried = self.ctx.retained_prev(slot);
                let first = self.ctx.first_eval();
                let seed =
                    self.ctx.make_select(first, |_s, taken| if taken { initial } else { carried });
                (seed, Some(slot))
            }
        };

        let call_args = [seed, a, b];
        let value = self.ctx.call1(CallBackKind::RngValue(dist), &call_args);
        let next = self.ctx.call1(CallBackKind::RngSeed(dist), &call_args);

        match (seed_var, slot) {
            (Some(var), _) => {
                let next = self.ctx.insert_cast(next, &Type::Real, &Type::Integer);
                self.ctx.def_place(PlaceKind::Var(var), next);
            }
            (None, Some(slot)) => self.ctx.store_retained(slot, next),
            _ => {}
        }

        value
    }

    /// An optional analog-operator argument. Absent and written as a null argument
    /// (`transition(x, , 1n)`) are the same thing: the clause's default applies.
    fn opt_arg(&mut self, args: &[ExprId], idx: usize) -> Option<Value> {
        match args.get(idx) {
            Some(&arg) if !self.body.is_missing(arg) => Some(self.lower_expr(arg)),
            _ => None,
        }
    }

    /// Which side of a Z-filter's transfer function each argument gives, per
    /// VAMS-2023 4.5.12.1 to 4.5.12.4.
    fn zi_sides(builtin: BuiltIn) -> (zi_filter::Side, zi_filter::Side) {
        use zi_filter::Side::{Coeffs, Roots};
        match builtin {
            BuiltIn::zi_nd => (Coeffs, Coeffs),
            BuiltIn::zi_zd => (Roots, Coeffs),
            BuiltIn::zi_np => (Coeffs, Roots),
            BuiltIn::zi_zp => (Roots, Roots),
            _ => unreachable!("not a Z-transform filter"),
        }
    }

    /// VAMS-2023 4.5.12:
    ///
    ///   zi_nd ( expr , n , d , T [ , tau [ , t0 ] ] )
    ///
    /// and the three other forms, which differ only in whether each side is given
    /// as coefficients of `z^-k` or as roots. `hir::zi_filter` reduces all four to
    /// one difference equation.
    ///
    /// These are discrete-time filters: "A filter with unity transfer function acts
    /// like a simple sample-and-hold which samples every T seconds and exhibits no
    /// delay." So the input is sampled on the grid `t0 + m*T`, the difference
    /// equation runs once per sample, and the output transitions to the new value
    /// over `tau`.
    ///
    /// Table 4-20 makes the coefficients, `T` and `t0` constant expressions, so the
    /// whole sample grid and every coefficient is known here and the generated code
    /// is a fixed dot product over retained history. Only `expr` and `tau` are
    /// dynamic.
    ///
    /// The output is handed to [`Self::emit_transition`] rather than reimplemented:
    /// 4.5.12 asks for the same thing 4.5.8 does, including that with no `tau`
    /// given "the timestep is not controlled to resolve the trailing corner of the
    /// transition", which is that code's own rule.
    ///
    /// Whatever cannot be built returns zero, which `hir_ty` makes unreachable by
    /// rejecting the call first.
    fn lower_zi_filter(&mut self, builtin: BuiltIn, args: &[ExprId]) -> Value {
        let (num_side, den_side) = Self::zi_sides(builtin);
        // A null `zeros` argument is the empty product: "The zeros argument may be
        // represented as a null argument."
        let num = match args.get(1) {
            Some(&arg) if !self.body.is_missing(arg) => match self.const_real_array(arg) {
                Some(num) => Some(num),
                None => return F_ZERO,
            },
            _ => None,
        };
        let den = match args.get(2).copied().filter(|&a| !self.body.is_missing(a)) {
            Some(arg) => match self.const_real_array(arg) {
                Some(den) => den,
                None => return F_ZERO,
            },
            None => return F_ZERO,
        };
        let filter = match zi_filter::Filter::build(num.as_deref(), num_side, &den, den_side) {
            Ok(filter) => filter,
            Err(_) => return F_ZERO,
        };
        // 4.5.14: a constant expression "remains static throughout an analysis",
        // which a `parameter` does even though its value is the simulator's to set.
        // `T` and `t0` only feed the scheduling arithmetic, never a coefficient, so
        // they are lowered as values and a parameter works.
        let period = self.lower_expr(args[3]);
        let start = match args.get(5).copied().filter(|&a| !self.body.is_missing(a)) {
            Some(arg) => self.lower_expr(arg),
            None => F_ZERO,
        };

        let input = self.lower_expr(args[0]);
        // `tau` is the one dynamic argument besides the input. Left at zero when
        // absent, which `emit_transition` floors to its negligible default.
        let tau = match args.get(4).copied().filter(|&a| !self.body.is_missing(a)) {
            Some(arg) => self.lower_expr(arg),
            None => F_ZERO,
        };

        let (m, n) = filter.order();
        let now = self.ctx.use_param(ParamKind::Abstime);
        let no_bound = self.ctx.fconst(f64::MAX);

        // -- state -------------------------------------------------------------
        // The next sample instant. A retained slot's initial value has to be a
        // constant, and `t0` need not be one, so the first evaluation reads it from
        // the argument instead: "t0 specifies the time of the first transition
        // [...] If not given, the first transition occurs at t=0."
        let s_next = self.ctx.alloc_retained_state(0.0);
        // The sampled inputs, newest first, and the outputs behind the newest. The
        // filter starts from rest, so every slot starts at zero.
        let s_x: Vec<_> = (0..=m).map(|_| self.ctx.alloc_retained_state(0.0)).collect();
        let s_y: Vec<_> = (0..n.max(1)).map(|_| self.ctx.alloc_retained_state(0.0)).collect();

        let first = self.ctx.first_eval();
        let next_held = self.ctx.retained_prev(s_next);
        let next_prev = self.ctx.make_select(first, |_s, b| if b { start } else { next_held });
        let x_prev: Vec<_> = s_x.iter().map(|&s| self.ctx.retained_prev(s)).collect();
        let y_prev: Vec<_> = s_y.iter().map(|&s| self.ctx.retained_prev(s)).collect();

        // -- has a sample instant arrived? -------------------------------------
        // The same arithmetic `timer` uses, including skipping whole periods in
        // case the solver stepped past several at once.
        let one = self.ctx.fconst(1.0);
        // "T [...] is mandatory, and shall be positive." A period that is not
        // leaves the filter parked rather than dividing by zero; a literal one is
        // rejected outright during validation.
        let usable = self.ctx.ins().fgt(period, F_ZERO);
        let divisor = self.ctx.make_select(usable, |_s, b| if b { period } else { one });
        let due = self.ctx.ins().fge(now, next_prev);
        let reached = self.and(due, usable);
        let elapsed = self.ctx.ins().fsub(now, next_prev);
        let periods = self.ctx.ins().fdiv(elapsed, divisor);
        let periods = self.ctx.ins().floor(periods);
        let periods = self.ctx.ins().fadd(periods, one);
        let advance = self.ctx.ins().fmul(divisor, periods);
        let after = self.ctx.ins().fadd(next_prev, advance);
        let next = self.ctx.make_select(reached, |_s, b| if b { after } else { next_prev });
        self.ctx.store_retained(s_next, next);

        // Ask for a timepoint on the next sample instant, so the grid is the one
        // the filter was written with rather than whatever the solver stepped onto.
        let remaining = self.ctx.ins().fsub(next, now);
        let ahead = self.ctx.ins().fgt(remaining, F_ZERO);
        let bound = self.ctx.make_select(ahead, |_s, b| if b { remaining } else { no_bound });
        self.bound_step(bound);

        // -- the difference equation ------------------------------------------
        // Shifted histories, as they would be if this evaluation takes a sample.
        let mut x_new = Vec::with_capacity(m + 1);
        x_new.push(input);
        x_new.extend(x_prev.iter().take(m).copied());

        //   y[m] = ( sum_k n_k x[m-k] - sum_{k>=1} d_k y[m-k] ) / d_0
        let inv_d0 = 1.0 / filter.den[0];
        let mut acc = F_ZERO;
        for (k, coeff) in filter.num.iter().enumerate() {
            let term = self.emit_scaled(x_new[k], coeff * inv_d0);
            acc = if acc == F_ZERO { term } else { self.ctx.ins().fadd(acc, term) };
        }
        for (k, coeff) in filter.den.iter().enumerate().skip(1) {
            let term = self.emit_scaled(y_prev[k - 1], -coeff * inv_d0);
            acc = if acc == F_ZERO { term } else { self.ctx.ins().fadd(acc, term) };
        }

        let mut y_new = Vec::with_capacity(y_prev.len());
        y_new.push(acc);
        y_new.extend(y_prev.iter().take(y_prev.len() - 1).copied());

        // A sample is only taken when an instant has arrived; otherwise the history
        // and the held output stand.
        for (k, &slot) in s_x.iter().enumerate() {
            let (new, old) = (x_new[k], x_prev[k]);
            let val = self.ctx.make_select(reached, |_s, b| if b { new } else { old });
            self.ctx.store_retained(slot, val);
        }
        for (k, &slot) in s_y.iter().enumerate() {
            let (new, old) = (y_new[k], y_prev[k]);
            let val = self.ctx.make_select(reached, |_s, b| if b { new } else { old });
            self.ctx.store_retained(slot, val);
        }

        // The output is the newest sample, held until the next one, ramped over
        // `tau`. No delay: a unity filter "exhibits no delay".
        let held = self.ctx.make_select(reached, |_s, b| if b { acc } else { y_prev[0] });
        self.emit_transition(held, F_ZERO, tau, tau)
    }

    /// `val * coeff` with the constant folded away where it is 0 or 1.
    fn emit_scaled(&mut self, val: Value, coeff: f64) -> Value {
        if coeff == 0.0 {
            return F_ZERO;
        }
        if coeff == 1.0 {
            return val;
        }
        let coeff = self.ctx.fconst(coeff);
        self.ctx.ins().fmul(val, coeff)
    }

    /// `min(a, b)` on reals. The MIR has no intrinsic for it.
    fn fmin(&mut self, a: Value, b: Value) -> Value {
        let lt = self.ctx.ins().flt(a, b);
        self.ctx.make_select(lt, |_s, t| if t { a } else { b })
    }

    /// `max(a, b)` on reals.
    fn fmax(&mut self, a: Value, b: Value) -> Value {
        let gt = self.ctx.ins().fgt(a, b);
        self.ctx.make_select(gt, |_s, t| if t { a } else { b })
    }

    /// VAMS-2023 4.5.7's rules for the delay itself, which both realizations of
    /// `absdelay` need. Returns the delay in force and the span of history that has
    /// to be kept to serve it.
    ///
    /// The two differ only when `maxdelay` is given, and that is the whole point of
    /// the argument: it is what tells the simulator how far back to remember.
    fn absdelay_window(&mut self, delay: Value, max_delay: Option<Value>) -> (Value, Value) {
        // "In all cases td shall be a positive number." A negative one is not a
        // prediction of the future, it is a mistake; the delay is floored instead.
        let delay = self.fmax(delay, F_ZERO);
        match max_delay {
            // "If the optional maxdelay is specified, then td can vary. If td becomes
            // greater than maxdelay, maxdelay will be used as a substitute for td."
            Some(max) => {
                let max = self.fmax(max, F_ZERO);
                let delay = self.fmin(delay, max);
                (delay, max)
            }
            // "If maxdelay is not specified, the value of td when the absdelay() is
            // first evaluated shall be used and any future changes to td shall be
            // ignored." A retained slot holds that first value. In dc, where time
            // never moves, every point is still the first evaluation and the
            // argument is read afresh -- which is right: the frozen value is
            // per-analysis, and dc has no delay to speak of anyway.
            None => {
                let slot = self.ctx.alloc_retained_state(0.0);
                let first = self.ctx.first_eval();
                let held = self.ctx.retained_prev(slot);
                let frozen = self.ctx.make_select(first, |_s, b| if b { delay } else { held });
                self.ctx.store_retained(slot, frozen);
                (frozen, frozen)
            }
        }
    }

    /// VAMS-2023 4.5.7 realized inside the model, for simulators that do not
    /// implement the `OsdiAbsDelayInfo` protocol:
    ///
    ///   Output(t) = Input(max(t - td, 0))
    ///
    /// The operator needs the input's history, so the model carries it: `depth`
    /// retained (time, value) pairs, newest first, with the value at `t - td` read
    /// off by linear interpolation between the two samples that bracket it. The
    /// clause asks for exactly that reading -- `absdelay` "implements the absolute
    /// transport delay for continuous waveforms (use the transition() operator to
    /// delay discrete-valued waveforms)", and interpolating a continuous waveform is
    /// not an approximation of a different answer, it is the same answer the
    /// simulator's own time discretization already gives.
    ///
    /// # A shift register, not a ring
    ///
    /// MIR has no memory operations, so a moving write index would have to be lowered
    /// as a select chain per slot -- quadratic in the depth, for both the write and
    /// the read. Shifting instead costs one select per slot, and nothing else about a
    /// ring is wanted here: the samples are always read newest-to-oldest.
    ///
    /// # The sample grid
    ///
    /// The history has to span the delay, and `depth` samples cannot span it if they
    /// are recorded closer together than `window / depth`. The solver's own stepping
    /// is no help: it shrinks the step for reasons of its own, and a plain
    /// record-every-step history would then quietly run out of window and read a
    /// stale value. So a new sample is only recorded once it is at least a grid
    /// spacing newer than the last one, which keeps the window covered whatever the
    /// solver does, and `$bound_step` asks for a timepoint on the next grid instant,
    /// which keeps the recording at that spacing and resolves the output's own
    /// features to it.
    ///
    /// That spacing is the accuracy of this realization and the reason the descriptor
    /// protocol is the default: a delay is resolved to `window / (depth - 2)`, and
    /// the run takes at least that many steps per delay window.
    fn emit_absdelay(&mut self, input: Value, delay: Value, window: Value, depth: usize) -> Value {
        let depth = depth.max(AbsDelayMode::MIN_DEPTH);
        let now = self.ctx.use_param(ParamKind::Abstime);
        let one = self.ctx.fconst(1.0);
        let no_bound = self.ctx.fconst(f64::MAX);

        // -- state ------------------------------------------------------------
        let slots: Vec<_> = (0..depth)
            .map(|_| {
                let at = self.ctx.alloc_retained_state(0.0);
                let val = self.ctx.alloc_retained_state(0.0);
                (at, val)
            })
            .collect();
        let first = self.ctx.first_eval();
        let mut t_prev = Vec::with_capacity(depth);
        let mut v_prev = Vec::with_capacity(depth);
        for &(at, val) in &slots {
            let at = self.ctx.retained_prev(at);
            let val = self.ctx.retained_prev(val);
            // Nothing has been recorded yet, so the history reads flat at the input.
            // That is the figure's own starting condition -- "From time 0 until 2s,
            // the output remains at input(0)" -- and it makes the first point of any
            // analysis a pass-through, as 4.5.7 requires of dc and the operating
            // point.
            t_prev.push(self.ctx.make_select(first, |_s, b| if b { now } else { at }));
            v_prev.push(self.ctx.make_select(first, |_s, b| if b { input } else { val }));
        }

        // -- the sample grid --------------------------------------------------
        let spacing = self.ctx.fconst(1.0 / (depth - 2) as f64);
        let step = self.ctx.ins().fmul(window, spacing);
        let since = self.ctx.ins().fsub(now, t_prev[0]);
        let record = self.ctx.ins().fge(since, step);

        let mut t_cur = Vec::with_capacity(depth);
        let mut v_cur = Vec::with_capacity(depth);
        for k in 0..depth {
            let (t_in, v_in) = if k == 0 { (now, input) } else { (t_prev[k - 1], v_prev[k - 1]) };
            let (t_held, v_held) = (t_prev[k], v_prev[k]);
            t_cur.push(self.ctx.make_select(record, |_s, b| if b { t_in } else { t_held }));
            v_cur.push(self.ctx.make_select(record, |_s, b| if b { v_in } else { v_held }));
        }
        for (k, &(at, val)) in slots.iter().enumerate() {
            self.ctx.store_retained(at, t_cur[k]);
            self.ctx.store_retained(val, v_cur[k]);
        }

        // -- read the history at t - td ---------------------------------------
        // The chain is built oldest first so the newest sample that starts at or
        // before the target wins. A target older than everything recorded reads the
        // oldest sample, which is both the clause's "output remains at input(0)" at
        // the start of an analysis and what a `td` that has just grown beyond the
        // history does: "switching the output back to input(0), since
        // input(max(t-td,0)) returns 0".
        //
        // Only the live pair carries the input, so the derivative of the output with
        // respect to it is the interpolation weight -- which autodiff works out for
        // itself, this being ordinary MIR arithmetic rather than a callback.
        let target = self.ctx.ins().fsub(now, delay);
        let mut out = v_cur[depth - 1];
        for k in (0..depth).rev() {
            let (t_hi, v_hi) = if k == 0 { (now, input) } else { (t_cur[k - 1], v_cur[k - 1]) };
            let span = self.ctx.ins().fsub(t_hi, t_cur[k]);
            let spans = self.ctx.ins().fgt(span, F_ZERO);
            let divisor = self.ctx.make_select(spans, |_s, b| if b { span } else { one });
            let into = self.ctx.ins().fsub(target, t_cur[k]);
            let frac = self.ctx.ins().fdiv(into, divisor);
            let frac = self.ctx.make_select(spans, |_s, b| if b { frac } else { F_ZERO });
            let rise = self.ctx.ins().fsub(v_hi, v_cur[k]);
            let travelled = self.ctx.ins().fmul(frac, rise);
            let val = self.ctx.ins().fadd(v_cur[k], travelled);
            let inside = self.ctx.ins().fge(target, t_cur[k]);
            out = self.ctx.make_select(inside, |_s, b| if b { val } else { out });
        }

        // -- timepoints -------------------------------------------------------
        // The output is the input shifted, so nothing in the rest of the circuit
        // tells the solver where its features are: the step is capped at the grid
        // spacing, which also keeps the recording at that spacing.
        //
        // A cap, deliberately, and not a request for a timepoint *on* the next grid
        // instant the way `timer` and the Z filters ask for their sample instants.
        // Those consume the instant when they reach it, so what they ask for jumps
        // forward by a whole period; this grid is relative to the last sample taken,
        // so landing just short of it would leave a sliver to ask for next, and the
        // sliver halves until the solver gives up with "timestep too small".
        //
        // A delay of zero is a pass-through and needs no help. History is still
        // recorded there, because a `td` guarded by `maxdelay` may grow again.
        let delayed = self.ctx.ins().fgt(delay, F_ZERO);
        let bound = self.ctx.make_select(delayed, |_s, b| if b { step } else { no_bound });
        self.bound_step(bound);

        out
    }

    /// VAMS-2023 4.5.8: the piecewise-linear realization of
    ///
    ///   transition ( expr [ , td [ , rise_time [ , fall_time [ , time_tol ] ] ] ] )
    ///
    /// `expr` is expected to evaluate to a piecewise constant waveform. Each change
    /// of it schedules a transition `td` later, which then ramps linearly to the new
    /// value, taking `rise_time` upwards and `fall_time` downwards: "td models
    /// transport delay and rise_time and fall_time model inertial delay". The clause
    /// is explicit that the result "describes a piecewise linear function over time".
    ///
    /// This used to be a first-order lag with the rise time as its time constant,
    /// which left the output 1/e short at the instant the ramp should have arrived,
    /// never arrived at all, and dropped `td` on the floor (issue #42). A lag is the
    /// easier continuous function to write as a DAE residual, but it is not this
    /// operator: a model transitioning a logic level into a charge pump got the wrong
    /// charge per pulse out of it, and no `td` could give it a propagation delay.
    ///
    /// The ramp needs no unknown of its own. It is an explicit function of `$abstime`
    /// and of state carried from one accepted timestep to the next, so there is no
    /// equation here any more and no Jacobian entry to go with it.
    ///
    /// # Transport delay
    ///
    /// `td` is a transport delay, not an inertial one, so a change arriving while an
    /// earlier one is still inside the delay window must not cancel it: 4.5.8 says a
    /// transition function "can have an arbitrary number of transitions pending" and
    /// that this "can be used to implement transport delay for discrete-valued
    /// signals". Pending transitions therefore sit in a queue, earliest first, and
    /// one is taken per evaluation.
    ///
    /// A fixed lowering cannot offer an arbitrary number of slots, so `PENDING` is
    /// the bound. It is reached only when `expr` changes more than that many times
    /// within one `td`; past it the newest two changes coalesce, which keeps the
    /// value the output eventually reaches correct and loses a glitch. The other half
    /// of the clause, "deleting any transitions which would follow a newly scheduled
    /// transition", is implemented too, and is reachable only when `td` shrinks
    /// during the simulation -- with a constant delay a new transition is always
    /// scheduled after the pending ones.
    ///
    /// # Interrupted transitions
    ///
    /// When a transition starts while another is still in flight, 4.5.8 does not ramp
    /// from the current value at the current slope. It "computes the slope which
    /// completes the transition from the origin (not the current value) in the
    /// specified transition time", where the origin is the old destination if the new
    /// destination is below the current value, and the *first* origin if it is above.
    /// The ramp then runs from the current value at that slope, so an interrupted
    /// transition finishes early rather than taking the full time again. That is what
    /// turns a pulse shorter than `rise_time` into a reduced-amplitude glitch instead
    /// of a full-swing one, which is the behaviour a phase detector depends on.
    ///
    /// # Timepoints
    ///
    /// "The transition function causes the simulator to place time-points at both
    /// corners of a transition", so `$bound_step` is capped at the distance to each
    /// pending start instant and to the arrival of the ramp in progress. The trailing
    /// corner is deliberately *not* requested when the transition time is the
    /// negligible default, which is the clause's own exemption: forcing it "would
    /// result in poor performance".
    ///
    /// That is also why `time_tol` is accepted and then not used. It asks for a point
    /// within `time_tol` of a corner, and landing on the corner exactly satisfies any
    /// tolerance -- the same reasoning `timer` is lowered with.
    ///
    /// What stays step-dependent is *when a change is noticed*: `expr` steps, so
    /// there is no crossing to interpolate. In practice the change is driven by a
    /// `cross` or `timer` event in the same module, and those already steer the
    /// solver onto the instant it happens.
    fn lower_transition(&mut self, target: Value, args: &[ExprId]) -> Value {
        let td = self.opt_arg(args, 1).unwrap_or(F_ZERO);
        let rise = self.opt_arg(args, 2).unwrap_or(F_ZERO);
        // "If only a positive rise_time value is specified, the simulator uses it for
        // both rise and fall times."
        let fall = self.opt_arg(args, 3).unwrap_or(rise);
        self.emit_transition(target, td, rise, fall)
    }

    /// The transition machinery itself, which 4.5.12's Z-transform filters share:
    /// the clause describes their output in the same terms, down to not resolving
    /// the trailing corner when the transition time is the default.
    fn emit_transition(&mut self, target: Value, td: Value, rise: Value, fall: Value) -> Value {
        // 4.5.8: with `rise_time`/`fall_time` unspecified or zero they "default to the
        // value defined by `default_transition"; without that directive -- which this
        // compiler does not implement -- "a negligible, but non-zero, transition time
        // is used", because "forcing a zero-duration transition is undesirable" for
        // convergence. This is that negligible time, and the floor the lag
        // realization used as its time constant, so a bare `transition(x)` keeps its
        // character.
        const MIN_RAMP: f64 = 1e-12;
        // How many transitions may be in flight through `td` at once. Each slot costs
        // two retained values.
        const PENDING: usize = 4;
        // An empty queue slot: a time no simulation reaches, kept far enough below
        // `f64::MAX` that `NEVER + td` is still finite.
        const NEVER: f64 = 1e300;

        let now = self.ctx.use_param(ParamKind::Abstime);

        let min_ramp = self.ctx.fconst(MIN_RAMP);
        let rise = self.fmax(rise, min_ramp);
        let fall = self.fmax(fall, min_ramp);

        let one = self.ctx.fconst(1.0);
        let unset = self.ctx.fconst(NEVER);
        // Half the marker, so a slot reads as empty however `NEVER` was arrived at.
        let unset_test = self.ctx.fconst(NEVER / 2.0);
        // `$bound_step`'s "no opinion", matching how `timer` spells it.
        let no_bound = self.ctx.fconst(f64::MAX);

        // -- state ------------------------------------------------------------
        // The input value last seen, so a change can be detected.
        let s_tgt = self.ctx.alloc_retained_state(0.0);
        // Transitions scheduled but not yet started, earliest first.
        let mut queue = Vec::with_capacity(PENDING);
        for _ in 0..PENDING {
            let at = self.ctx.alloc_retained_state(NEVER);
            let to = self.ctx.alloc_retained_state(0.0);
            queue.push((at, to));
        }
        // The transition in progress: when it started and from what value, where it
        // is going, the origin its slope was computed from, that slope, and the
        // transition time the slope was computed over.
        let s_t0 = self.ctx.alloc_retained_state(0.0);
        let s_v0 = self.ctx.alloc_retained_state(0.0);
        let s_dest = self.ctx.alloc_retained_state(0.0);
        let s_origin = self.ctx.alloc_retained_state(0.0);
        let s_slope = self.ctx.alloc_retained_state(0.0);
        let s_span = self.ctx.alloc_retained_state(0.0);

        let tgt_prev = self.ctx.retained_prev(s_tgt);
        let mut at_prev = Vec::with_capacity(PENDING);
        let mut to_prev = Vec::with_capacity(PENDING);
        for k in 0..PENDING {
            at_prev.push(self.ctx.retained_prev(queue[k].0));
            to_prev.push(self.ctx.retained_prev(queue[k].1));
        }
        let t0 = self.ctx.retained_prev(s_t0);
        let v0 = self.ctx.retained_prev(s_v0);
        let dest = self.ctx.retained_prev(s_dest);
        let origin = self.ctx.retained_prev(s_origin);
        let slope = self.ctx.retained_prev(s_slope);

        // -- where the transition in progress has got to ----------------------
        // A line from `v0` at `t0` at `slope`, clamped to the interval it travels.
        // The clamp also covers `now < t0`, where the line runs backwards away from
        // `dest` and is pinned at `v0`.
        let elapsed = self.ctx.ins().fsub(now, t0);
        let travelled = self.ctx.ins().fmul(slope, elapsed);
        let raw = self.ctx.ins().fadd(v0, travelled);
        let lo = self.fmin(v0, dest);
        let hi = self.fmax(v0, dest);
        let cur = self.fmax(raw, lo);
        let cur = self.fmin(cur, hi);

        // 4.5.8: "In DC analysis, transition() passes the value of the expr directly
        // to its output", and the first point of a transient has no history to ramp
        // from either. Retained state commits only once time moves, so this reads
        // true for every point of a dc sweep and for the operating point, and false
        // from the first accepted transient step onwards.
        let first = self.ctx.first_eval();
        let cur = self.ctx.make_select(first, |_s, b| if b { target } else { cur });

        // -- schedule a transition for each change of the input ---------------
        // `expr` is piecewise constant, so an exact comparison is the change test:
        // there is no tolerance to apply to a signal that steps.
        let changed = self.ctx.ins().fne(target, tgt_prev);
        let changed = self.lower_select_with(first, |_| FALSE, |_| changed);
        let starts = self.ctx.ins().fadd(now, td);

        // "deleting any transitions which would follow a newly scheduled transition"
        let mut kept_at = Vec::with_capacity(PENDING);
        for k in 0..PENDING {
            let at = at_prev[k];
            let after = self.ctx.ins().fge(at, starts);
            let drop = self.and(changed, after);
            kept_at.push(self.ctx.make_select(drop, |_s, b| if b { unset } else { at }));
        }

        // Push into the first slot that is free. The last slot also catches a push
        // that found none, so the queue degrades by coalescing its newest two
        // entries rather than by dropping the change and losing the final value.
        let mut empty = Vec::with_capacity(PENDING);
        for k in 0..PENDING {
            empty.push(self.ctx.ins().fge(kept_at[k], unset_test));
        }
        let mut push = Vec::with_capacity(PENDING);
        let mut placed = FALSE;
        for k in 0..PENDING - 1 {
            let here = match k {
                0 => empty[0],
                _ => {
                    let prev_taken = self.lower_select_with(empty[k - 1], |_| FALSE, |_| TRUE);
                    self.and(empty[k], prev_taken)
                }
            };
            let here = self.and(changed, here);
            placed = self.or(placed, here);
            push.push(here);
        }
        let spill = self.lower_select_with(placed, |_| FALSE, |_| TRUE);
        push.push(self.and(changed, spill));

        let mut pushed_at = Vec::with_capacity(PENDING);
        let mut pushed_to = Vec::with_capacity(PENDING);
        for k in 0..PENDING {
            let (at, to, p) = (kept_at[k], to_prev[k], push[k]);
            pushed_at.push(self.ctx.make_select(p, |_s, b| if b { starts } else { at }));
            pushed_to.push(self.ctx.make_select(p, |_s, b| if b { target } else { to }));
        }

        // -- start the head of the queue once its instant arrives -------------
        // Pushing before testing is what makes `td` of zero start the ramp in this
        // evaluation rather than the next one.
        let head_at = pushed_at[0];
        let head_set = self.ctx.ins().flt(head_at, unset_test);
        let head_due = self.ctx.ins().fle(head_at, now);
        let due = self.and(head_set, head_due);
        let new_dest = pushed_to[0];

        // The interrupt rules. "If the new final value level is below the value level
        // at the point of the interruption (the current value), transition() uses the
        // old destination as the origin. If the new destination is above the current
        // level, the first origin is retained." A transition that has already arrived
        // has no origin to retain and starts from where it is.
        let in_flight = self.ctx.ins().fne(cur, dest);
        let below = self.ctx.ins().flt(new_dest, cur);
        let kept_origin = self.ctx.make_select(below, |_s, b| if b { dest } else { origin });
        let new_origin = self.ctx.make_select(in_flight, |_s, b| if b { kept_origin } else { cur });
        // Retaining the first origin assumes the new destination lies beyond the
        // current value, which is the case the clause is written for. A destination
        // *between* the origin and the current value would otherwise be given a slope
        // pointing away from it, so fall back to a fresh transition there.
        let from_origin = self.ctx.ins().fsub(cur, new_origin);
        let to_dest = self.ctx.ins().fsub(new_dest, cur);
        let spanned = self.ctx.ins().fmul(from_origin, to_dest);
        let usable = self.ctx.ins().fge(spanned, F_ZERO);
        let new_origin = self.ctx.make_select(usable, |_s, b| if b { new_origin } else { cur });

        // "forces all positive transitions of expr to occur over rise_time and all
        // negative transitions to occur in fall_time", positive measured from the
        // origin. Both have been floored, so the slope needs no divisor guard.
        let up = self.ctx.ins().fgt(new_dest, new_origin);
        let new_span = self.ctx.make_select(up, |_s, b| if b { rise } else { fall });
        let swing = self.ctx.ins().fsub(new_dest, new_origin);
        let new_slope = self.ctx.ins().fdiv(swing, new_span);

        let t0_next = self.ctx.make_select(due, |_s, b| if b { now } else { t0 });
        let v0_next = self.ctx.make_select(due, |_s, b| if b { cur } else { v0 });
        let dest_next = self.ctx.make_select(due, |_s, b| if b { new_dest } else { dest });
        let origin_next = self.ctx.make_select(due, |_s, b| if b { new_origin } else { origin });
        let slope_next = self.ctx.make_select(due, |_s, b| if b { new_slope } else { slope });
        let span_prev = self.ctx.retained_prev(s_span);
        let span_next = self.ctx.make_select(due, |_s, b| if b { new_span } else { span_prev });

        // The first evaluation sits at the target with nothing in flight.
        let t0_next = self.ctx.make_select(first, |_s, b| if b { now } else { t0_next });
        let v0_next = self.ctx.make_select(first, |_s, b| if b { target } else { v0_next });
        let dest_next = self.ctx.make_select(first, |_s, b| if b { target } else { dest_next });
        let origin_next = self.ctx.make_select(first, |_s, b| if b { target } else { origin_next });
        let slope_next = self.ctx.make_select(first, |_s, b| if b { F_ZERO } else { slope_next });
        let span_next = self.ctx.make_select(first, |_s, b| if b { F_ZERO } else { span_next });

        // Taking the head shifts the rest of the queue up.
        for k in 0..PENDING {
            let next_at = if k + 1 < PENDING { pushed_at[k + 1] } else { unset };
            let next_to = if k + 1 < PENDING { pushed_to[k + 1] } else { F_ZERO };
            let (at, to) = (pushed_at[k], pushed_to[k]);
            let at = self.ctx.make_select(due, |_s, b| if b { next_at } else { at });
            let to = self.ctx.make_select(due, |_s, b| if b { next_to } else { to });
            self.ctx.store_retained(queue[k].0, at);
            self.ctx.store_retained(queue[k].1, to);

            // A leading corner: every instant still pending wants a timepoint.
            let togo = self.ctx.ins().fsub(at, now);
            let ahead = self.ctx.ins().fgt(togo, F_ZERO);
            let set = self.ctx.ins().flt(at, unset_test);
            let want = self.and(ahead, set);
            let bound = self.ctx.make_select(want, |_s, b| if b { togo } else { no_bound });
            self.bound_step(bound);
        }

        self.ctx.store_retained(s_tgt, target);
        self.ctx.store_retained(s_t0, t0_next);
        self.ctx.store_retained(s_v0, v0_next);
        self.ctx.store_retained(s_dest, dest_next);
        self.ctx.store_retained(s_origin, origin_next);
        self.ctx.store_retained(s_slope, slope_next);
        self.ctx.store_retained(s_span, span_next);

        // The trailing corner: where the ramp in progress arrives. Skipped when the
        // transition time is the negligible default, per the clause's own exemption
        // against forcing very small timesteps for it.
        let remaining = self.ctx.ins().fsub(dest_next, v0_next);
        let moving = self.ctx.ins().fne(slope_next, F_ZERO);
        let divisor = self.ctx.make_select(moving, |_s, b| if b { slope_next } else { one });
        let duration = self.ctx.ins().fdiv(remaining, divisor);
        let arrival = self.ctx.ins().fadd(t0_next, duration);
        let togo = self.ctx.ins().fsub(arrival, now);
        let ahead = self.ctx.ins().fgt(togo, F_ZERO);
        let resolved = self.ctx.ins().fgt(span_next, min_ramp);
        let want = self.and(ahead, moving);
        let want = self.and(want, resolved);
        let bound = self.ctx.make_select(want, |_s, b| if b { togo } else { no_bound });
        self.bound_step(bound);

        cur
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
        // A null argument (`laplace_nd(V(in), , den)`, VAMS-2023 4.5.11) has no
        // expression at all. It means an empty coefficient vector; lowering it as an
        // expression would reach `get_expr`'s `invalid HIR` panic.
        if self.body.is_missing(arg) {
            return Vec::new();
        }
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
        self.lower_laplace_state_space(input, num, den)
    }

    /// Lower the root forms of the Laplace filter (VAMS-2023 4.5.11.1-4.5.11.3) by
    /// expanding each root vector into polynomial coefficients and reusing the
    /// `laplace_nd` realization. The number of roots is fixed at compile time (array
    /// lengths are static), so only the coefficient arithmetic is emitted.
    fn lower_laplace_roots(&mut self, args: &[ExprId], zeros: bool, poles: bool) -> Value {
        let input = self.lower_expr(args[0]);
        let num = if zeros {
            let roots = self.array_coeffs(args[1]);
            self.expand_roots(&roots)
        } else {
            self.array_coeffs(args[1])
        };
        let den = if poles {
            let roots = self.array_coeffs(args[2]);
            self.expand_roots(&roots)
        } else {
            self.array_coeffs(args[2])
        };
        self.lower_laplace_state_space(input, num, den)
    }

    /// Expand a flat vector of (real, imaginary) root pairs into the real polynomial
    /// coefficients of `prod_k (1 - s/r_k)`, ascending powers of `s`.
    ///
    /// A root of zero contributes a bare `s` factor instead of `1 - s/r`, as
    /// 4.5.11.1 requires. The LRM also requires a complex root's conjugate to be
    /// present, which is what makes the product real: the expansion carries the
    /// imaginary parts through and drops them at the end, so no case analysis on
    /// whether a given root is real is needed -- which matters because the roots are
    /// runtime values.
    fn expand_roots(&mut self, roots: &[Value]) -> Vec<Value> {
        let one = self.ctx.fconst(1.0);
        // running polynomial, real and imaginary parts, ascending powers of s
        let mut re = vec![one];
        let mut im = vec![F_ZERO];

        for pair in roots.chunks(2) {
            let sigma = pair[0];
            // an odd-length vector is malformed; treat the missing part as zero
            let omega = pair.get(1).copied().unwrap_or(F_ZERO);

            // |r|^2 decides both the reciprocal and whether the root is zero
            let s2 = self.ctx.ins().fmul(sigma, sigma);
            let w2 = self.ctx.ins().fmul(omega, omega);
            let mag2 = self.ctx.ins().fadd(s2, w2);
            let is_zero = self.ctx.ins().feq(mag2, F_ZERO);
            // divide by 1 instead of 0 in the branch the select discards
            let denom = self.ctx.make_select(is_zero, |_ctx, b| if b { one } else { mag2 });

            // 1/r = conj(r)/|r|^2, so the s coefficient of (1 - s/r) is
            // (-sigma + j*omega)/|r|^2; a zero root makes the factor a bare s.
            let inv_re = self.ctx.ins().fdiv(sigma, denom);
            let c1_re_nonzero = self.ctx.ins().fneg(inv_re);
            let c1_im_nonzero = self.ctx.ins().fdiv(omega, denom);

            let c0 = self.ctx.make_select(is_zero, |_ctx, b| if b { F_ZERO } else { one });
            let c1_re =
                self.ctx.make_select(is_zero, |_ctx, b| if b { one } else { c1_re_nonzero });
            let c1_im =
                self.ctx.make_select(is_zero, |_ctx, b| if b { F_ZERO } else { c1_im_nonzero });

            // multiply the running polynomial by [c0, c1]
            let mut next_re = vec![F_ZERO; re.len() + 1];
            let mut next_im = vec![F_ZERO; re.len() + 1];
            for i in 0..re.len() {
                // times c0, whose imaginary part is always zero
                let t_re = self.ctx.ins().fmul(re[i], c0);
                let t_im = self.ctx.ins().fmul(im[i], c0);
                next_re[i] = self.ctx.ins().fadd(next_re[i], t_re);
                next_im[i] = self.ctx.ins().fadd(next_im[i], t_im);

                // times c1, shifted up one power of s
                let rr = self.ctx.ins().fmul(re[i], c1_re);
                let ii = self.ctx.ins().fmul(im[i], c1_im);
                let ri = self.ctx.ins().fmul(re[i], c1_im);
                let ir = self.ctx.ins().fmul(im[i], c1_re);
                let real = self.ctx.ins().fsub(rr, ii);
                let imag = self.ctx.ins().fadd(ri, ir);
                next_re[i + 1] = self.ctx.ins().fadd(next_re[i + 1], real);
                next_im[i + 1] = self.ctx.ins().fadd(next_im[i + 1], imag);
            }
            re = next_re;
            im = next_im;
        }

        // The conjugate pairs the LRM requires cancel the imaginary parts.
        re
    }

    fn lower_laplace_state_space(
        &mut self,
        input: Value,
        num: Vec<Value>,
        den: Vec<Value>,
    ) -> Value {
        // A null numerator (`laplace_nd(V(in), , den)`, VAMS-2023 4.5.11) is the
        // empty product of zeros, so the numerator is *unity* -- H(s) = 1/D(s) --
        // not zero, which is what an empty coefficient list would otherwise produce.
        // The root forms need no such case: an empty product of factors is already 1.
        let num = if num.is_empty() {
            let one = self.ctx.fconst(1.0);
            vec![one]
        } else {
            num
        };
        let n = den.len().saturating_sub(1); // filter order
        if n == 0 {
            // A null denominator likewise means D(s) = 1, so the filter is its
            // numerator; only the constant case is realizable without differentiating
            // the input, which is the same restriction as before.
            if den.is_empty() {
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

fn body_has_return(body: &BodyRef<'_>) -> bool {
    fn walk(body: &BodyRef<'_>, stmt: hir::StmtId) -> bool {
        match body.get_stmt(stmt) {
            Some(Stmt::Return { .. }) => true,
            Some(Stmt::Block { body: stmts }) => stmts.iter().any(|&s| walk(body, s)),
            Some(Stmt::If { then_branch, else_branch, .. }) => {
                walk(body, then_branch) || walk(body, else_branch)
            }
            Some(Stmt::WhileLoop { body: b, .. }) | Some(Stmt::EventControl { body: b, .. }) => {
                walk(body, b)
            }
            Some(Stmt::ForLoop { init, incr, body: b, .. }) => {
                walk(body, init) || walk(body, incr) || walk(body, b)
            }
            Some(Stmt::Case { case_arms, .. }) => case_arms.iter().any(|arm| walk(body, arm.body)),
            _ => false,
        }
    }
    body.entry().iter().any(|&s| walk(body, s))
}
