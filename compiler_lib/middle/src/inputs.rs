/*
 *  ******************************************************************************************
 *  Copyright (c) 2021 Pascal Kuthe. This file is part of the frontend project.
 *  It is subject to the license terms in the LICENSE file found in the top-level directory
 *  of this distribution and at  https://gitlab.com/DSPOM/OpenVAF/blob/master/LICENSE.
 *  No part of frontend, including this file, may be copied, modified, propagated, or
 *  distributed except according to the terms contained in the LICENSE file.
 *  *****************************************************************************************
 */

use crate::osdi_types::ConstVal::Scalar;
use crate::osdi_types::SimpleConstVal::Real;
use crate::{
    BranchId, CfgFunctions, ConstVal, Derivative, Mir, OperandData, ParameterId, PortId, Type,
};
use derive_more::Display;
use enum_dispatch::enum_dispatch;
use openvaf_ir::ids::{CallArg, NodeId};
use openvaf_ir::Unknown;
use openvaf_session::{
    sourcemap::StringLiteral,
    symbols::{kw, sysfun},
};
use std::convert::TryInto;
use std::fmt::{Debug, Display};

#[enum_dispatch]
pub trait CfgInputs: Clone + Sized + Debug + PartialEq + Display {
    fn derivative<C: CfgFunctions>(&self, unknown: Unknown, mir: &Mir<C>) -> InputDerivative;

    fn ty<C: CfgFunctions>(&self, mir: &Mir<C>) -> Type;
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputDerivative {
    One,
    Zero,
    Const(ConstVal),
}

impl InputDerivative {
    pub fn into_option(self) -> Option<ConstVal> {
        match self {
            InputDerivative::One => Some(Scalar(Real(1.0))),
            InputDerivative::Zero => None,
            InputDerivative::Const(val) => Some(val),
        }
    }
}

#[enum_dispatch(CfgInputs)]
#[derive(Clone, Debug, PartialEq, Display)]
/// This struct is generated by HIR lowering and represents all input kinds which are explicitly found in the VerilogA code
/// OpenVAF drivers can easily create their own variant of this enum by combining variants with `#[enum_dispatch(CfgInputs)]`
pub enum LimFunctionInput {
    Parameter(ParameterInput),
    PortConnected,
    SimParam,
    FunctionArg(LimFunctionArg),
    Temperature
}

#[derive(Clone, Debug, PartialEq, Display)]
#[display(fmt = "{}", "arg")]
pub struct LimFunctionArg {
    pub arg: CallArg,
    pub ty: Type,
}

impl CfgInputs for LimFunctionArg {
    fn derivative<C: CfgFunctions>(&self, _unknown: Unknown, _mir: &Mir<C>) -> InputDerivative {
        unimplemented!()
    }

    fn ty<C: CfgFunctions>(&self, _mir: &Mir<C>) -> Type {
        self.ty
    }
}

#[enum_dispatch(CfgInputs)]
#[derive(Clone, Debug, PartialEq, Display)]
/// This struct is generated by HIR lowering and represents all input kinds which are explicitly found in the VerilogA code
/// OpenVAF drivers can easily create their own variant of this enum by combining variants with `#[enum_dispatch(CfgInputs)]`
pub enum DefaultInputs {
    Parameter(ParameterInput),
    PortConnected,
    SimParam,
    Voltage,
    CurrentProbe,
    Temperature,
    PartialTimeDerivative,
}

#[derive(Copy, Clone, Debug, PartialEq, Display)]
pub enum NoInput {}

impl CfgInputs for NoInput {
    fn derivative<C: CfgFunctions>(&self, _unknown: Unknown, _mir: &Mir<C>) -> InputDerivative {
        match *self {}
    }

    fn ty<C: CfgFunctions>(&self, _mir: &Mir<C>) -> Type {
        unreachable!("This cfg has no input")
    }
}

impl<I: CfgInputs> From<InputDerivative> for Derivative<I> {
    fn from(from: InputDerivative) -> Self {
        match from {
            InputDerivative::One => Self::One,
            InputDerivative::Zero => Self::Zero,
            InputDerivative::Const(val) => Self::Operand(OperandData::Constant(val)),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Display)]
pub enum SimParamKind {
    #[display(fmt = "get_val,t y=real,opt=false")]
    Real,
    #[display(fmt = "get_val, ty=real,opt=true")]
    RealOptional,
    #[display(fmt = "is_val_given")]
    RealOptionalGiven,
    #[display(fmt = "get_val, ty=string, opt=false")]
    String,
}

#[derive(Copy, Clone, Debug, PartialEq, Display)]
#[display(fmt = "{}({}, {})", "sysfun::simparam", "self.name", "self.kind")]
pub struct SimParam {
    pub name: StringLiteral,
    pub kind: SimParamKind,
}

impl CfgInputs for SimParam {
    #[inline(always)]
    fn derivative<C: CfgFunctions>(&self, _unknown: Unknown, _mir: &Mir<C>) -> InputDerivative {
        InputDerivative::Zero
    }

    fn ty<C: CfgFunctions>(&self, _mir: &Mir<C>) -> Type {
        match self.kind {
            SimParamKind::String => Type::STRING,
            SimParamKind::Real => Type::REAL,
            SimParamKind::RealOptional => Type::REAL,
            SimParamKind::RealOptionalGiven => Type::BOOL,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Display)]
#[display(fmt = "{}", "sysfun::temperature")]
pub struct Temperature;

impl CfgInputs for Temperature {
    #[inline]
    fn derivative<C: CfgFunctions>(&self, unknown: Unknown, _mir: &Mir<C>) -> InputDerivative {
        if unknown == Unknown::Temperature {
            InputDerivative::One
        } else {
            InputDerivative::Zero
        }
    }

    #[inline(always)]
    fn ty<C: CfgFunctions>(&self, _mir: &Mir<C>) -> Type {
        Type::REAL
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Display)]
#[display(fmt = "{}({}(V(x)), V(x))", "kw::ddx", "kw::ddt")]
pub struct PartialTimeDerivative;

impl CfgInputs for PartialTimeDerivative {
    #[inline]
    fn derivative<C: CfgFunctions>(&self, _unknown: Unknown, _mir: &Mir<C>) -> InputDerivative {
        InputDerivative::Zero
    }

    #[inline(always)]
    fn ty<C: CfgFunctions>(&self, _mir: &Mir<C>) -> Type {
        Type::REAL
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Display)]
#[display(fmt = "{}({})", "sysfun::port_connected", "0")]
pub struct PortConnected(pub PortId);

impl CfgInputs for PortConnected {
    #[inline]
    fn derivative<C: CfgFunctions>(&self, _unknown: Unknown, _mir: &Mir<C>) -> InputDerivative {
        unreachable!()
    }

    #[inline(always)]
    fn ty<C: CfgFunctions>(&self, _mir: &Mir<C>) -> Type {
        Type::BOOL
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Display)]
#[display(fmt = "{}({},{})", "kw::potential", "lo", "hi")]
pub struct Voltage {
    pub hi: NodeId,
    pub lo: NodeId,
}

impl CfgInputs for Voltage {
    #[inline]
    fn derivative<C: CfgFunctions>(&self, unknown: Unknown, _mir: &Mir<C>) -> InputDerivative {
        match unknown {
            Unknown::NodePotential(node) if self.hi == node => InputDerivative::One,

            Unknown::NodePotential(node) if self.lo == node => {
                InputDerivative::Const(Scalar(Real(-1.0)))
            }

            Unknown::BranchPotential(hi_demanded, lo_demanded)
                if self.hi == hi_demanded && self.lo == lo_demanded =>
            {
                InputDerivative::One
            }

            Unknown::BranchPotential(hi_demanded, lo_demanded)
                if self.lo == hi_demanded && self.hi == lo_demanded =>
            {
                InputDerivative::Const(Scalar(Real(-1.0)))
            }
            _ => InputDerivative::Zero,
        }
    }

    #[inline(always)]
    fn ty<C: CfgFunctions>(&self, _mir: &Mir<C>) -> Type {
        Type::REAL
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Display)]
#[display(fmt = "{}({})", "kw::flow", "0")]
pub struct CurrentProbe(pub BranchId);

impl CfgInputs for CurrentProbe {
    #[inline]
    fn derivative<C: CfgFunctions>(&self, unknown: Unknown, _mir: &Mir<C>) -> InputDerivative {
        if matches!(unknown, Unknown::Flow(branch) if branch == self.0) {
            InputDerivative::One
        } else {
            InputDerivative::Zero
        }
    }

    #[inline(always)]
    fn ty<C: CfgFunctions>(&self, _mir: &Mir<C>) -> Type {
        Type::REAL
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Display)]
pub enum ParameterInput {
    Value(ParameterId),
    #[display(fmt = "{}({})", "sysfun::param_given", "0")]
    Given(ParameterId),
}

impl CfgInputs for ParameterInput {
    fn derivative<C: CfgFunctions>(&self, unknown: Unknown, _mir: &Mir<C>) -> InputDerivative {
        if matches!((unknown, self), (Unknown::Parameter(x), Self::Value(y)) if &x == y) {
            InputDerivative::One
        } else {
            InputDerivative::Zero
        }
    }

    fn ty<C: CfgFunctions>(&self, mir: &Mir<C>) -> Type {
        match self {
            Self::Value(param) => mir[*param].ty,
            Self::Given(_) => Type::BOOL,
        }
    }
}

impl From<NoInput> for DefaultInputs {
    fn from(src: NoInput) -> Self {
        match src {}
    }
}

impl TryInto<NoInput> for DefaultInputs {
    type Error = ();

    fn try_into(self) -> Result<NoInput, Self::Error> {
        Err(())
    }
}