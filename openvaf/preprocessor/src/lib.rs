use std::sync::Arc;

use diagnostics::PreprocessorDiagnostic;
use sourcemap::{CtxSpan, SourceMap};
use vfs::{FileId, FileReadError, VfsPath};

use crate::processor::Processor;
// use tracing::trace_span;

pub mod diagnostics;
mod grammar;
mod parser;
mod processor;
pub mod sourcemap;

mod scoped_arc_arena;
#[cfg(test)]
#[rustfmt::skip]
mod tests;

type Text = Arc<str>;
type ScopedTextArea = scoped_arc_arena::ScopedArea<Text>;
type Diagnostics = Vec<PreprocessorDiagnostic>;

#[derive(PartialEq, Eq, Clone, Debug)]
pub struct Preprocess {
    pub ts: Arc<Vec<Token>>,
    pub sm: Arc<SourceMap>,
    pub diagnostics: Arc<Diagnostics>,
    /// The directive states the tokens refer to by [`DirectiveIdx`]. Index zero
    /// is the empty state and is the only entry a file without directives has.
    pub directives: Arc<Vec<Directives>>,
}

/// # Panics
/// This function panics if called multiple times in the same OpenVAF session
pub fn preprocess(sources: &dyn SourceProvider, file: FileId) -> Preprocess {
    // let span = trace_span!("preprocessor", main_file = display(sources.file_path(file)));
    // let _scope = span.enter();

    let storage = ScopedTextArea::new();
    let (ts, diagnostics, sm, directives) = match Processor::new(&storage, file, sources) {
        Ok(mut processor) => {
            let (ts, diagnostics) = processor.run(file);
            let directives = processor.take_directives();
            (ts, diagnostics, processor.source_map, directives)
        }
        Err(FileReadError::Io(error)) => (
            vec![],
            vec![PreprocessorDiagnostic::FileNotFound {
                file: sources.file_path(file).to_string(),
                error,
                span: None,
            }],
            SourceMap::new(file, 0.into()),
            vec![Directives::default()],
        ),
        Err(FileReadError::InvalidTextFormat(err)) => (
            vec![],
            vec![PreprocessorDiagnostic::InvalidTextFormat {
                file: sources.file_path(file),
                span: None,
                err,
            }],
            SourceMap::new(file, 0.into()),
            vec![Directives::default()],
        ),
    };

    Preprocess {
        ts: Arc::new(ts),
        diagnostics: Arc::new(diagnostics),
        sm: Arc::new(sm),
        directives: Arc::new(directives),
    }
}

pub trait SourceProvider {
    fn include_dirs(&self, root_file: FileId) -> Arc<[VfsPath]>;
    fn macro_flags(&self, file_root: FileId) -> Arc<[Arc<str>]>;

    fn file_text(&self, file: FileId) -> Result<Arc<str>, FileReadError>;
    fn file_path(&self, file: FileId) -> VfsPath;
    fn file_id(&self, path: VfsPath) -> FileId;

    /// Allocate a virtual file whose contents become the expansion text for a
    /// preprocessor-generated token (e.g. `` `__FILE__ `` / `` `__LINE__ ``).
    /// Token spans must point at real `FileId` text for the green tree builder.
    fn allocate_virtual_file(&self, path: &str, contents: Arc<str>) -> FileId;
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Token {
    pub span: CtxSpan,
    pub kind: tokens::parser::SyntaxKind,
    /// The keyword set that was active where this token was produced.
    ///
    /// Reserved-identifier checking happens on the syntax tree, long after the
    /// `` `begin_keywords `` regions have been consumed, so the active set
    /// travels with the tokens (VAMS-2023 10.6).
    pub keywords: tokens::KeywordSet,
    /// The directive state that was active where this token was produced, as an
    /// index into [`Preprocess::directives`].
    pub directives: DirectiveIdx,
}

/// A directive state in [`Preprocess::directives`].
///
/// Tokens carry an index rather than the state itself: the state holds a name and
/// a time, a token is [`Copy`] and there are a few million of them in a compact
/// model, and a file changes its directives a handful of times at most.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub struct DirectiveIdx(pub u32);

impl DirectiveIdx {
    /// The state of a file that has used none of the directives below.
    pub const EMPTY: DirectiveIdx = DirectiveIdx(0);
}

/// What the directives that set a default have set, at one point in the text
/// stream.
///
/// 10.1: "The scope of compiler directives extends from the point where it is
/// processed, across all files processed, to the point where another compiler
/// directive supersedes it" -- so this is a state that the token stream carries,
/// not a property of a file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Directives {
    /// `` `default_transition `` (10.3): the default rise and fall time of a
    /// transition filter that specifies neither.
    pub transition: Option<DefaultTransition>,
    /// `` `default_discipline `` (10.2), at most one entry per qualifier: "more
    /// than one `` `default_discipline `` directive can be in force
    /// simultaneously, provided each differs in qualifier".
    pub disciplines: Vec<DefaultDiscipline>,
}

// The transition time is a `f64` read from a literal, which is never a NaN, so
// the reflexivity `Eq` promises holds.
impl Eq for Directives {}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DefaultTransition {
    pub time: f64,
    /// The directive that set it, for a diagnostic that has to point at it.
    pub span: CtxSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultDiscipline {
    /// The discipline the directive names.
    pub name: Arc<str>,
    /// The net type the default is restricted to, if the directive gave one.
    pub qualifier: Option<Arc<str>>,
    pub span: CtxSpan,
}

impl Directives {
    /// The default discipline for a net that was declared without one.
    ///
    /// A net declared without a net type is of the default net type, which is
    /// `wire`, so a `wire`-qualified directive applies to it -- and in preference
    /// to an unqualified one, because 10.2 ends with "the more specific
    /// directives have higher precedence over general directives". No other
    /// qualifier can apply, because Verilog-A has no nets of any other type.
    pub fn default_discipline(&self) -> Option<&DefaultDiscipline> {
        let qualified = self.disciplines.iter().find(|it| it.qualifier.as_deref() == Some("wire"));
        qualified.or_else(|| self.disciplines.iter().find(|it| it.qualifier.is_none()))
    }
}
