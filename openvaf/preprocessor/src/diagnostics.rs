use std::io;

use stdx::impl_display;
use vfs::{InvalidTextFormatErr, VfsPath};

use crate::sourcemap::CtxSpan;

#[derive(Debug, PartialEq, Clone, Eq)]
pub enum PreprocessorDiagnostic {
    MacroArgumentCountMismatch { expected: usize, found: usize, span: CtxSpan },
    MacroNotFound { name: String, span: CtxSpan },
    MacroNotDefined { name: String, span: CtxSpan },
    MacroRecursion { name: String, span: CtxSpan },
    UnsupportedCompDir { name: String, span: CtxSpan },
    FileNotFound { file: String, error: io::ErrorKind, span: Option<CtxSpan> },
    InvalidTextFormat { span: Option<CtxSpan>, file: VfsPath, err: InvalidTextFormatErr },
    UnexpectedEof { expected: &'static str, span: CtxSpan },
    MissingOrUnexpectedToken { expected: &'static str, expected_at: CtxSpan, span: CtxSpan },
    UnexpectedToken(CtxSpan),
    MacroOverwritten { old: CtxSpan, new: CtxSpan, name: String },
    // `begin_keywords / `end_keywords (VAMS-2023 10.6)
    UnknownKeywordVersion { version: String, span: CtxSpan },
    UnmatchedEndKeywords { span: CtxSpan },
    UnterminatedKeywords { span: CtxSpan },
    KeywordsInDesignElement { name: &'static str, span: CtxSpan },
}

use PreprocessorDiagnostic::*;
impl_display! {
    match PreprocessorDiagnostic{
        MacroArgumentCountMismatch { expected, found, ..} => "argument mismatch expected {} but found {}!", expected, found;
        MacroNotFound{name,..} =>  "macro '`{}' has not been declared", name;
        MacroNotDefined{name,..} =>  "cannot undefine macro '`{}'", name;
        MacroRecursion { name,..} => "macro '`{}' was called recursively",name;
        UnsupportedCompDir { name,.. } => "unsupported compiler directive {}",name;
        FileNotFound { file, error, .. } => "failed to read '{}': {}", file, std::io::Error::from(*error);
        InvalidTextFormat {  file, ..} => "failed to read {}: file contents are not valid text", file;
        UnexpectedEof { expected ,..} => "unexpected EOF, expected {}",expected;
        MissingOrUnexpectedToken { expected, ..} => "unexpected token, expected '{}'", expected;
        UnexpectedToken(_) => "encountered unexpected token!";
        MacroOverwritten { name, .. } => "macro '`{}' was overwritten", name;
        UnknownKeywordVersion { version, .. } => "unknown keyword version specifier \"{}\"", version;
        UnmatchedEndKeywords { .. } => "'`end_keywords' without a matching '`begin_keywords'";
        UnterminatedKeywords { .. } => "'`begin_keywords' without a matching '`end_keywords'";
        KeywordsInDesignElement { name, .. } => "'`{}' is not allowed inside a design element", name;
    }
}
