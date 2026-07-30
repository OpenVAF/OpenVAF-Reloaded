pub mod keywords;
pub mod lexer;
pub mod parser;

pub use keywords::KeywordSet;
use lexer::LiteralKind;
use lexer::TokenKind::*;
pub use parser::SyntaxKind;

pub enum LexerErrorKind {
    UnterminatedBlockComment,
    UnterminatedStr,
    UnexpectedToken,
}

impl lexer::TokenKind {
    /// Converts a lexer token into a syntax token.
    ///
    /// `keywords` selects which identifiers are reserved words; it is changed by
    /// the `` `begin_keywords ``/`` `end_keywords `` directives and is
    /// [`KeywordSet::OpenVaf`] unless one is in effect.
    pub fn to_syntax(
        self,
        src: &str,
        keywords: KeywordSet,
    ) -> (Option<parser::SyntaxKind>, Option<LexerErrorKind>) {
        let token = match self {
            // Combined operators
            LineComment | BlockComment { terminated: true } => SyntaxKind::COMMENT,
            BlockComment { terminated: false } => {
                return (Some(SyntaxKind::COMMENT), Some(LexerErrorKind::UnterminatedBlockComment))
            }
            Whitespace => SyntaxKind::WHITESPACE,
            SimpleIdent => keywords::from_keyword_in(src, keywords).unwrap_or(SyntaxKind::IDENT),
            EscapedIdent => SyntaxKind::IDENT,
            SystemCallIdent if src == "$root" => SyntaxKind::ROOT_KW,
            SystemCallIdent => SyntaxKind::SYSFUN,
            Literal { kind: LiteralKind::Int } => SyntaxKind::INT_NUMBER,
            Literal { kind: LiteralKind::Float { has_scale_char: true } } => {
                SyntaxKind::SI_REAL_NUMBER
            }
            Literal { kind: LiteralKind::Float { has_scale_char: false } } => {
                SyntaxKind::STD_REAL_NUMBER
            }
            Literal { kind: LiteralKind::Str { terminated: true } } => SyntaxKind::STR_LIT,
            Literal { kind: LiteralKind::Str { terminated: false } } => {
                return (Some(SyntaxKind::STR_LIT), Some(LexerErrorKind::UnterminatedStr))
            }
            CompilerDirective | Define { .. } | IllegalDefine => return (None, None),
            Semi => T![;],
            Comma => T![,],
            Dot => T![.],
            OpenParen => T!['('],
            CloseParen => T![')'],
            OpenBrace => T!['{'],
            CloseBrace => T!['}'],
            OpenBracket => T!['['],
            CloseBracket => T![']'],
            At => T![@],
            Pound => T![#],
            Tilde => T![~],
            Question => T![?],
            Colon => T![:],
            Dollar => T![$],
            Eq => T![=],
            Not => T![!],
            Lt => T![<],
            Gt => T![>],
            Minus => T![-],
            And => T![&],
            Or => T![|],
            Plus => T![+],
            Star => T![*],
            Slash => T![/],
            Caret => T![^],
            Percent => T![%],
            AttrOpenParen => T!["(*"],
            AttrCloseParen => T!["*)"],
            ArrStart => T!["'{"],
            Eq2 => T![==],
            Neq => T![!=],
            Leq => T![<=],
            Geq => T![>=],
            Pipe2 => T![||],
            Amp2 => T![&&],
            Shl => T![<<],
            Shr => T![>>],
            ShlA => T![<<<],
            ShrA => T![>>>],
            Contribute => T![<+],
            Trigger => T![->],
            Pow => T![**],
            NXorL => T![~^],
            NXorR => T![^~],

            Unknown => return (None, Some(LexerErrorKind::UnexpectedToken)),
        };

        (Some(token), None)
    }
}
