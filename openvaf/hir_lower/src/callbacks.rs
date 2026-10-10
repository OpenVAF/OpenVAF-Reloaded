use hir::{Node, Parameter};
use lasso::Spur;
use mir::{FunctionSignature, Param};
use stdx::Ieee64;

use crate::fmt::{DisplayKind, FmtArg, PrintSink};
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

/// VAMS-2023 9.5: what to do to a file, once the descriptor naming it is in hand.
/// Each one is a function in `openvaf/osdi/stdlib.c`, which owns the table a
/// descriptor indexes.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum FileOp {
    /// `$fopen(name)`: a multichannel descriptor, one bit per file.
    OpenMcd,
    /// `$fopen(name, mode)`: a file descriptor, with the top bit set.
    Open,
    Close,
    Flush,
    /// `$fflush()` with no argument, which flushes everything open.
    FlushAll,
    Eof,
    Tell,
    Seek,
    Rewind,
    /// `$fgets`, which returns the line rather than writing through its argument.
    Gets,
    /// How long that line was, which is what `$fgets` itself returns. Not a file
    /// operation, but it belongs to the same runtime.
    Len,
    /// `$sscanf`: perform the conversions and return how many succeeded. The
    /// results wait in the runtime for the two below to read back, because a
    /// Verilog-A argument is not a pointer a runtime can write through.
    Scan,
    ScanReal,
    ScanStr,
}

impl FileOp {
    /// The name of the function in the stdlib that performs it.
    pub fn stdlib_name(self) -> &'static str {
        match self {
            FileOp::OpenMcd => "va_fopen_mcd",
            FileOp::Open => "va_fopen",
            FileOp::Close => "va_fclose",
            FileOp::Flush | FileOp::FlushAll => "va_fflush",
            FileOp::Eof => "va_feof",
            FileOp::Tell => "va_ftell",
            FileOp::Seek => "va_fseek",
            FileOp::Rewind => "va_rewind",
            FileOp::Gets => "va_fgets",
            FileOp::Len => "va_strlen",
            FileOp::Scan => "va_sscanf",
            FileOp::ScanReal => "va_scan_real",
            FileOp::ScanStr => "va_scan_str",
        }
    }

    /// How many arguments it is called with. `$fflush` takes the descriptor and a
    /// flag saying to ignore it, so that both of 9.5.6's forms are one function.
    pub fn num_args(self) -> u16 {
        match self {
            FileOp::OpenMcd => 1,
            FileOp::Open | FileOp::Flush | FileOp::FlushAll | FileOp::Scan => 2,
            FileOp::Close
            | FileOp::Eof
            | FileOp::Tell
            | FileOp::Rewind
            | FileOp::Gets
            | FileOp::Len
            | FileOp::ScanReal
            | FileOp::ScanStr => 1,
            FileOp::Seek => 3,
        }
    }
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum CallBackKind {
    /// 9.4 and 9.5 format alike and differ in where the line goes: `sink` says
    /// which, and for a file the first argument is the descriptor.
    Print { kind: DisplayKind, sink: PrintSink, arg_tys: Box<[FmtArg]> },
    File(FileOp),
    /// Whether this is the first evaluation at the current timepoint, which is what
    /// 9.4.1's "at the end of the current simulation time" needs in order not to
    /// mean "once per Newton iteration". Carries its own cell.
    RetainedFirst(RetainedState),
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
    /// The same slot, holding a string variable's pointer instead of a number.
    /// 4.5.10 makes no exception for strings, so neither does retention.
    StoreRetainedStr(RetainedState),
    /// What such a slot held at the end of the previous accepted timestep. A
    /// callback rather than a parameter because the value is a string, which the
    /// parameter machinery reads as a double.
    PrevRetainedStr(RetainedState),
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
            CallBackKind::Print { kind, sink, arg_tys: args } => FunctionSignature {
                name: match sink {
                    PrintSink::Log => format!("{:?})", kind),
                    PrintSink::File => format!("f{:?})", kind),
                    PrintSink::Str => "$sformat".to_owned(),
                },
                // The format string, plus the descriptor for the file forms.
                params: args.len() as u16 + 1 + u16::from(*sink == PrintSink::File),
                returns: u16::from(*sink == PrintSink::Str),
                // Pinned in the evaluation even for the string form, which is a pure
                // function of its arguments: left movable, the pass that hoists
                // op-independent work into instance setup would take it there and
                // leave the evaluation reading a value nothing computes.
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
            CallBackKind::File(op) => FunctionSignature {
                name: op.stdlib_name().to_owned(),
                params: op.num_args(),
                returns: 1,
                // Opening, closing and seeking a file are the point of calling them,
                // and the returned status is usually dropped.
                has_sideeffects: true,
            },
            CallBackKind::StoreRetainedStr(state) => FunctionSignature {
                name: format!("$store_retained_str[{state:?}]"),
                params: 1,
                returns: 1,
                // As for the numeric form: the stored value is read at the start of
                // the next timestep, not here, so without this the call would be
                // eliminated and the slot would never be written.
                has_sideeffects: true,
            },
            CallBackKind::PrevRetainedStr(state) => FunctionSignature {
                name: format!("$retained_prev_str[{state:?}]"),
                params: 0,
                returns: 1,
                // Reading takes no arguments, which would otherwise leave it free to
                // be hoisted into instance setup, where the slot it reads is not
                // addressable. Pinned here instead.
                has_sideeffects: true,
            },
            CallBackKind::RetainedFirst(state) => FunctionSignature {
                name: format!("$retained_first[{state:?}]"),
                params: 1,
                returns: 1,
                // It writes the cell it tests, so it is not a pure predicate.
                has_sideeffects: true,
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
                // Reading or writing a retained slot, asking whether this is the
                // first evaluation at a timepoint, and anything touching a file all
                // belong to the evaluation. Left op-independent, the pass that moves
                // such work into instance setup would move them there -- where the
                // slot a retained callback addresses is not addressable, and where a
                // line written to a file is written once for the instance instead of
                // once for the timepoint.
                | CallBackKind::StoreRetainedStr(_)
                | CallBackKind::PrevRetainedStr(_)
                | CallBackKind::RetainedFirst(_)
                | CallBackKind::File(_)
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
