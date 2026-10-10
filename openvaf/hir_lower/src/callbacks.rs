use hir::{Node, Parameter};
use lasso::Spur;
use mir::{FunctionSignature, Param};
use stdx::Ieee64;

use crate::fmt::{DisplayKind, FmtArg};
use crate::{LimitState, RetainedState};

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum ParamInfoKind {
    Invalid,
    MinInclusive,
    MaxInclusive,
    MinExclusive,
    MaxExclusive,
}

/// The distributions of VAMS-2023 9.13, as the stdlib's `rng_value` / `rng_seed`
/// select between them. The discriminants are the ABI.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum RngDist {
    /// An inclusive integer draw over `[a, b]`, which is also what `$random` is:
    /// a draw over the whole 32-bit range.
    UniformInt = 0,
    Uniform = 1,
    Normal = 2,
    Exponential = 3,
    Poisson = 4,
    ChiSquare = 5,
    T = 6,
    Erlang = 7,
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum RetFlag {
    Abort,
    Finish,
    Stop,
    Limited,
}

impl std::fmt::Display for RetFlag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let txt = match self {
            Self::Abort => "abort",
            Self::Finish => "finish",
            Self::Stop => "stop",
            Self::Limited => "limited",
        };
        write!(f, "{}", txt)
    }
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum CallBackKind {
    Print { kind: DisplayKind, arg_tys: Box<[FmtArg]> },
    SimParam,
    SimParamOpt,
    SimParamStr,
    Derivative(Param),
    NodeDerivative(Node),
    ParamInfo(ParamInfoKind, Parameter),
    CollapseHint(Node, Option<Node>),
    LimDiscontinuity,
    Analysis,
    BuiltinLimit { name: Spur, num_args: u32 },
    StoreLimit(LimitState),
    StoreRetained(RetainedState),
    /// The value drawn from a distribution (9.13), given `(seed, a, b)`.
    RngValue(RngDist),
    /// Where that same draw left the seed. Pure, like the value, so the pair can be
    /// asked for separately without an out-parameter in the ABI.
    RngSeed(RngDist),
    TimeDerivative,
    WhiteNoise { name: Spur, idx: u32 },
    FlickerNoise { name: Spur, idx: u32 },
    NoiseTable(Box<NoiseTable>),
    SetRetFlag(RetFlag),
}

impl CallBackKind {
    pub fn signature(&self) -> FunctionSignature {
        match self {
            CallBackKind::SimParam => FunctionSignature {
                name: "simparam".to_owned(),
                params: 1,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::SimParamOpt => FunctionSignature {
                name: "simparam_opt".to_owned(),
                params: 2,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::SimParamStr => FunctionSignature {
                name: "simparam_str".to_owned(),
                params: 1,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::Derivative(param) => FunctionSignature {
                name: format!("ddx_{}", param),
                params: 1,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::NodeDerivative(node) => FunctionSignature {
                name: format!("ddx_node_{:?}", node),
                params: 1,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::ParamInfo(kind, param) => FunctionSignature {
                name: format!("set_{:?}({:?})", kind, param),
                params: 0,
                returns: 0,
                has_sideeffects: true,
            },
            CallBackKind::CollapseHint(hi, lo) => FunctionSignature {
                name: format!("collapse_{:?}_{:?}", hi, lo),
                params: 0,
                returns: 0,
                has_sideeffects: true,
            },
            CallBackKind::Print { kind, arg_tys: args } => FunctionSignature {
                name: format!("{:?})", kind),
                params: args.len() as u16 + 1,
                returns: 0,
                has_sideeffects: true,
            },
            CallBackKind::BuiltinLimit { name, num_args } => FunctionSignature {
                name: format!("$limit[{name:?}]"),
                params: *num_args as u16,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::StoreLimit(state) => FunctionSignature {
                name: format!("$store[{state:?}]"),
                params: 1,
                returns: 1,
                // Writing `next_state` is a side effect. The limit path always uses
                // the returned value too, so this is only belt and braces there.
                has_sideeffects: true,
            },
            CallBackKind::RngValue(dist) => FunctionSignature {
                name: format!("$rng_value[{dist:?}]"),
                params: 3,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::RngSeed(dist) => FunctionSignature {
                name: format!("$rng_seed[{dist:?}]"),
                params: 3,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::StoreRetained(state) => FunctionSignature {
                name: format!("$store_retained[{state:?}]"),
                params: 1,
                returns: 1,
                // The stored value is usually *not* used afterwards (a latch is read
                // at the start of the next timestep, not here), so without this the
                // call would be eliminated and the slot would never be written.
                has_sideeffects: true,
            },
            CallBackKind::LimDiscontinuity => FunctionSignature {
                name: "$discontinuty[-1]".to_owned(),
                params: 0,
                returns: 0,
                has_sideeffects: true,
            },
            CallBackKind::Analysis => FunctionSignature {
                name: "analysis".to_owned(),
                params: 1,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::TimeDerivative => FunctionSignature {
                name: "ddt".to_string(),
                params: 1,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::WhiteNoise { name, .. } => FunctionSignature {
                name: format!("white_noise({name:?})"),
                params: 1,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::FlickerNoise { name, .. } => FunctionSignature {
                name: format!("flickr_noise({name:?})"),
                params: 2,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::NoiseTable(table) => FunctionSignature {
                name: format!(
                    "table_noise{}({:?}, {:?})",
                    if table.log { "lob" } else { "" },
                    table.name,
                    &table.vals
                ),
                params: 1,
                returns: 1,
                has_sideeffects: false,
            },
            CallBackKind::SetRetFlag(flag) => FunctionSignature {
                name: format!("SetRetFlag[{}]", flag),
                params: 0,
                returns: 0,
                has_sideeffects: true,
            },
        }
    }
    pub fn is_noise(&self) -> bool {
        matches!(
            self,
            CallBackKind::WhiteNoise { .. }
                | CallBackKind::FlickerNoise { .. }
                | CallBackKind::NoiseTable(_)
        )
    }

    pub fn op_dependent(&self) -> bool {
        matches!(
            self,
            CallBackKind::SimParam
                | CallBackKind::SimParamOpt
                | CallBackKind::StoreLimit(_)
                | CallBackKind::StoreRetained(_)
                | CallBackKind::RngValue(_)
                | CallBackKind::RngSeed(_)
                | CallBackKind::Analysis
                | CallBackKind::SimParamStr
                | CallBackKind::LimDiscontinuity
                | CallBackKind::BuiltinLimit { .. }
        )
    }

    pub fn ignore_if_op_dependent(&self) -> bool {
        matches!(self, CallBackKind::CollapseHint(_, _))
    }

    pub fn tracked(&self) -> bool {
        !matches!(self, CallBackKind::Print { .. })
    }
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct NoiseTable {
    pub name: Spur,
    pub log: bool,
    pub vals: Box<[(Ieee64, Ieee64)]>,
    idx: u32,
}

impl NoiseTable {
    // TODO: read from disk
    pub fn new(
        vals: impl IntoIterator<Item = (f64, f64)>,
        log: bool,
        name: Spur,
        idx: u32,
    ) -> Self {
        let mut vals: Vec<(Ieee64, Ieee64)> = if log {
            vals.into_iter().map(|(f, pwr)| (f.into(), pwr.into())).collect()
        } else {
            vals.into_iter().map(|(f, pwr)| (f.log10().into(), pwr.into())).collect()
        };
        vals.sort_unstable_by(|(f1, _), (f2, _)| f1.partial_cmp(f2).unwrap());
        vals.dedup_by_key(|(f, _)| *f);
        Self { name, log, vals: vals.into_boxed_slice(), idx }
    }
}
