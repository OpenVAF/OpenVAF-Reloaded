use std::mem;
use std::sync::Arc;

use preprocessor::sourcemap::{CtxSpan, SourceContext, SourceMap};
use preprocessor::{DirectiveIdx, Directives, SourceProvider, Token};
use rowan::{GreenNodeBuilder, Language};
use tokens::KeywordSet;
use vfs::FileId;

use crate::syntax_node::{GreenNode, VerilogALanguage};
use crate::{SyntaxError, SyntaxKind, TextRange, TextSize, T};

pub(crate) struct SyntaxTreeBuilder<'a> {
    tokens: &'a [Token],
    text_pos: TextSize,
    token_pos: usize,

    state: State,

    errors: Vec<SyntaxError>,
    last_error: Option<SyntaxError>,

    inner: GreenNodeBuilder<'static>,

    db: &'a dyn SourceProvider,

    current_src: Arc<str>,
    panic: bool,
    err_depth: u32,
    sm: &'a SourceMap,
    ranges: Vec<(TextRange, SourceContext, TextSize)>,
    current_range: CtxSpan,
    /// Runs of tree text that a `` `begin_keywords `` directive applies to,
    /// sorted and non-overlapping. Regions using the default keyword set are not
    /// recorded.
    keyword_regions: Vec<(TextRange, KeywordSet)>,
    /// The same, for the directives that set a default (10.2, 10.3).
    directive_regions: Vec<(TextRange, DirectiveIdx)>,
    directive_states: Arc<Vec<Directives>>,
}

enum State {
    PendingStart,
    Normal,
    PendingFinish,
}

pub(crate) struct Built {
    pub tree: GreenNode,
    pub errors: Vec<SyntaxError>,
    pub ranges: Vec<(TextRange, SourceContext, TextSize)>,
    pub keyword_regions: KeywordRegions,
    pub directives: DirectiveMap,
}

/// Which directive state covers which run of the tree text.
///
/// `` `default_discipline `` (10.2) and `` `default_transition `` (10.3) apply to
/// "the text stream following the directive", and the directive itself is gone by
/// the time there is a tree, so the state travels with the tokens and is turned
/// back into positions here. Runs in the empty state are not recorded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DirectiveMap {
    regions: Vec<(TextRange, DirectiveIdx)>,
    states: Arc<Vec<Directives>>,
}

impl DirectiveMap {
    /// What the directives had set at `pos`, which is nothing unless one of them
    /// was used before it.
    pub fn get(&self, pos: TextSize) -> &Directives {
        let idx = self.regions.partition_point(|(range, _)| range.end() <= pos);
        let state = match self.regions.get(idx) {
            Some(&(range, state)) if range.contains(pos) => state.0 as usize,
            _ => DirectiveIdx::EMPTY.0 as usize,
        };
        static EMPTY: Directives = Directives { transition: None, disciplines: Vec::new() };
        self.states.get(state).unwrap_or(&EMPTY)
    }
}

/// The `` `begin_keywords `` regions of a parsed file, in tree coordinates.
#[derive(Debug, Default)]
pub(crate) struct KeywordRegions(Vec<(TextRange, KeywordSet)>);

impl KeywordRegions {
    /// The keyword set in effect at `pos`, which is OpenVAF's default set unless
    /// a `` `begin_keywords `` directive covers it.
    pub fn get(&self, pos: TextSize) -> KeywordSet {
        let idx = self.0.partition_point(|(range, _)| range.end() <= pos);
        match self.0.get(idx) {
            Some(&(range, set)) if range.contains(pos) => set,
            _ => KeywordSet::default(),
        }
    }
}

impl<'a> SyntaxTreeBuilder<'a> {
    pub(super) fn token(&mut self, kind: SyntaxKind) {
        match mem::replace(&mut self.state, State::Normal) {
            State::PendingStart => unreachable!(),
            State::PendingFinish => self.inner.finish_node(),
            State::Normal => (),
        }
        self.eat_trivia();
        let token = self.tokens[self.token_pos];
        self.panic &= !matches!(
            kind,
            T![;] | T![end] | T![endnature] | T![endmodule] | T![enddiscipline] | T![endfunction]
        ) || self.err_depth != u32::MAX;
        self.do_token(kind, token.span, token.keywords, token.directives);
    }

    pub(super) fn start_node(&mut self, kind: SyntaxKind) {
        match mem::replace(&mut self.state, State::Normal) {
            State::PendingStart => {
                self.inner.start_node(VerilogALanguage::kind_to_raw(kind));
                // No need to attach trivia to previous node: there is no
                // previous node.
                return;
            }
            State::PendingFinish => self.inner.finish_node(),
            State::Normal => (),
        }

        if self.err_depth != u32::MAX {
            self.err_depth += 1
        } else if kind == SyntaxKind::ERROR {
            self.err_depth = 0
        } else {
            self.eat_trivia();
        }
        self.inner.start_node(VerilogALanguage::kind_to_raw(kind));
    }

    pub(super) fn finish_node(&mut self) {
        match mem::replace(&mut self.state, State::PendingFinish) {
            State::PendingStart => unreachable!(),
            State::PendingFinish => self.inner.finish_node(),
            State::Normal => (),
        }
        if self.err_depth == 0 {
            if let Some(mut err) = self.last_error.take() {
                if let SyntaxError::UnexpectedToken { span, panic_end, .. } = &mut err {
                    if span.end() < self.text_pos {
                        *panic_end = Some(self.text_pos);
                    }
                }

                self.errors.push(err)
            }
            self.err_depth = u32::MAX;
        } else if self.err_depth != u32::MAX {
            self.err_depth -= 1;
        }
    }

    pub(super) fn error(&mut self, error: parser::SyntaxError) {
        let n_trivia =
            self.tokens[self.token_pos..].iter().take_while(|it| it.kind.is_trivia()).count();
        let leading_trivia = &self.tokens[self.token_pos..self.token_pos + n_trivia];
        let pos =
            self.text_pos + leading_trivia.iter().map(|it| it.span.range.len()).sum::<TextSize>();
        let parser::SyntaxError::UnexpectedToken { expected, found }: parser::SyntaxError = error;
        let missing_delimiter = found == T![end];
        if self.token_pos + n_trivia == self.tokens.len() {
            let expected_at = expected
                .data
                .iter()
                .any(|t| *t == T![;] || *t == T![')'])
                .then(|| TextRange::at(self.text_pos, 0.into()));
            let error = SyntaxError::UnexpectedToken {
                expected,
                found,
                span: TextRange::at(
                    self.text_pos,
                    self.tokens.last().map_or_else(|| TextSize::from(0), |t| t.span.range.len()),
                ),
                expected_at,
                missing_delimiter,
                panic_end: None,
            };
            self.errors.push(error);
            return;
        }
        let len = self.tokens[self.token_pos + n_trivia].span.range.len();

        let panic = mem::replace(&mut self.panic, true);
        if panic && !missing_delimiter || self.last_error.is_some() {
            return;
        }

        let expected_at = expected
            .data
            .iter()
            .any(|t| *t == T![;] || *t == T![')'])
            .then(|| TextRange::at(self.text_pos, 0.into()));
        let error = SyntaxError::UnexpectedToken {
            expected,
            found,
            span: TextRange::at(pos, len),
            expected_at,
            missing_delimiter,
            panic_end: None,
        };
        self.last_error = Some(error)
    }

    pub(super) fn new(
        db: &'a dyn SourceProvider,
        root_file: FileId,
        tokens: &'a [Token],
        sm: &'a SourceMap,
        directive_states: Arc<Vec<Directives>>,
    ) -> Self {
        let current_src = db.file_text(root_file).unwrap_or_else(|_| Arc::from(""));
        Self {
            tokens,
            text_pos: 0.into(),
            token_pos: 0,
            state: State::PendingStart,
            inner: Default::default(),
            db,
            sm,
            current_src,
            keyword_regions: Vec::new(),
            directive_regions: Vec::new(),
            directive_states,
            ranges: Vec::with_capacity(128),
            current_range: CtxSpan {
                ctx: SourceContext::ROOT,
                range: TextRange::empty(TextSize::from(0)),
            },
            panic: false,
            err_depth: u32::MAX,
            errors: Vec::new(),
            last_error: None,
        }
    }

    pub(super) fn finish(mut self) -> Built {
        match mem::replace(&mut self.state, State::Normal) {
            State::PendingFinish => {
                self.eat_trivia();
                self.inner.finish_node()
            }
            State::PendingStart | State::Normal => unreachable!(),
        }
        let start = self.ranges.last().map_or(0.into(), |(range, _, _)| range.end());
        let range = TextRange::new(start, self.text_pos);
        self.ranges.push((range, self.current_range.ctx, self.current_range.range.start()));
        Built {
            tree: self.inner.finish(),
            errors: self.errors,
            ranges: self.ranges,
            keyword_regions: KeywordRegions(self.keyword_regions),
            directives: DirectiveMap {
                regions: self.directive_regions,
                states: self.directive_states,
            },
        }
    }

    fn eat_trivia(&mut self) {
        while let Some(&token) = self.tokens.get(self.token_pos) {
            if !token.kind.is_trivia() {
                break;
            }
            self.do_token(token.kind, token.span, token.keywords, token.directives);
        }
    }

    /// Extends the current `` `begin_keywords `` region, or starts a new one.
    ///
    /// Called for every token in tree order, so `self.text_pos` is the start of
    /// the token that is about to be appended.
    fn record_keywords(&mut self, keywords: KeywordSet, len: TextSize) {
        if keywords == KeywordSet::default() {
            return;
        }
        let end = self.text_pos + len;
        match self.keyword_regions.last_mut() {
            Some((range, set)) if *set == keywords && range.end() == self.text_pos => {
                *range = TextRange::new(range.start(), end)
            }
            _ => self.keyword_regions.push((TextRange::new(self.text_pos, end), keywords)),
        }
    }

    /// Extends the current directive region, or starts a new one. Same shape as
    /// [`Self::record_keywords`], down to leaving the default state unrecorded.
    fn record_directives(&mut self, directives: DirectiveIdx, len: TextSize) {
        if directives == DirectiveIdx::EMPTY {
            return;
        }
        let end = self.text_pos + len;
        match self.directive_regions.last_mut() {
            Some((range, state)) if *state == directives && range.end() == self.text_pos => {
                *range = TextRange::new(range.start(), end)
            }
            _ => self.directive_regions.push((TextRange::new(self.text_pos, end), directives)),
        }
    }

    fn do_token(
        &mut self,
        kind: SyntaxKind,
        span: CtxSpan,
        keywords: KeywordSet,
        directives: DirectiveIdx,
    ) {
        let same_ctx = span.ctx == self.current_range.ctx;
        let is_continuous = same_ctx && span.range.start() == self.current_range.range.end();
        if is_continuous {
            self.current_range.range = self.current_range.range.cover(span.range);
        } else {
            let start = self.ranges.last().map_or(0.into(), |(range, _, _)| range.end());
            let range = TextRange::new(start, self.text_pos);
            let old_range = mem::replace(&mut self.current_range, span);
            self.ranges.push((range, old_range.ctx, old_range.range.start()))
        }

        if !same_ctx {
            // We are in a different ctx and therefore the text comes from somewhere else...
            // Switch the src code
            // Unwrap is okay here because the file was already read succesffully by he preprocessor or the SourceContext wouldn't exist
            let decl = self.sm.ctx_data(span.ctx).decl;
            let src = self.db.file_text(decl.file).unwrap();
            self.current_src = src;
        }

        let range = span.to_file_span(self.sm).range;
        self.record_keywords(keywords, range.len());
        self.record_directives(directives, range.len());
        let text = &self.current_src[range];
        self.text_pos += range.len();
        self.token_pos += 1;
        self.inner.token(VerilogALanguage::kind_to_raw(kind), text);
    }
}
