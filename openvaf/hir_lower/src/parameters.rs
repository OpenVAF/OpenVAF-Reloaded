use std::f64::NEG_INFINITY;
use std::mem::replace;

use hir::{CompilationDB, ConstraintValue, ParamConstraint, Parameter, Type};
use lasso::Rodeo;
use mir::builder::InstBuilder;
use mir::{Block, FuncRef, Function, Opcode, Value, FALSE, INFINITY};
use mir_build::{FunctionBuilder, FunctionBuilderContext};
use stdx::packed_option::ReservedValue;
use syntax::ast::ConstraintKind;

use crate::body::BodyLoweringCtx;
use crate::callbacks::ParamInfoKind;
use crate::ctx::LoweringCtx;
use crate::{CallBackKind, HirInterner, ParamKind, PlaceKind};

#[derive(Clone, Copy, Debug)]
struct CmpOps {
    lt: Option<Opcode>,
    le: Option<Opcode>,
    eq: Opcode,
}

impl CmpOps {
    fn from_ty(ty: &Type) -> Self {
        match ty {
            Type::Real => CmpOps { lt: Some(Opcode::Flt), le: Some(Opcode::Fle), eq: Opcode::Feq },
            Type::Integer => {
                CmpOps { lt: Some(Opcode::Ilt), le: Some(Opcode::Ile), eq: Opcode::Ieq }
            }
            Type::String => CmpOps { lt: None, le: None, eq: Opcode::Seq },
            Type::Array { ty, .. } => Self::from_ty(ty),
            Type::EmptyArray => CmpOps { lt: None, le: None, eq: Opcode::Ieq },
            _ => unreachable!(),
        }
    }

    fn in_bound(self, inclusive: bool) -> Opcode {
        if inclusive {
            self.le.unwrap()
        } else {
            self.lt.unwrap()
        }
    }
}

// Instance parameter value in eval() is read always from instance, even if it is not given in the instance (bad)
// Instance parameter value in instance setup is read from instance, if not available there then it is read from model
// Storing the default values is performed in setup.rs at MIR->IR translation.
impl HirInterner {
    pub fn insert_param_init(
        &mut self,
        db: &CompilationDB,
        func: &mut Function,
        literals: &mut Rodeo,
        build_min_max: bool,
        build_stores: bool,
        params: &[Parameter],
    ) {
        // One default per element: an array parameter has as many as it was
        // declared with (VAMS-2023 3.4.4), a scalar has one.
        let mut default_vals: Vec<Vec<Value>> =
            if build_stores { vec![Vec::new(); params.len()] } else { vec![] };

        let f_neg_inf = func.dfg.fconst(NEG_INFINITY.into());
        let f_inf = INFINITY;
        let i_inf = func.dfg.iconst(i32::MAX);
        let i_neg_inf = func.dfg.iconst(i32::MIN);

        let mut ctx = FunctionBuilderContext::default();
        let (builder, term) = FunctionBuilder::edit(func, literals, &mut ctx, false);
        let mut ctx = LoweringCtx::new(db, builder, true, self);

        for (i, param) in params.iter().copied().enumerate() {
            // An array parameter is initialised element by element; everything
            // below is the scalar case.
            if !param.ty(db).dim().is_empty() {
                let vals = Self::init_array_param(&mut ctx, db, param, build_stores);
                if build_stores {
                    default_vals[i] = vals;
                }
                continue;
            }

            let mut param_val = ctx.use_param(ParamKind::Param(param));
            let param_given = ctx.use_param(ParamKind::ParamGiven { param });

            // create a temporary to hold onto the uses
            let new_val = ctx.func.make_param(0u32.into());
            ctx.dfg_mut().replace_uses(param_val, new_val);

            let body = param.init(db);
            let ty = param.ty(db);
            let bounds = param.bounds(db);

            let ops = CmpOps::from_ty(&ty);
            let invalid = ctx.dec_callback(CallBackKind::ParamInfo(ParamInfoKind::Invalid, param));

            let (then_src, else_src) = ctx.make_cond(param_given, |ctx, param_given| {
                if param_given {
                    // Builds the if block for the case when param is given
                    if build_stores {
                        let exit = ctx.create_block();
                        let mut ctx = BodyLoweringCtx { ctx, body: body.borrow(), path: "" };
                        ctx.check_param(
                            param_val,
                            &bounds,
                            &[],
                            ConstraintKind::From,
                            ops,
                            invalid,
                            exit,
                        );
                        ctx.check_param(
                            param_val,
                            &bounds,
                            &[],
                            ConstraintKind::Exclude,
                            ops,
                            invalid,
                            exit,
                        );
                        ctx.ctx.switch_to_block(exit);
                    }
                    param_val
                } else {
                    // Builds the else block for the case when param is not given
                    let default_val = ctx.lower_expr_body(body.borrow(), 0);
                    if build_stores {
                        let exit = ctx.create_block();
                        let mut ctx = BodyLoweringCtx { ctx, body: body.borrow(), path: "" };
                        ctx.check_param(
                            default_val,
                            &bounds,
                            &[],
                            ConstraintKind::From,
                            ops,
                            invalid,
                            exit,
                        );
                        ctx.check_param(
                            default_val,
                            &bounds,
                            &[],
                            ConstraintKind::Exclude,
                            ops,
                            invalid,
                            exit,
                        );
                        ctx.ctx.switch_to_block(exit);
                        default_vals[i] = vec![ctx.ctx.ins().optbarrier(default_val)];
                    }
                    default_val
                }
            });

            // let last_inst = builder.func.layout.last_inst(else_src.0).unwrap();
            ctx.ins().with_result(new_val).phi(&[then_src, else_src]);

            // we purposfull insert these reversed here (new val into params and old val into
            // outputs). This ensures that the code generated for other parameters uses the
            // correct value. After code generation is complete we swap these two again
            ctx.def_param(ParamKind::Param(param), new_val);
            ctx.def_output(PlaceKind::Param(param), param_val);
            param_val = new_val;

            if !build_min_max {
                continue;
            }

            let invalid = ctx.dec_callback(CallBackKind::ParamInfo(ParamInfoKind::Invalid, param));

            let precomputed_vals = if build_min_max {
                let min_inclusive =
                    ctx.dec_callback(CallBackKind::ParamInfo(ParamInfoKind::MinInclusive, param));
                let max_inclusive =
                    ctx.dec_callback(CallBackKind::ParamInfo(ParamInfoKind::MaxInclusive, param));
                let min_exclusive =
                    ctx.dec_callback(CallBackKind::ParamInfo(ParamInfoKind::MinExclusive, param));
                let max_exclusive =
                    ctx.dec_callback(CallBackKind::ParamInfo(ParamInfoKind::MaxExclusive, param));

                let mut ctx = BodyLoweringCtx { ctx: &mut ctx, body: body.borrow(), path: "" };
                let mut lowered_bounds = None;
                let precomputed_vals = bounds
                    .iter()
                    .filter_map(|bound| {
                        if matches!(bound.kind, ConstraintKind::Exclude) {
                            return None;
                        }
                        let (val0, val1) = match bound.val {
                            ConstraintValue::Value(val) => {
                                let val = ctx.lower_expr(val);

                                if let Some((min, max)) = lowered_bounds {
                                    let is_min = ctx.ctx.ins().binary1(ops.le.unwrap(), val, min);
                                    let min = ctx.ctx.make_select(is_min, |builder, is_min| {
                                        if is_min {
                                            builder.ins().call(min_inclusive, &[]);
                                            val
                                        } else {
                                            min
                                        }
                                    });

                                    let is_max = ctx.ctx.ins().binary1(ops.le.unwrap(), max, val);
                                    let max = ctx.ctx.make_select(is_max, |builder, is_max| {
                                        if is_max {
                                            builder.ins().call(max_inclusive, &[]);
                                            val
                                        } else {
                                            min
                                        }
                                    });

                                    lowered_bounds = Some((min, max));
                                } else if ops.le.is_some() {
                                    lowered_bounds = Some((val, val));
                                    ctx.ctx.ins().call(min_inclusive, &[]);
                                    ctx.ctx.ins().call(max_inclusive, &[]);
                                }
                                (val, Value::reserved_value())
                            }
                            ConstraintValue::Range(range) => {
                                let start = ctx.lower_expr(range.start);
                                let end = ctx.lower_expr(range.end);

                                if let Some((min, max)) = lowered_bounds {
                                    let (op, call) = if range.start_inclusive {
                                        (ops.le.unwrap(), min_inclusive)
                                    } else {
                                        (ops.lt.unwrap(), min_exclusive)
                                    };

                                    let is_min = ctx.ctx.ins().binary1(op, start, min);
                                    let min = ctx.ctx.make_select(is_min, |builder, is_min| {
                                        if is_min {
                                            builder.ins().call(call, &[]);
                                            start
                                        } else {
                                            min
                                        }
                                    });

                                    let (op, call) = if range.end_inclusive {
                                        (ops.le.unwrap(), max_inclusive)
                                    } else {
                                        (ops.lt.unwrap(), max_exclusive)
                                    };

                                    let is_max = ctx.ctx.ins().binary1(op, max, end);
                                    let max = ctx.ctx.make_select(is_max, |builder, is_max| {
                                        if is_max {
                                            builder.ins().call(call, &[]);
                                            start
                                        } else {
                                            min
                                        }
                                    });

                                    lowered_bounds = Some((min, max));
                                } else {
                                    if range.start_inclusive {
                                        ctx.ctx.ins().call(min_inclusive, &[]);
                                    } else {
                                        ctx.ctx.ins().call(min_exclusive, &[]);
                                    }

                                    if range.end_inclusive {
                                        ctx.ctx.ins().call(max_inclusive, &[]);
                                    } else {
                                        ctx.ctx.ins().call(max_exclusive, &[]);
                                    }

                                    lowered_bounds = Some((start, end));
                                }

                                (start, end)
                            }
                        };

                        Some((val0, val1))
                    })
                    .collect();

                let (min, max) = lowered_bounds.unwrap_or_else(|| match ty.base_type() {
                    Type::Real => (f_neg_inf, f_inf),
                    Type::Integer => (i_neg_inf, i_inf),
                    _ => unreachable!(),
                });

                ctx.ctx.intern.outputs.insert(PlaceKind::ParamMin(param), min.into());
                ctx.ctx.intern.outputs.insert(PlaceKind::ParamMax(param), max.into());
                precomputed_vals
            } else {
                vec![]
            };

            // first from bounds (here we also get min/max from)
            let exit = ctx.create_block();
            let mut ctx = BodyLoweringCtx { ctx: &mut ctx, body: body.borrow(), path: "" };
            ctx.check_param(
                param_val,
                &bounds,
                &precomputed_vals,
                ConstraintKind::From,
                ops,
                invalid,
                exit,
            );
            ctx.check_param(
                param_val,
                &bounds,
                &precomputed_vals,
                ConstraintKind::Exclude,
                ops,
                invalid,
                exit,
            );
            ctx.ctx.switch_to_block(exit);
        }
        ctx.ensured_sealed();
        ctx.func.func.layout.append_inst_to_bb(term, ctx.current_block());

        for (i, param) in params.iter().copied().enumerate() {
            let len = array_len(db, param);
            for pos in 0..len.max(1) {
                let (kind, place) = if len == 0 {
                    (ParamKind::Param(param), PlaceKind::Param(param))
                } else {
                    (ParamKind::ParamElement(param, pos), PlaceKind::ParamElement(param, pos))
                };
                let val = &mut self.params.raw[&kind];
                let output_val = if build_stores { default_vals[i][pos as usize] } else { *val };
                *val =
                    replace(&mut self.outputs[&place], Some(output_val).into()).unwrap_unchecked();
            }
        }
    }

    /// Initialise an array parameter, element by element.
    ///
    /// VAMS-2023 3.4.4: a parameter array is overridden as a whole, and by an array
    /// of the exact declared size, so one `given` flag covers the array. What the
    /// model reads is one element at a time, so each element has an input of its
    /// own, takes its default from its own position in the assignment pattern, and
    /// is checked against the declared range, which applies to every element.
    ///
    /// Returns each element's default, in row-major order, for the store that
    /// reflects a defaulted parameter back to the simulator.
    fn init_array_param(
        ctx: &mut LoweringCtx<'_, '_>,
        db: &CompilationDB,
        param: Parameter,
        build_stores: bool,
    ) -> Vec<Value> {
        let ty = param.ty(db);
        let dims = ty.dim();
        let len: u32 = dims.iter().product();
        let elem_ty = ty.base_type().clone();
        let body = param.init(db);
        let bounds = param.bounds(db);
        let ops = CmpOps::from_ty(&ty);
        let default = body.borrow().get_entry_expr(0);
        let invalid = ctx.dec_callback(CallBackKind::ParamInfo(ParamInfoKind::Invalid, param));
        let mut defaults = Vec::with_capacity(len as usize);

        for pos in 0..len {
            let kind = ParamKind::ParamElement(param, pos);
            let param_val = ctx.use_param(kind);
            let param_given = ctx.use_param(ParamKind::ParamGiven { param });

            // Same dance as the scalar case: a temporary takes over the uses so
            // that the phi of the given value and the default can be the one every
            // other parameter sees.
            let new_val = ctx.func.make_param(0u32.into());
            ctx.dfg_mut().replace_uses(param_val, new_val);

            let (then_src, else_src) = ctx.make_cond(param_given, |ctx, param_given| {
                let mut ctx = BodyLoweringCtx { ctx, body: body.borrow(), path: "" };
                let val = if param_given {
                    param_val
                } else {
                    ctx.lower_array_elem_default(default, pos, &dims, &elem_ty)
                };
                if build_stores {
                    let exit = ctx.ctx.create_block();
                    ctx.check_param(val, &bounds, &[], ConstraintKind::From, ops, invalid, exit);
                    ctx.check_param(val, &bounds, &[], ConstraintKind::Exclude, ops, invalid, exit);
                    ctx.ctx.switch_to_block(exit);
                    if !param_given {
                        let barrier = ctx.ctx.ins().optbarrier(val);
                        defaults.push(barrier);
                    }
                }
                val
            });

            ctx.ins().with_result(new_val).phi(&[then_src, else_src]);
            ctx.def_param(kind, new_val);
            ctx.def_output(PlaceKind::ParamElement(param, pos), param_val);
        }

        defaults
    }
}

/// The number of elements of an array parameter, or 0 for a scalar.
fn array_len(db: &CompilationDB, param: Parameter) -> u32 {
    let dims = param.ty(db).dim();
    if dims.is_empty() {
        0
    } else {
        dims.iter().product()
    }
}

impl BodyLoweringCtx<'_, '_, '_> {
    #[allow(clippy::too_many_arguments)]
    fn check_param(
        &mut self,
        param_val: Value,
        bounds: &[ParamConstraint],
        precomputed_vals: &[(Value, Value)],
        kind: ConstraintKind,
        ops: CmpOps,
        invalid: FuncRef,
        global_exit: Block,
    ) {
        let mut exit = None;

        for (i, bound) in bounds.iter().enumerate() {
            if bound.kind != kind {
                continue;
            }

            let exit = match exit {
                Some(exit) => exit,
                None => {
                    let bb = self.ctx.create_block();
                    exit = Some(bb);
                    bb
                }
            };

            match bound.val {
                ConstraintValue::Value(val) => {
                    let val = precomputed_vals
                        .get(i)
                        .map_or_else(|| self.lower_expr(val), |(val, _)| *val);

                    let is_ok = self.ctx.ins().binary1(ops.eq, val, param_val);
                    let next_bb = self.ctx.create_block();
                    self.ctx.ins().br(is_ok, exit, next_bb);
                    self.ctx.switch_to_block(next_bb);
                }
                ConstraintValue::Range(range) => {
                    let (start, end) = precomputed_vals.get(i).map_or_else(
                        || (self.lower_expr(range.start), self.lower_expr(range.end)),
                        |(start, end)| (*start, *end),
                    );

                    let op = ops.in_bound(range.start_inclusive);
                    let is_lo_ok = self.ctx.ins().binary1(op, start, param_val);

                    let is_ok = self.ctx.make_select(is_lo_ok, |builder, is_ok| {
                        if is_ok {
                            let op = ops.in_bound(range.end_inclusive);
                            builder.ins().binary1(op, param_val, end)
                        } else {
                            FALSE
                        }
                    });

                    let next_bb = self.ctx.create_block();
                    self.ctx.ins().br(is_ok, exit, next_bb);
                    self.ctx.switch_to_block(next_bb);
                }
            }
        }

        match kind {
            ConstraintKind::From => {
                if let Some(exit) = exit {
                    // error on fallthrough
                    self.ctx.ins().call(invalid, &[]);
                    self.ctx.ins().jump(global_exit);

                    self.ctx.switch_to_block(exit);
                }
            }

            ConstraintKind::Exclude => {
                self.ctx.ins().jump(global_exit);

                if let Some(exit) = exit {
                    // error on fallthrough
                    self.ctx.switch_to_block(exit);
                    self.ctx.ins().call(invalid, &[]);
                    self.ctx.ins().jump(global_exit);
                }
            }
        }
    }
}
