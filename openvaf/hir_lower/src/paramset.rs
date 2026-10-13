//! Paramsets (VAMS-2023 6.4).
//!
//! A paramset collects parameter values for a module, the way a SPICE model card
//! does, and declares parameters of its own: what an instance may set and what it
//! may not are the paramset's, not the module's. "The paramset itself contains no
//! behavioural code; all of the behaviour is determined by the associated module."
//!
//! So a paramset is compiled as the module it names, with one substitution: where
//! the module reads a parameter of its own, the lowered body reads the paramset's
//! statement for it instead, or that parameter's declared default where the
//! paramset is silent. Nothing of the module's parameters is left to read from a
//! model card, which is what the clause asks for: "an instance of the paramset that
//! attempts to override any of the other parameters of the underlying module would
//! generate an error". The paramset's own parameters stay inputs, so they get the
//! model-card storage, the `given` flag and the range checks any parameter gets.
//!
//! The substitution happens on the finished body, the way a retained variable's
//! initial value does, so everything downstream (the derivatives, the split into
//! instance setup and evaluation, the descriptor) treats a paramset's arithmetic
//! exactly as it treats a module's.

use std::mem::replace;

use hir::{Body, CompilationDB, Parameter, Type};
use lasso::Rodeo;
use mir::{Function, Value, FALSE, TRUE};
use mir_build::{FunctionBuilder, FunctionBuilderContext};

use crate::body::BodyLoweringCtx;
use crate::ctx::LoweringCtx;
use crate::{HirInterner, ParamKind};

/// What a paramset puts in place of the target module's model card.
pub struct ParamsetInit<'a> {
    /// Every parameter of the target module, in declaration order. A default may
    /// read a parameter declared before it (6.3.4), so this is the order the
    /// substitution runs in.
    pub target_params: &'a [Parameter],
    /// The paramset's statements: the right-hand side of the i-th one is the i-th
    /// entry statement of this body.
    pub body: Body,
    /// A parameter of the target module and the statement that sets it. Every other
    /// target parameter keeps its declared default.
    pub assigns: &'a [(Parameter, usize)],
}

impl ParamsetInit<'_> {
    /// The statement that sets `param`, if the paramset has one.
    fn assign(&self, param: Parameter) -> Option<usize> {
        self.assigns.iter().find(|(target, _)| *target == param).map(|(_, stmt)| *stmt)
    }
}

impl HirInterner {
    /// Replace every read of a target module parameter with the paramset's value for
    /// it. Call this on the freshly lowered body, before anything optimizes it.
    pub fn insert_paramset_init(
        &mut self,
        db: &CompilationDB,
        func: &mut Function,
        literals: &mut Rodeo,
        init: &ParamsetInit<'_>,
    ) {
        let mut builder_ctx = FunctionBuilderContext::default();
        let (builder, term) = FunctionBuilder::edit(func, literals, &mut builder_ctx, false);
        let mut ctx = LoweringCtx::new(db, builder, true, self);

        // While the substitution runs, an input maps to the value that replaced it,
        // so that the default of a parameter declared later reads the right thing.
        // Afterwards every one of them is put back, dead, because the descriptor
        // reads the map to decide what storage an input comes from and a target
        // parameter has none.
        let mut inputs: Vec<(ParamKind, Value)> = Vec::new();

        for &param in init.target_params {
            let stmt = init.assign(param);
            let ty = param.ty(db);
            let dims = ty.dim();

            if dims.is_empty() {
                // A parameter the body never read is substituted too, because the
                // default of a parameter declared later may read it.
                let kind = ParamKind::Param(param);
                let input = ctx.use_param(kind);
                let val = match stmt {
                    Some(stmt) => paramset_value(&mut ctx, init, stmt, &ty),
                    None => ctx.lower_expr_body(param.init(db).borrow(), 0),
                };
                ctx.dfg_mut().replace_uses(input, val);
                ctx.def_param(kind, val);
                inputs.push((kind, input));
            } else {
                // An array parameter is substituted element by element, the way it
                // is initialised from a model card (3.4.4).
                let len: u32 = dims.iter().product();
                let elem_ty = ty.base_type().clone();
                let default = param.init(db);
                for pos in 0..len {
                    let kind = ParamKind::ParamElement(param, pos);
                    let input = ctx.use_param(kind);
                    let val = match stmt {
                        Some(stmt) => {
                            let body = init.body.borrow();
                            let expr = body.get_entry_expr(stmt);
                            let mut ctx = BodyLoweringCtx { ctx: &mut ctx, body, path: "" };
                            ctx.lower_array_elem_default(expr, pos, &dims, &elem_ty)
                        }
                        None => {
                            let body = default.borrow();
                            let expr = body.get_entry_expr(0);
                            let mut ctx = BodyLoweringCtx { ctx: &mut ctx, body, path: "" };
                            ctx.lower_array_elem_default(expr, pos, &dims, &elem_ty)
                        }
                    };
                    ctx.dfg_mut().replace_uses(input, val);
                    ctx.def_param(kind, val);
                    inputs.push((kind, input));
                }
            }

            // `$param_given` (6.3.5) of a parameter the paramset sets is true, and
            // of one it is silent about is false. Both are constants: a paramset
            // assignment is the override, and nothing else can reach the module's
            // parameters.
            let kind = ParamKind::ParamGiven { param };
            let input = ctx.use_param(kind);
            let given = if stmt.is_some() { TRUE } else { FALSE };
            ctx.dfg_mut().replace_uses(input, given);
            ctx.def_param(kind, given);
            inputs.push((kind, input));
        }

        ctx.ensured_sealed();
        ctx.func.func.layout.append_inst_to_bb(term, ctx.current_block());

        for (kind, input) in inputs {
            replace(&mut self.params.raw[&kind], input);
        }
    }
}

/// One paramset statement's value, converted to the parameter's type the way an
/// assignment to it would be.
fn paramset_value(
    ctx: &mut LoweringCtx<'_, '_>,
    init: &ParamsetInit<'_>,
    stmt: usize,
    dst: &Type,
) -> Value {
    let body = init.body.borrow();
    let expr = body.get_entry_expr(stmt);
    let src = BodyLoweringCtx { ctx, body, path: "" }.resolved_ty(expr);
    let val = ctx.lower_expr_body(body, stmt);
    if &src == dst {
        val
    } else {
        ctx.insert_cast(val, &src, dst)
    }
}
