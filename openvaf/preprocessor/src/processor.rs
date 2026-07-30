use std::io;
use std::iter::once;
use std::rc::Rc;
use std::sync::Arc;

use ahash::AHashMap;
use stdx::{impl_debug_display, impl_idx_from};
use text_size::{TextRange, TextSize};
use tokens::parser::SyntaxKind;
use tokens::KeywordSet;
use tokens::SyntaxKind::{INT_NUMBER, L_PAREN, R_PAREN, STR_LIT};
// use tracing::{debug, debug_span, trace};
use typed_index_collections::{TiSlice, TiVec};
use vfs::{FileId, VfsPath};

use crate::diagnostics::PreprocessorDiagnostic::{
    self, MacroArgumentCountMismatch, MacroNotFound, UnexpectedToken,
};
use crate::grammar::{
    parse_begin_keywords, parse_condition, parse_define, parse_include, parse_macro_call,
};
use crate::parser::{CompilerDirective, LexerState, Parser, PreprocessorToken};
use crate::sourcemap::{CtxSpan, FileSpan, SourceContext, SourceMap};
use crate::{Diagnostics, FileReadError, ScopedTextArea, SourceProvider, Token};

pub(crate) struct Processor<'a> {
    pub(crate) source_map: SourceMap,
    sources: &'a dyn SourceProvider,
    arena: &'a ScopedTextArea,
    macros: AHashMap<&'a str, Macro<'a>>,
    include_dirs: Arc<[VfsPath]>,
    /// Lexer state shared with every [`Parser`] this processor creates.
    lexer_state: Rc<LexerState>,
    /// The `` `begin_keywords `` directives that are still open, innermost last.
    ///
    /// The directives nest and span file boundaries (VAMS-2023 10.6), so the
    /// stack belongs to the processor rather than to a single parser.
    keyword_stack: Vec<(KeywordSet, CtxSpan)>,
    /// Monotonic id for virtual expansion files allocated for `` `__FILE__ `` / `` `__LINE__ ``.
    expand_seq: u32,
}

impl<'a> Processor<'a> {
    pub fn new(
        storage: &'a ScopedTextArea,
        root_file: FileId,
        sources: &'a dyn SourceProvider,
    ) -> Result<Self, FileReadError> {
        let src = sources.file_text(root_file)?;
        let src = storage.ensure(src);
        let macros = sources
            .macro_flags(root_file)
            .iter()
            .map(|name| -> (&str, Macro) {
                (
                    storage.ensure(name.clone()),
                    Macro { head: 0.into(), span: CtxSpan::dummy(), body: vec![], arg_cnt: 0 },
                )
            })
            .collect();
        let res = Self {
            source_map: SourceMap::new(root_file, TextSize::of(src)),
            macros,
            arena: storage,
            sources,
            include_dirs: sources.include_dirs(root_file),
            lexer_state: Rc::default(),
            keyword_stack: Vec::new(),
            expand_seq: 0,
        };
        Ok(res)
    }

    pub fn run(&mut self, file: FileId) -> (Vec<Token>, Diagnostics) {
        let working_dir = self.sources.file_path(file).parent().unwrap();

        let mut err = Diagnostics::new();
        let mut dst = Vec::new();
        let parser = Parser::new(
            self.arena.get(0),
            SourceContext::ROOT,
            working_dir,
            &mut dst,
            self.lexer_state.clone(),
            &mut err,
        );
        self.process_file(parser, &mut err);

        // every `begin_keywords must be closed by the end of the compilation unit
        for (_, span) in self.keyword_stack.drain(..) {
            err.push(PreprocessorDiagnostic::UnterminatedKeywords { span })
        }

        (dst, err)
    }

    /// Applies the innermost open `` `begin_keywords `` directive, or OpenVAF's
    /// default keyword set if there is none.
    fn sync_keywords(&self) {
        let keywords =
            self.keyword_stack.last().map_or(KeywordSet::default(), |&(keywords, _)| keywords);
        self.lexer_state.set_keywords(keywords)
    }

    /// VAMS-2023 10.6: the keyword directives may only appear outside a design
    /// element. Returns `true` (and reports) when that is violated.
    fn reject_keywords_in_design_element(
        &self,
        name: &'static str,
        span: CtxSpan,
        err: &mut Diagnostics,
    ) -> bool {
        if self.lexer_state.in_design_element() {
            err.push(PreprocessorDiagnostic::KeywordsInDesignElement { name, span });
            true
        } else {
            false
        }
    }

    pub(crate) fn is_macro_defined(&mut self, name: &'a str) -> bool {
        self.macros.contains_key(name)
    }

    pub(crate) fn include_file(
        &mut self,
        path: &str,
        span: CtxSpan,
        dst: &mut Vec<Token>,
        errors: &mut Diagnostics,
        workdir: &VfsPath,
    ) -> Result<(), (FileReadError, Option<VfsPath>)> {
        let mut include_dirs = once(workdir).chain(&*self.include_dirs);
        let found = loop {
            if let Some(dir) = include_dirs.next() {
                if let Some(path) = dir.join(path) {
                    let file = self.sources.file_id(path.clone());
                    match self.sources.file_text(file) {
                        Ok(contents) => break Some((contents, file)),
                        Err(FileReadError::Io(io::ErrorKind::NotFound)) => (),
                        Err(err) => return Err((err, Some(path))),
                    }
                }
            } else {
                break None;
            }
        };
        let (src, file) = found.ok_or((FileReadError::Io(io::ErrorKind::NotFound), None))?;
        let src = self.arena.ensure(src);
        let workdir = self.sources.file_path(file).parent().unwrap();

        let ctx = self
            .source_map
            .add_ctx(FileSpan { file, range: TextRange::up_to(TextSize::of(src)) }, span);

        let parser = Parser::new(src, ctx, workdir, dst, self.lexer_state.clone(), errors);
        self.process_file(parser, errors);

        Ok(())
    }

    pub(crate) fn define_macro(
        &mut self,
        name: &'a str,
        def: Macro<'a>,
        diagnostics: &mut Diagnostics,
    ) {
        let span = def.head_span();
        if let Some(old) = self.macros.insert(name, def) {
            diagnostics.push(PreprocessorDiagnostic::MacroOverwritten {
                old: old.head_span(),
                new: span,
                name: name.to_owned(),
            })
        }
    }

    fn process_macro_token(
        &mut self,
        token: &ParsedTokenKind<'a>,
        span: CtxSpan,
        args: &TiSlice<MacroArg, Vec<Token>>,
        dst: &mut Vec<Token>,
        errors: &mut Diagnostics,
    ) {
        match *token {
            // the token kinds of a macro body are resolved where the `define is
            // parsed, but the expansion site decides which identifiers are
            // reserved names there
            ParsedTokenKind::ResolvedToken(kind) => {
                dst.push(Token { kind, span, keywords: self.lexer_state.keywords() })
            }
            ParsedTokenKind::ArgumentReference(arg) => {
                dst.extend(&args[arg]);
            }
            ParsedTokenKind::MacroCall(ref call) => self.call_macro(call, span, args, dst, errors),
        }
    }

    pub(crate) fn call_macro(
        &mut self,
        call: &MacroCall<'a>,
        span: CtxSpan,
        args: &TiSlice<MacroArg, Vec<Token>>,
        dst: &mut Vec<Token>,
        errors: &mut Diagnostics,
    ) {
        // `` `__FILE__ `` / `` `__LINE__ `` may appear inside `` `define `` bodies as
        // nested macro calls; expand them at the nested call site.
        if call.name == "__FILE__" {
            self.expand_file_line(true, span, dst);
            return;
        }
        if call.name == "__LINE__" {
            self.expand_file_line(false, span, dst);
            return;
        }

        // TODO track recursion
        //
        let parent_ctx_span = self.source_map.ctx_data(span.ctx).decl.range.start();
        if let Some(def) = self.macros.get(&call.name).cloned() {
            let new_args: TiVec<_, _> = call
                .arg_bindings
                .iter()
                .map(|(arg, _decl)| {
                    let mut dst = Vec::new();
                    for ParsedToken { kind, range } in arg {
                        // trace!(range = debug(range), "Arg token");
                        let span = CtxSpan { range: range - parent_ctx_span, ctx: span.ctx };
                        self.process_macro_token(kind, span, args, &mut dst, errors)
                    }
                    dst
                })
                .collect();

            if new_args.len() == def.arg_cnt || def.arg_cnt == 0 {
                let ctx = self.source_map.add_ctx(def.span.to_file_span(&self.source_map), span);
                for ParsedToken { kind, range } in &def.body {
                    let span = CtxSpan { range: range - def.span.range.start(), ctx };
                    self.process_macro_token(kind, span, &new_args, dst, errors)
                }
                if new_args.len() > def.arg_cnt {
                    // macro definition has no arguments, but some were parsed as part of the call
                    // so put the arguments back
                    let keywords = self.lexer_state.keywords();
                    dst.push(Token { kind: L_PAREN, span, keywords });
                    for arg in new_args {
                        for tok in arg {
                            dst.push(tok)
                        }
                    }
                    dst.push(Token { kind: R_PAREN, span, keywords });
                }
            } else {
                errors.push(MacroArgumentCountMismatch {
                    expected: def.arg_cnt,
                    found: new_args.len(),
                    span,
                })
            }
        } else {
            errors.push(MacroNotFound { name: call.name.to_owned(), span })
        }
    }

    /// Expand `` `__FILE__ `` (`is_file`) or `` `__LINE__ `` to a literal token whose
    /// text lives in a freshly allocated virtual file (green-tree text is always
    /// sliced from a `FileId`).
    fn expand_file_line(&mut self, is_file: bool, call_site: CtxSpan, dst: &mut Vec<Token>) {
        // Direct uses (and uses inside `` `include ``d files) report the current
        // input file. When expanding from a user-macro body the context decl is a
        // subrange of a file, and we report the macro invocation site instead so
        // idioms like `` `define LOC `__FILE__, `__LINE__ `` are useful.
        let loc_span = {
            let ctx_data = self.source_map.ctx_data(call_site.ctx);
            let decl = ctx_data.decl;
            let whole_file = self
                .sources
                .file_text(decl.file)
                .ok()
                .map(|src| decl.range == TextRange::up_to(TextSize::of(&*src)))
                .unwrap_or(true);
            if !whole_file {
                ctx_data.call_site.unwrap_or(call_site)
            } else {
                call_site
            }
        };
        let filespan = loc_span.to_file_span(&self.source_map);
        let lit_text: Arc<str> = if is_file {
            let path = self.sources.file_path(filespan.file);
            format!("\"{}\"", escape_pp_string(&path.to_string())).into()
        } else {
            let src =
                self.sources.file_text(filespan.file).expect("SourceContext file must be readable");
            let line = line_number_1based(&src, filespan.range.start());
            line.to_string().into()
        };

        let seq = self.expand_seq;
        self.expand_seq = seq.wrapping_add(1);
        let virt_path = format!("/<pp-expand>/{}/{}", if is_file { "file" } else { "line" }, seq);
        let file = self.sources.allocate_virtual_file(&virt_path, lit_text.clone());
        let lit_text = self.arena.ensure(lit_text);
        let range = TextRange::up_to(TextSize::of(lit_text));
        let ctx = self.source_map.add_ctx(FileSpan { file, range }, call_site);
        dst.push(Token {
            kind: if is_file { STR_LIT } else { INT_NUMBER },
            span: CtxSpan { range, ctx },
            // a literal is never a keyword, but the token still carries the set in
            // effect at the expansion site so the regions stay contiguous
            keywords: self.lexer_state.keywords(),
        });
    }

    pub(crate) fn process_file(&mut self, mut p: Parser<'a, '_>, err: &mut Diagnostics) {
        while !p.at(PreprocessorToken::Eof) {
            self.process_token(&mut p, err)
        }
    }

    pub(crate) fn process_token(&mut self, p: &mut Parser<'a, '_>, err: &mut Diagnostics) {
        match p.current() {
            PreprocessorToken::Define { end } => {
                if let Some((name, def)) = parse_define(p, err, &mut self.source_map, end) {
                    self.define_macro(name, def, err)
                }
            }
            PreprocessorToken::CompilerDirective => match p.compiler_directive() {
                CompilerDirective::Include => {
                    if let Some((file_name, range)) = parse_include(p, err) {
                        let span = CtxSpan { range, ctx: p.ctx() };
                        match self.include_file(file_name, span, p.dst, err, &p.working_dir) {
                            Ok(_) => (),
                            Err((FileReadError::InvalidTextFormat(err_msg), file)) => {
                                err.push(PreprocessorDiagnostic::InvalidTextFormat {
                                    file: file.unwrap(),
                                    span: Some(span),
                                    err: err_msg,
                                })
                            }
                            Err((FileReadError::Io(kind), file)) => {
                                err.push(PreprocessorDiagnostic::FileNotFound {
                                    file: file.map_or_else(
                                        || file_name.to_owned(),
                                        |path| path.to_string(),
                                    ),
                                    error: kind,
                                    span: Some(span),
                                })
                            }
                        }
                    }
                }
                CompilerDirective::IfDef => {
                    // let _span = debug_span!("preprocessing `ifdef");
                    // let _tspan = _span.enter();
                    p.bump();
                    parse_condition(p, err, self, false);
                }
                CompilerDirective::IfNotDef => {
                    // let _span = debug_span!("preprocessing `ifndef");
                    // let _tspan = _span.enter();
                    p.bump();
                    parse_condition(p, err, self, true);
                }
                CompilerDirective::Undef => {
                    p.bump();
                    let name = p.current_text();
                    if self.macros.contains_key(name) {
                        self.macros.remove(name);
                    } else {
                        err.push(PreprocessorDiagnostic::MacroNotDefined {
                            name: name.to_owned(),
                            span: p.current_span(),
                        })
                    }
                    p.bump();
                }
                CompilerDirective::ResetAll => {
                    let name = p.current_text();
                    err.push(PreprocessorDiagnostic::UnsupportedCompDir {
                        name: name.to_owned(),
                        span: p.current_span(),
                    });
                    p.bump();
                }
                CompilerDirective::BeginKeywords => {
                    if let Some((keywords, span)) = parse_begin_keywords(p, err) {
                        if !self.reject_keywords_in_design_element("begin_keywords", span, err) {
                            self.keyword_stack.push((keywords, span));
                            self.sync_keywords();
                        }
                    }
                }
                CompilerDirective::EndKeywords => {
                    let span = p.current_span();
                    p.bump();
                    if !self.reject_keywords_in_design_element("end_keywords", span, err) {
                        if self.keyword_stack.pop().is_none() {
                            err.push(PreprocessorDiagnostic::UnmatchedEndKeywords { span })
                        }
                        self.sync_keywords();
                    }
                }
                CompilerDirective::File => {
                    let span = p.current_span();
                    p.bump();
                    self.expand_file_line(true, span, p.dst);
                }
                CompilerDirective::Line => {
                    let span = p.current_span();
                    p.bump();
                    self.expand_file_line(false, span, p.dst);
                }
                CompilerDirective::Macro => {
                    let (call, range) =
                        parse_macro_call(p, err, &[], &mut self.source_map, p.end());
                    let span = CtxSpan { range, ctx: p.ctx() };
                    self.call_macro(&call, span, TiSlice::from_ref(&[]), p.dst, err);
                }

                _ => {
                    err.push(UnexpectedToken(p.current_span()));
                    p.bump()
                }
            },

            _ => p.save_token(err),
        }
    }
}

/// 1-based line number of `offset` in `src` (newlines before the offset).
fn line_number_1based(src: &str, offset: TextSize) -> u32 {
    let idx: usize = offset.into();
    let idx = idx.min(src.len());
    1 + src[..idx].bytes().filter(|&b| b == b'\n').count() as u32
}

/// Escape a path for embedding inside a Verilog string literal.
fn escape_pp_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

pub(crate) type MacroArgs<'s> = TiVec<MacroArg, (Vec<ParsedToken<'s>>, TextRange)>;

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash)]
pub(crate) struct MacroArg(u8);

impl_idx_from!(MacroArg(u8));
impl_debug_display!(c@MacroArg => "arg{}",c.0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedToken<'s> {
    pub(crate) range: TextRange,
    pub(crate) kind: ParsedTokenKind<'s>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParsedTokenKind<'s> {
    ResolvedToken(SyntaxKind),
    ArgumentReference(MacroArg),
    MacroCall(MacroCall<'s>),
}

impl From<SyntaxKind> for ParsedTokenKind<'static> {
    fn from(value: SyntaxKind) -> ParsedTokenKind<'static> {
        ParsedTokenKind::ResolvedToken(value)
    }
}
#[derive(Debug, Clone)]
pub(crate) struct Macro<'s> {
    pub head: TextSize,
    pub span: CtxSpan,
    pub body: Vec<ParsedToken<'s>>,
    pub arg_cnt: usize,
}

impl Macro<'_> {
    pub fn head_span(&self) -> CtxSpan {
        self.span.with_len(self.head)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MacroCall<'s> {
    pub name: &'s str,
    pub arg_bindings: MacroArgs<'s>,
}
