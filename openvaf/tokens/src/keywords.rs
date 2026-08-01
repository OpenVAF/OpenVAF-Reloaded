//! Selectable sets of reserved keywords.
//!
//! Verilog-AMS `` `begin_keywords ``/`` `end_keywords `` (VAMS-2023 10.6, Mantis
//! 7921) let a source file pick which identifiers are reserved as keywords. The
//! directives change *only* the set of reserved words; they do not change the
//! grammar, the token kinds or any other aspect of the language.
//!
//! The interesting direction is *releasing* words: every set the standard
//! defines is a subset of what OpenVAF reserves by default, so
//! [`KeywordSet::reserves`] answers "does the selected set still reserve this
//! word that OpenVAF reserves by default". For the Verilog-AMS specifiers that
//! is always the case, which leaves the IEEE Std 1364 keyword lists as the only
//! table this module has to carry.

use crate::parser::SyntaxKind;

/// A set of reserved keywords, selected by a `` `begin_keywords `` version
/// specifier.
///
/// Ordered by inclusion: every set reserves everything the sets before it
/// reserve.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub enum KeywordSet {
    /// `"1364-1995"` — IEEE Std 1364-1995 Verilog.
    Verilog1995,
    /// `"1364-2001"` — IEEE Std 1364-2001 Verilog.
    Verilog2001,
    /// `"1364-2005"` — IEEE Std 1364-2005 Verilog.
    Verilog2005,
    /// `"VAMS-2.3"` — Verilog-AMS 2.3, Annex B.
    Vams23,
    /// `"VAMS-2023"` — Verilog-AMS 2023, Annex B.
    Vams2023,
    /// The set used when no `` `begin_keywords `` directive is in effect: the
    /// Verilog-AMS keywords plus the extensions OpenVAF recognises.
    #[default]
    OpenVaf,
}

impl KeywordSet {
    /// Maps a `` `begin_keywords `` version specifier (without the surrounding
    /// quotes) onto the keyword set it selects.
    ///
    /// VAMS-2023 10.6 requires `"1364-1995"`, `"1364-2001"`, `"1364-2005"`,
    /// `"VAMS-2.3"` and `"VAMS-2023"` to be supported; no other specifier is
    /// defined by the standard.
    pub fn from_version_specifier(specifier: &str) -> Option<KeywordSet> {
        let set = match specifier {
            "1364-1995" => KeywordSet::Verilog1995,
            "1364-2001" => KeywordSet::Verilog2001,
            "1364-2005" => KeywordSet::Verilog2005,
            "VAMS-2.3" => KeywordSet::Vams23,
            "VAMS-2023" => KeywordSet::Vams2023,
            _ => return None,
        };
        Some(set)
    }

    /// The specifiers accepted by [`KeywordSet::from_version_specifier`], for
    /// use in diagnostics.
    pub const VERSION_SPECIFIERS: &'static [&'static str] =
        &["1364-1995", "1364-2001", "1364-2005", "VAMS-2.3", "VAMS-2023"];

    /// Whether this set is one of the plain Verilog specifiers, which do not
    /// reserve any of the Verilog-AMS keywords.
    pub fn is_verilog(self) -> bool {
        self <= KeywordSet::Verilog2005
    }

    /// Whether `ident` is reserved in this set.
    ///
    /// This is only meaningful for words that OpenVAF reserves in the first
    /// place: it answers "does the selected set still reserve this", never
    /// "does the selected set additionally reserve this". Words that OpenVAF
    /// does not reserve by default (`sin`, `abs`, ... in expression position,
    /// which are shadowable builtins) stay unreserved in every set.
    pub fn reserves(self, ident: &str) -> bool {
        if !self.is_verilog() {
            // Verilog-AMS 2.3 and 2023 reserve everything OpenVAF reserves:
            // none of the words added by VAMS-2023 over 2.3 (`break`,
            // `continue`, `return`, `expm1`, `ln1p`, ...) are reserved words in
            // OpenVAF, and OpenVAF's own extensions stay reserved so that
            // opting into a Verilog-AMS specifier never disables a language
            // feature.
            return true;
        }
        verilog_keyword_since(ident).is_some_and(|since| since <= self)
    }
}

/// The earliest IEEE Std 1364 revision that reserves `ident`, or `None` if
/// `ident` is not a Verilog keyword at all (i.e. it is a Verilog-AMS keyword or
/// an OpenVAF extension).
fn verilog_keyword_since(ident: &str) -> Option<KeywordSet> {
    use KeywordSet::{Verilog1995, Verilog2001, Verilog2005};
    let since = match ident {
        // IEEE Std 1364-1995, Table 3-1
        "always" | "and" | "assign" | "begin" | "buf" | "bufif0" | "bufif1" | "case" | "casex"
        | "casez" | "cmos" | "deassign" | "default" | "defparam" | "disable" | "edge" | "else"
        | "end" | "endcase" | "endfunction" | "endmodule" | "endprimitive" | "endspecify"
        | "endtable" | "endtask" | "event" | "for" | "force" | "forever" | "fork" | "function"
        | "highz0" | "highz1" | "if" | "ifnone" | "initial" | "inout" | "input" | "integer"
        | "join" | "large" | "macromodule" | "medium" | "module" | "nand" | "negedge" | "nmos"
        | "nor" | "not" | "notif0" | "notif1" | "or" | "output" | "parameter" | "pmos"
        | "posedge" | "primitive" | "pull0" | "pull1" | "pulldown" | "pullup" | "rcmos"
        | "real" | "realtime" | "reg" | "release" | "repeat" | "rnmos" | "rpmos" | "rtran"
        | "rtranif0" | "rtranif1" | "scalared" | "small" | "specify" | "specparam" | "strong0"
        | "strong1" | "supply0" | "supply1" | "table" | "task" | "time" | "tran" | "tranif0"
        | "tranif1" | "tri" | "tri0" | "tri1" | "triand" | "trior" | "trireg" | "vectored"
        | "wait" | "wand" | "weak0" | "weak1" | "while" | "wire" | "wor" | "xnor" | "xor" => {
            Verilog1995
        }

        // added by IEEE Std 1364-2001
        "automatic"
        | "cell"
        | "config"
        | "design"
        | "endconfig"
        | "endgenerate"
        | "generate"
        | "genvar"
        | "incdir"
        | "include"
        | "instance"
        | "liblist"
        | "library"
        | "localparam"
        | "noshowcancelled"
        | "pulsestyle_ondetect"
        | "pulsestyle_onevent"
        | "showcancelled"
        | "signed"
        | "unsigned"
        | "use" => Verilog2001,

        // added by IEEE Std 1364-2005
        "uwire" => Verilog2005,

        _ => return None,
    };
    Some(since)
}

/// [`SyntaxKind::from_keyword`], restricted to the keywords `set` reserves.
///
/// Identifiers that are keywords in OpenVAF's default set but not in `set` lex
/// as plain [`SyntaxKind::IDENT`].
pub fn from_keyword_in(ident: &str, set: KeywordSet) -> Option<SyntaxKind> {
    let kind = SyntaxKind::from_keyword(ident)?;
    set.reserves(ident).then_some(kind)
}

#[cfg(test)]
mod tests {
    use super::{from_keyword_in, KeywordSet};
    use crate::parser::SyntaxKind;

    #[test]
    fn verilog_sets_release_verilog_ams_keywords() {
        for kw in ["analog", "string", "ground", "discipline", "from", "inf", "aliasparam"] {
            assert_eq!(from_keyword_in(kw, KeywordSet::Verilog2005), None, "{kw}");
            assert_eq!(
                from_keyword_in(kw, KeywordSet::Vams23),
                SyntaxKind::from_keyword(kw),
                "{kw}"
            );
        }
    }

    #[test]
    fn verilog_revisions_release_later_keywords() {
        assert_eq!(from_keyword_in("localparam", KeywordSet::Verilog1995), None);
        assert_eq!(
            from_keyword_in("localparam", KeywordSet::Verilog2001),
            Some(SyntaxKind::LOCALPARAM_KW)
        );
        assert_eq!(from_keyword_in("genvar", KeywordSet::Verilog1995), None);
        assert_eq!(from_keyword_in("genvar", KeywordSet::Verilog2001), Some(SyntaxKind::GENVAR_KW));
        assert_eq!(from_keyword_in("uwire", KeywordSet::Verilog2001), None);
        assert_eq!(from_keyword_in("uwire", KeywordSet::Verilog2005), Some(SyntaxKind::NET_TYPE));
    }

    #[test]
    fn core_keywords_are_reserved_everywhere() {
        for kw in ["module", "endmodule", "begin", "end", "input", "real", "parameter", "wire"] {
            for set in [
                KeywordSet::Verilog1995,
                KeywordSet::Verilog2005,
                KeywordSet::Vams2023,
                KeywordSet::OpenVaf,
            ] {
                assert_eq!(from_keyword_in(kw, set), SyntaxKind::from_keyword(kw), "{kw} {set:?}");
            }
        }
    }

    #[test]
    fn unreserved_words_stay_unreserved() {
        // shadowable builtins are not keywords in OpenVAF, in any set
        for set in [KeywordSet::Verilog1995, KeywordSet::Vams2023, KeywordSet::OpenVaf] {
            assert_eq!(from_keyword_in("sin", set), None);
            assert_eq!(from_keyword_in("expm1", set), None);
        }
    }

    #[test]
    fn version_specifiers_round_trip() {
        for specifier in KeywordSet::VERSION_SPECIFIERS {
            assert!(KeywordSet::from_version_specifier(specifier).is_some(), "{specifier}");
        }
        assert_eq!(KeywordSet::from_version_specifier("VAMS-2.4"), None);
        assert_eq!(KeywordSet::from_version_specifier(""), None);
    }
}
