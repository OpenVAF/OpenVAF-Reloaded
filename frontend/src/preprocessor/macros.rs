use super::lexer::Token as LexicalToken;
use crate::lints::dispatch_early;
use crate::literals::unesacpe_string;
use crate::parser::tokenstream::Token as ParserToken;
use crate::preprocessor::error::{Error, Result};
use crate::preprocessor::lints::MacroCutOffAtFileEnd;
use crate::preprocessor::tokenstream::Token::FileInclude;
use crate::preprocessor::tokenstream::{
    MacroArg, MacroCall, SpannedToken, Token, TokenStream, UnresolvedCondition,
};
use crate::preprocessor::{Preprocessor, TokenProcessor};
use crate::sourcemap::span::DUMMY_SP;
use crate::symbol::{Ident, Symbol};
use crate::Span;
use index_vec::IndexVec;
use std::mem::swap;

pub struct MacroParser<'p, 'sm> {
    pub parent: &'p mut Preprocessor<'sm>,
    pub dst: TokenStream,
    pub args: IndexVec<MacroArg, Symbol>,
}

impl<'p, 'sm> MacroParser<'p, 'sm> {
    pub(super) fn run(&mut self, macro_name: Symbol) {
        loop {
            let (token, span) = if let Some(res) = self.parent.lexer.next() {
                res
            } else {
                dispatch_early(Box::new(MacroCutOffAtFileEnd {
                    span: self.parent.lexer.span(),
                    name: macro_name,
                }));
                break;
            };

            match self.process_token(token, span) {
                Ok(ContinueParsing(true)) => (),
                Ok(ContinueParsing(false)) => break,

                Err(Error::MissingTokenAtEnd(token, span)) => {
                    self.parent
                        .errors
                        .add(Error::MissingTokenAtEnd(token, span));
                    break;
                }
                Err(Error::EndTooEarly(span)) => {
                    self.parent.errors.add(Error::EndTooEarly(span));
                    break;
                }

                Err(error) => self.parent.errors.add(error),
            }
        }
    }

    fn process_token_inside_directive(
        &mut self,
        token: LexicalToken,
        span: Span,
        from: Span,
        unclosed: LexicalToken,
    ) -> Result {
        if self.process_token(token, span)?.0 {
            Ok(())
        } else {
            Err(Error::MissingTokenAtEnd(unclosed, from.extend(span)))
        }
    }

    fn parse_ifdef(&mut self, inverted: bool) -> Result<UnresolvedCondition> {
        let name = self.parent.lexer.expect_simple_ident()?;
        let mut true_tokens = Vec::with_capacity(32);

        std::mem::swap(&mut self.dst, &mut true_tokens);

        loop {
            let (token, span) = self
                .parent
                .lookahead_expecting(name.span, LexicalToken::MacroEndIf)?;
            match token {
                LexicalToken::MacroEndIf => {
                    self.parent.lexer.consume_lookahead();
                    std::mem::swap(&mut self.dst, &mut true_tokens);
                    return Ok(UnresolvedCondition {
                        if_def: name.name,
                        inverted,
                        true_tokens,
                        else_ifs: Vec::new(),
                        else_tokens: Vec::new(),
                    });
                }

                LexicalToken::MacroElsif => break,

                LexicalToken::MacroElse => {
                    self.parent.lexer.consume_lookahead();
                    let else_tokens = self.parse_else(name.span)?;
                    return Ok(UnresolvedCondition {
                        if_def: name.name,
                        inverted,
                        true_tokens,
                        else_ifs: Vec::new(),
                        else_tokens,
                    });
                }

                _ => self.process_token_inside_directive(
                    token,
                    span,
                    name.span,
                    LexicalToken::MacroEndIf,
                )?,
            }
        }

        let mut else_tokens = Vec::new();
        let mut else_ifs = Vec::new();

        loop {
            let (token, span) = self.parent.lookahead(name.span)?;
            match token {
                LexicalToken::MacroEndIf => {
                    self.parent.lexer.consume_lookahead();
                    break;
                }

                LexicalToken::MacroElsif => {
                    self.parent.lexer.consume_lookahead();
                    let else_if = self.parse_else_ifdef()?;
                    else_ifs.push(else_if)
                }

                LexicalToken::MacroElse => {
                    self.parent.lexer.consume_lookahead();
                    else_tokens = self.parse_else(span)?;
                    break;
                }
                _ => unreachable!("Should be handled inside sub functions"),
            }
        }

        Ok(UnresolvedCondition {
            if_def: name.name,
            inverted,
            true_tokens,
            else_ifs,
            else_tokens,
        })
    }

    fn parse_else_ifdef(&mut self) -> Result<(Symbol, Vec<SpannedToken>)> {
        let name = self.parent.lexer.expect_simple_ident()?;
        let mut tokens = Vec::new();
        std::mem::swap(&mut self.dst, &mut tokens);
        loop {
            let (token, span) = self
                .parent
                .lookahead_expecting(name.span, LexicalToken::MacroEndIf)?;
            match token {
                LexicalToken::MacroEndIf | LexicalToken::MacroElsif | LexicalToken::MacroElse => {
                    break
                }
                _ => self.process_token_inside_directive(
                    token,
                    span,
                    name.span,
                    LexicalToken::MacroEndIf,
                )?,
            }
        }
        std::mem::swap(&mut self.dst, &mut tokens);
        Ok((name.name, tokens))
    }

    fn parse_else(&mut self, else_span: Span) -> Result<Vec<SpannedToken>> {
        let mut tokens = Vec::new();
        std::mem::swap(&mut self.dst, &mut tokens);
        loop {
            let (token, span) = self
                .parent
                .lookahead_expecting(else_span, LexicalToken::MacroEndIf)?;
            match token {
                LexicalToken::MacroEndIf => {
                    self.parent.lexer.consume_lookahead();
                    break;
                }
                _ => self.process_token_inside_directive(
                    token,
                    span,
                    else_span,
                    LexicalToken::MacroEndIf,
                )?,
            }
        }
        std::mem::swap(&mut self.dst, &mut tokens);
        Ok(tokens)
    }
}

// small bool wrapper that defaults to true instead of false
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct ContinueParsing(pub bool);

impl Default for ContinueParsing {
    fn default() -> Self {
        Self(true)
    }
}

impl<'p, 'sm> TokenProcessor<'sm> for MacroParser<'p, 'sm> {
    type Res = ContinueParsing;

    #[inline(always)]
    fn preprocessor(&self) -> &Preprocessor<'sm> {
        self.parent
    }

    #[inline(always)]
    fn save_token(&mut self, token: ParserToken, span: Span) {
        self.dst.push((Token::ResolvedToken(token), span))
    }

    fn handle_include(&mut self, include_span: Span) -> Result {
        match self
            .parent
            .lexer
            .expect_optional_at(LexicalToken::LiteralString, include_span)
        {
            Err(error) => self.parent.errors.add(error),
            Ok(span) => {
                let mut literal_span = span.data();
                literal_span.lo += 1;
                literal_span.hi -= 1;

                let path = self.parent.lexer.slice(&literal_span);
                let path = unesacpe_string(path);
                let call_span = include_span.extend(span);

                self.dst.push((FileInclude(path), call_span));
            }
        }

        Ok(())
    }

    fn handle_macro_call(&mut self, name: Ident, args: bool) -> Result {
        let arg_bindings = if args {
            let mut arg_bindings = IndexVec::with_capacity(4);

            let paren_span = self
                .parent
                .lexer
                .expect(LexicalToken::ParenOpen)
                .expect("Lexer said this should be here");

            'list: loop {
                let mut depth = 0u32;
                let mut arg = Vec::with_capacity(8);
                swap(&mut arg, &mut self.dst);
                loop {
                    let (token, span) = self
                        .parent
                        .next_expecting(paren_span, LexicalToken::ParenClose)?;
                    match token {
                        LexicalToken::Comma if depth == 0 => break,

                        LexicalToken::ParenClose if depth == 0 => {
                            swap(&mut arg, &mut self.dst);
                            arg_bindings.push(arg);
                            break 'list;
                        }

                        LexicalToken::ParenOpen => depth += 1,
                        LexicalToken::ParenClose => depth -= 1,
                        _ => (),
                    }

                    // We cant use self.process_token_inside_directive here because we want to
                    // save process process_token errors instead of returning upon encountering them using?
                    match self.process_token(token, span) {
                        Ok(ContinueParsing(true)) => (),

                        Ok(ContinueParsing(false)) => {
                            return Err(Error::MissingTokenAtEnd(
                                LexicalToken::ParenClose,
                                paren_span.extend(span),
                            ))
                        }

                        Err(error) => self.parent.errors.add(error),
                    }
                }
                swap(&mut arg, &mut self.dst);
                arg_bindings.push(arg);
            }

            arg_bindings
        } else {
            IndexVec::new()
        };

        let call = MacroCall {
            name: name.name,
            arg_bindings,
        };

        self.dst.push((
            Token::MacroCall(call),
            name.span
                .data()
                .extend(self.parent.lexer.span_data())
                .compress(),
        ));
        Ok(())
    }

    fn handle_macrodef(&mut self) -> Result {
        let (name, def) = self.parent.parse_macro_definition()?;
        // dummy span is used because the spans of macro definitions are never used
        // this might change in the future (but unlikely) then the parse_maro_definition() method needs to be modified
        self.dst.push((Token::MacroDefinition(name, def), DUMMY_SP));
        Ok(())
    }

    fn handle_ifdef(&mut self, inverted: bool) -> Result {
        let span = self.parent.lexer.span();

        match self.parse_ifdef(inverted) {
            // DUMMY SPAN because the only thing we care about is ctxt and that is ony added during name resolution
            Ok(res) => self.dst.push((Token::Condition(res), DUMMY_SP)),
            Err(error) => {
                self.parent.skip_until_endif(span)?;
                return Err(error);
            }
        }

        Ok(())
    }

    #[inline(always)]
    fn handle_newline(&mut self, _span: Span) -> Result<ContinueParsing> {
        Ok(ContinueParsing(false))
    }

    #[inline(always)]
    fn handle_simple_ident(&mut self, ident: Ident) -> Result {
        if let Some(idx) = self.args.iter().position(|&name| name == ident.name) {
            self.dst
                .push((Token::ArgumentReference(MacroArg::new(idx)), ident.span))
        } else {
            self.save_token(ParserToken::Ident(ident.name), ident.span)
        }
        Ok(())
    }
}