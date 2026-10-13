/*
 *  ******************************************************************************************
 *  Copyright (c) 2021 Pascal Kuthe. This file is part of the frontend project.
 *  It is subject to the license terms in the LICENSE file found in the top-level directory
 *  of this distribution and at  https://gitlab.com/DSPOM/OpenVAF/blob/master/LICENSE.
 *  No part of frontend, including this file, may be copied, modified, propagated, or
 *  distributed except according to the terms contained in the LICENSE file.
 *  *****************************************************************************************
 */

use std::sync::Arc;

use text_size::TextRange;
use tokens::{literal, KeywordSet};
// use tracing::{debug, trace, trace_span};
use typed_index_collections::TiVec;

use crate::diagnostics::PreprocessorDiagnostic::{self, UnexpectedEof};
use crate::parser::{CompilerDirective, FullTokenIdx, Parser, PreprocessorToken};
use crate::processor::{Macro, MacroArg, MacroCall, ParsedToken, ParsedTokenKind, Processor};
use crate::sourcemap::{CtxSpan, SourceMap};
use crate::Diagnostics;

pub(crate) fn parse_condition<'a>(
    p: &mut Parser<'a, '_>,
    err: &mut Diagnostics,
    processor: &mut Processor<'a>,
    inverted: bool,
) {
    // let tspan = trace_span!("parsing macro condition");
    // let _tspan = tspan.enter();

    let name = p.current_text();

    if p.expect(PreprocessorToken::SimpleIdent, "an identifier", err) {
        // trace!(name = name, "condition");
        if processor.is_macro_defined(name) != inverted {
            parse_if_body::<true, false>(p, err, processor); // condition is true
        } else {
            parse_if_body::<false, true>(p, err, processor); // condition is false. find an else or endif
        }
    } else {
        // Just try to skip to the end of the if
        parse_if_body::<false, false>(p, err, processor)
    }
}

fn parse_if_body<'a, const PROCESS: bool, const CONSIDER_ELSE: bool>(
    p: &mut Parser<'a, '_>,
    err: &mut Diagnostics,
    processor: &mut Processor<'a>,
) {
    let mut depth = 0;
    loop {
        match p.current() {
            PreprocessorToken::CompilerDirective => match p.compiler_directive() {
                CompilerDirective::IfDef | CompilerDirective::IfNotDef if !PROCESS => depth += 1,

                CompilerDirective::EndIf if depth == 0 => {
                    p.bump();
                    break;
                }

                CompilerDirective::Else | CompilerDirective::ElseIf if PROCESS && depth == 0 => {
                    parse_if_body::<false, false>(p, err, processor);
                    return;
                }

                CompilerDirective::Else if CONSIDER_ELSE && depth == 0 => {
                    p.bump();
                    parse_if_body::<true, false>(p, err, processor);
                    break;
                }

                CompilerDirective::ElseIf if CONSIDER_ELSE && depth == 0 => {
                    p.bump();
                    parse_condition(p, err, processor, false);
                    break;
                }

                CompilerDirective::EndIf => depth -= 1,
                _ => (),
            },
            PreprocessorToken::Eof => {
                err.push(UnexpectedEof { expected: "`endif", span: p.current_span() });
                break;
            }
            _ => (),
        }

        if PROCESS {
            processor.process_token(p, err)
        } else {
            // ignore tokens
            p.bump()
        }
    }
}

pub(crate) fn parse_include<'a>(
    p: &mut Parser<'a, '_>,
    err: &mut Diagnostics,
) -> Option<(&'a str, TextRange)> {
    // let tspan = trace_span!("parsing `include");
    // let _tspan = tspan.enter();

    let start = p.current_range().start();
    p.bump();
    let path = p.current_text();
    if p.expect(PreprocessorToken::StrLit, "a string literal", err) {
        Some((&path[1..path.len() - 1], TextRange::new(start, p.previous_range().end())))
    } else {
        None
    }
}

/// Parses `` `begin_keywords "<version_specifier>" `` (VAMS-2023 10.6).
///
/// Returns the selected keyword set together with the span of the whole
/// directive. `None` is returned (and a diagnostic emitted) if the specifier is
/// missing or is not one of the specifiers the standard defines; the caller
/// keeps the currently active set in that case.
pub(crate) fn parse_begin_keywords(
    p: &mut Parser<'_, '_>,
    err: &mut Diagnostics,
) -> Option<(KeywordSet, CtxSpan)> {
    let start = p.current_range().start();
    p.bump();

    let specifier = p.current_text();
    if !p.expect(PreprocessorToken::StrLit, "a version specifier", err) {
        return None;
    }

    let range = TextRange::new(start, p.previous_range().end());
    let span = CtxSpan { ctx: p.ctx(), range };
    // strip the surrounding quotes
    let specifier = &specifier[1..specifier.len() - 1];

    match KeywordSet::from_version_specifier(specifier) {
        Some(set) => Some((set, span)),
        None => {
            err.push(PreprocessorDiagnostic::UnknownKeywordVersion {
                version: specifier.to_owned(),
                span,
            });
            None
        }
    }
}

/// Parses `` `default_transition <transition_time> `` (VAMS-2023 10.3).
///
/// Returns the time together with the span of the whole directive. The clause
/// writes `transition_time ::= constant_expression`, but a directive is read
/// before anything is in scope to make an expression out of, so what is accepted
/// is a numeric literal, with its scale factor character (`1n`) because that is
/// how a model writer spells a transition time. Anything else is reported rather
/// than quietly left at the default it was meant to replace.
pub(crate) fn parse_default_transition(
    p: &mut Parser<'_, '_>,
    err: &mut Diagnostics,
) -> Option<(f64, CtxSpan)> {
    let start = p.current_range().start();
    p.bump();

    let span = |p: &Parser<'_, '_>| CtxSpan {
        ctx: p.ctx(),
        range: TextRange::new(start, p.previous_range().end()),
    };

    // A sign is a token of its own, and is read here only so that a negative time
    // gets the diagnostic it deserves rather than "expected a transition time".
    let negated = p.on_current_line() && p.current_text() == "-";
    if negated {
        p.bump();
    }

    // The argument is on the same line as the directive, like `` `define ``'s
    // macro text: without that rule the next line's first token is eaten.
    let time = if p.on_current_line() {
        p.current_syntax_kind().and_then(|kind| literal::number_value(kind, p.current_text()))
    } else {
        None
    };

    let Some(time) = time else {
        // Reported against the directive, not against whatever follows it: what
        // follows may be the next line, which is nothing to do with it.
        err.push(PreprocessorDiagnostic::MissingTransitionTime { span: span(p) });
        return None;
    };
    p.bump();

    if negated && time != 0.0 {
        // 4.5.8: `rise_time` and `fall_time` "shall be non-negative".
        err.push(PreprocessorDiagnostic::NegativeTransitionTime { span: span(p) });
        return None;
    }

    Some((time, span(p)))
}

/// Parses `` `default_discipline [discipline_identifier [ qualifier ] ] ``
/// (VAMS-2023 10.2).
///
/// `None` means the directive named no discipline, which is the form that removes
/// the default: "if this directive is used without a discipline name, discipline
/// resolution will not use a default discipline for nets declared after this
/// directive". The qualifier it removes is returned with it.
pub(crate) fn parse_default_discipline(
    p: &mut Parser<'_, '_>,
    err: &mut Diagnostics,
) -> (Option<Arc<str>>, Option<Arc<str>>, CtxSpan) {
    let start = p.current_range().start();
    p.bump();

    // Both arguments are optional and the directive is not terminated, so they
    // are the identifiers that follow it on its own line.
    let mut name = None;
    let mut qualifier = None;
    if p.on_current_line() && p.at(PreprocessorToken::SimpleIdent) {
        name = Some(Arc::from(p.current_text()));
        p.bump();
        if p.on_current_line() && p.at(PreprocessorToken::SimpleIdent) {
            qualifier = Some((Arc::from(p.current_text()), p.current_span()));
            p.bump();
        }
    }

    let span = CtxSpan { ctx: p.ctx(), range: TextRange::new(start, p.previous_range().end()) };

    let qualifier = qualifier.and_then(|(qualifier, qspan): (Arc<str>, CtxSpan)| {
        if !NET_TYPE_QUALIFIERS.contains(&&*qualifier) {
            err.push(PreprocessorDiagnostic::UnknownDisciplineQualifier {
                qualifier: qualifier.to_string(),
                span: qspan,
            });
            return None;
        }
        // Every qualifier but `wire` names a net type Verilog-A has no nets of,
        // so the directive would be in force and apply to nothing.
        if &*qualifier != "wire" {
            err.push(PreprocessorDiagnostic::UnreachableDisciplineQualifier {
                qualifier: qualifier.to_string(),
                span: qspan,
            });
        }
        Some(qualifier)
    });

    (name, qualifier, span)
}

/// The qualifiers of Syntax 10-1, which are the digital net and variable types.
const NET_TYPE_QUALIFIERS: &[&str] = &[
    "integer", "real", "reg", "wreal", "wire", "tri", "wand", "triand", "wor", "trior", "trireg",
    "tri0", "tri1", "supply0", "supply1",
];

// const MACRO_ARG_DEF_TERMINATOR_SET: TokenSet =
//     TokenSet::new(&[RawToken::ParenClose]).union(MACRO_TERMINATOR_SET);

// const MACRO_TERMINATOR_SET: TokenSet = TokenSet::new(&[RawToken::Eof, RawToken::Newline]);

pub(crate) fn parse_define<'a>(
    p: &mut Parser<'a, '_>,
    err: &mut Diagnostics,
    sm: &mut SourceMap,
    end: FullTokenIdx,
) -> Option<(&'a str, Macro<'a>)> {
    let start = p.current_range();
    p.bump();
    let name = p.current_text();
    // let tspan = trace_span!("parsing `define", name = name);
    // let _tspan = tspan.enter();

    let followed_by_bracket = p.followed_by_bracket_without_space();
    let mut success = p.expect(PreprocessorToken::SimpleIdent, "an identifier", err);
    let args = if followed_by_bracket {
        debug_assert!(p.at(PreprocessorToken::OpenParen));
        p.bump();
        let mut args = Vec::new();
        loop {
            if !p.before(end) {
                success = false;
                err.push(UnexpectedEof {
                    expected: ")",
                    span: CtxSpan { ctx: p.ctx(), range: p.current_range() },
                });
                break;
            }
            args.push(p.current_text());
            if !p.expect(PreprocessorToken::SimpleIdent, "an identifier", err) {
                success = false
            }

            if !p.before(end) {
                success = false;
                err.push(UnexpectedEof {
                    expected: ")",
                    span: CtxSpan { ctx: p.ctx(), range: p.current_range() },
                });
                break;
            }

            if p.eat(PreprocessorToken::CloseParen) {
                break;
            } else {
                let expect = p.expect(PreprocessorToken::Comma, ")", err);
                success &= expect;
            }
        }
        args
    } else {
        Vec::new()
    };

    let head = p.previous_range().end() - start.start();

    let mut body = Vec::new();

    while p.before(end) {
        parse_macro_token(p, err, &args, &mut body, sm, end)
    }
    // p.bump();

    let range = TextRange::new(start.start(), p.end_pos(end));
    if success {
        Some((
            name,
            Macro { head, body, arg_cnt: args.len(), span: p.current_span().with_range(range) },
        ))
    } else {
        None
    }
}

fn parse_macro_token<'a>(
    p: &mut Parser<'a, '_>,
    err: &mut Diagnostics,
    args: &[&'a str],
    dst: &mut Vec<ParsedToken<'a>>,
    sm: &mut SourceMap,
    end: FullTokenIdx,
) {
    // trace!(token = display(p.current()), "parse macro token");

    if p.at(PreprocessorToken::SimpleIdent) {
        if let Some(arg) = args.iter().position(|x| *x == p.current_text()) {
            // debug!(name = p.current_text(), idx = arg, "macro arg reference");
            p.bump();
            dst.push(ParsedToken {
                range: p.current_range(),
                kind: ParsedTokenKind::ArgumentReference(MacroArg::from(arg)),
            });
            return;
        }
    }

    if p.at(PreprocessorToken::CompilerDirective) {
        match p.compiler_directive() {
            // `` `__FILE__ `` / `` `__LINE__ `` must be stored like macros so they
            // expand at the call site of the enclosing `` `define ``. Treating them
            // as unexpected without bumping the parser would spin forever - and the
            // same applies to every other directive that is not valid here (such as
            // `` `begin_keywords ``), hence the `bump()` in the fallback arm.
            CompilerDirective::Macro | CompilerDirective::File | CompilerDirective::Line => {
                let (call, range) = parse_macro_call(p, err, args, sm, end);
                dst.push(ParsedToken { range, kind: ParsedTokenKind::MacroCall(call) });
            }
            _ => {
                err.push(PreprocessorDiagnostic::UnexpectedToken(CtxSpan {
                    ctx: p.ctx,
                    range: p.current_range(),
                }));
                p.bump();
            }
        }
        return;
    }

    p.bump_to_macro(dst, end, err)
}
pub(crate) fn parse_macro_call<'a>(
    p: &mut Parser<'a, '_>,
    err: &mut Diagnostics,
    args: &[&'a str],
    sm: &mut SourceMap,
    end: FullTokenIdx,
) -> (MacroCall<'a>, TextRange) {
    // let tspan = trace_span!("parsing macro call");
    // let _tspan = tspan.enter();

    let start = p.current_range();
    let name = &p.current_text()[1..];
    let followed = p.followed_by_bracket_without_space();
    p.bump();
    let arg_bindings = if followed {
        p.bump();
        let mut arg_bindings = TiVec::<MacroArg, _>::with_capacity(4);
        'outer: while !p.eat(PreprocessorToken::CloseParen) {
            let mut dst = Vec::with_capacity(18);
            let start = p.current_range().start();

            let mut depth = 0; // allow for nested brackets inside macros
            loop {
                match p.current() {
                    PreprocessorToken::OpenParen => depth += 1,
                    PreprocessorToken::CloseParen | PreprocessorToken::Comma if depth == 0 => {
                        break;
                    }
                    PreprocessorToken::CloseParen => depth -= 1,
                    _ if p.before(end) => (),
                    _ => {
                        let end = p.previous_range().end();
                        arg_bindings.push((dst, TextRange::new(start, end)));
                        err.push(UnexpectedEof {
                            expected: ")",
                            span: CtxSpan { ctx: p.ctx(), range: p.current_range() },
                        });
                        break 'outer;
                    }
                }
                parse_macro_token(p, err, args, &mut dst, sm, end)
            }

            p.eat(PreprocessorToken::Comma);

            let end = p.previous_range().end();
            arg_bindings.push((dst, TextRange::new(start, end)));
        }
        arg_bindings
    } else {
        TiVec::new()
    };

    let span = start.cover(p.previous_range());
    (MacroCall { name, arg_bindings }, span)
}
