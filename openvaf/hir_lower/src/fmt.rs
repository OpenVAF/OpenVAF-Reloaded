use hir::{ExprId, Literal, Type};
use mir::GRAVESTONE;

use crate::body::BodyLoweringCtx;
use crate::callbacks::CallBackKind;

#[derive(Debug, Clone, Hash, Eq, PartialEq, Copy)]
pub enum DisplayKind {
    Debug,
    Display,
    Info,
    Warn,
    Error,
    Fatal,
    Monitor,
}

/// Where a formatted line goes. 9.4's tasks write to the simulator's log, 9.5.2's
/// to whatever a descriptor names, and 9.5.3's into a string variable: one
/// formatter, three destinations.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Copy)]
pub enum PrintSink {
    Log,
    File,
    Str,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum FmtArgKind {
    Binary,
    EngineerReal,
    Other,
}

impl From<Type> for FmtArg {
    fn from(ty: Type) -> FmtArg {
        FmtArg { ty, kind: FmtArgKind::Other }
    }
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct FmtArg {
    pub ty: Type,
    pub kind: FmtArgKind,
}

impl BodyLoweringCtx<'_, '_, '_> {
    pub fn ins_display(&mut self, kind: DisplayKind, newline: bool, args: &[ExprId]) {
        self.ins_fmt(kind, newline, args, None, PrintSink::Log);
    }

    /// VAMS-2023 9.5.2: the same task, writing to whatever `desc` names. The
    /// descriptor rides in front of the format string, so one callback serves both
    /// forms and the formatting below does not have to know which it is.
    pub fn ins_display_file(
        &mut self,
        kind: DisplayKind,
        newline: bool,
        desc: mir::Value,
        args: &[ExprId],
    ) {
        self.ins_fmt(kind, newline, args, Some(desc), PrintSink::File);
    }

    /// VAMS-2023 9.5.3: `$swrite` and `$sformat` format into a string instead of
    /// writing it anywhere, so this one has a value to return.
    pub fn ins_sformat(&mut self, args: &[ExprId]) -> mir::Value {
        // Neither appends a newline: 9.5.3 gives them `$write`'s behaviour, not
        // `$display`'s.
        self.ins_fmt(DisplayKind::Display, false, args, None, PrintSink::Str)
            .expect("the string sink returns the formatted string")
    }

    fn ins_fmt(
        &mut self,
        kind: DisplayKind,
        newline: bool,
        args: &[ExprId],
        desc: Option<mir::Value>,
        sink: PrintSink,
    ) -> Option<mir::Value> {
        let mut fmt_lit = String::new();
        let mut call_args = match desc {
            Some(desc) => vec![desc, GRAVESTONE],
            None => vec![GRAVESTONE],
        };
        let fmt_idx = call_args.len() - 1;
        let mut arg_tys = Vec::new();

        let mut i = 0;

        while let Some(&expr) = args.get(i) {
            i += 1;
            // For $fatal skip the first argument
            if i == 1 && kind == DisplayKind::Fatal {
                continue;
            }
            if let Some(Literal::String(ref lit)) = self.body.as_literal(expr) {
                fmt_lit.reserve(lit.len());
                let mut chars = lit.chars();
                while let Some(mut c) = chars.next() {
                    if c == '%' {
                        c = chars.next().unwrap();
                        let ty = match c {
                            '%' => {
                                fmt_lit.push_str("%%");
                                continue;
                            }
                            'm' | 'M' => {
                                fmt_lit.push_str(self.path);
                                continue;
                            }
                            'l' | 'L' => {
                                // TODO support properly
                                fmt_lit.push_str("__.__");
                                continue;
                            }
                            'h' => {
                                fmt_lit.push_str("%x");
                                Type::Integer.into()
                            }
                            'H' => {
                                fmt_lit.push_str("%X");
                                Type::Integer.into()
                            }
                            'b' | 'B' => {
                                fmt_lit.push_str("%s");
                                FmtArg { ty: Type::Integer, kind: FmtArgKind::Binary }
                            }
                            'd' | 'D' => {
                                fmt_lit.push_str("%d");
                                Type::Integer.into()
                            }
                            'o' | 'O' => {
                                fmt_lit.push_str("%o");
                                Type::Integer.into()
                            }
                            'c' | 'C' => {
                                fmt_lit.push_str("%c");
                                Type::Integer.into()
                            }
                            's' | 'S' => {
                                fmt_lit.push_str("%s");
                                Type::String.into()
                            }
                            _ => {
                                fmt_lit.push('%');
                                // real fmt specifiers may contain cmplx prefixes
                                // we validatet these
                                while !matches!(c, 'e'..='g'|'E'..='G'|'r'|'R') {
                                    if c == '*' {
                                        arg_tys.push(Type::Integer.into());
                                        call_args.push(self.lower_expr(args[i]));
                                        i += 1;
                                    }
                                    fmt_lit.push(c);
                                    c = chars.next().unwrap()
                                }
                                let kind = if matches!(c, 'r' | 'R') {
                                    fmt_lit.push_str("f%c");
                                    FmtArgKind::EngineerReal
                                } else {
                                    fmt_lit.push(c);
                                    FmtArgKind::Other
                                };

                                FmtArg { ty: Type::Real, kind }
                            }
                        };

                        arg_tys.push(ty);
                        call_args.push(self.lower_expr(args[i]));
                        i += 1;
                    } else {
                        fmt_lit.push(c)
                    }
                }
            } else {
                let ty = self.resolved_ty(expr);
                let has_whitespace = fmt_lit.chars().last().map_or(false, |c| c.is_whitespace());
                if !has_whitespace {
                    fmt_lit.push(' ')
                }
                match ty {
                    Type::Real => fmt_lit.push_str("%g"),
                    Type::Integer => fmt_lit.push_str("%d"),
                    Type::String => fmt_lit.push_str("%s"),
                    Type::Void => {
                        fmt_lit.push(' ');
                        continue;
                    }
                    _ => unreachable!(),
                }

                arg_tys.push(ty.into());
                call_args.push(self.lower_expr(expr));
            }
        }
        if newline {
            fmt_lit.push('\n');
        }

        call_args[fmt_idx] = self.ctx.sconst(&fmt_lit);
        let cb = CallBackKind::Print { kind, sink, arg_tys: arg_tys.into_boxed_slice() };
        match sink {
            PrintSink::Str => Some(self.ctx.call1(cb, &call_args)),
            _ => {
                self.ctx.call(cb, &call_args);
                None
            }
        }
    }
}
