//! The value of a numeric literal.
//!
//! VAMS-2023 2.6.2 lets a real literal carry one of ten scale factor characters
//! (`1n` is `1e-9`). The table is needed in two places: on the syntax tree, where
//! `ast::SiRealNumber::value` reads a literal of a model, and in the
//! preprocessor, which reads the argument of `` `default_transition `` (10.3)
//! before there is a tree to read it from. One table, two callers.

use crate::SyntaxKind;

/// The decimal exponent of a scale factor character, e.g. `n` is `-9`.
///
/// The characters are case sensitive except for `k`: 2.6.2 spells the kilo
/// factor `K` and `M` is mega, not milli.
pub fn scale_factor(c: char) -> Option<i32> {
    let exp = match c {
        'T' => 12,
        'G' => 9,
        'M' => 6,
        'K' | 'k' => 3,
        'm' => -3,
        'u' => -6,
        'n' => -9,
        'p' => -12,
        'f' => -15,
        'a' => -18,
        _ => return None,
    };
    Some(exp)
}

/// The value of a real literal with a scale factor character, such as `1n`.
pub fn si_real_value(text: &str) -> Option<f64> {
    let mut chars = text.chars();
    let exp = scale_factor(chars.next_back()?)?;
    let digits = &text[..text.len() - 1];
    Some(digits.parse::<f64>().ok()? * 10f64.powi(exp))
}

/// The value of any numeric literal token, or `None` if `kind` is not one (or the
/// text is not a literal of that kind).
///
/// An integer literal is included: a transition time of `0` is written without a
/// decimal point, and so is a time in whole seconds.
pub fn number_value(kind: SyntaxKind, text: &str) -> Option<f64> {
    match kind {
        SyntaxKind::INT_NUMBER => text.parse::<i64>().ok().map(|val| val as f64),
        SyntaxKind::STD_REAL_NUMBER => text.parse::<f64>().ok(),
        SyntaxKind::SI_REAL_NUMBER => si_real_value(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn si_literals() {
        assert_eq!(si_real_value("1n"), Some(1e-9));
        assert_eq!(si_real_value("2.5p"), Some(2.5e-12));
        assert_eq!(si_real_value("1.5K"), Some(1500.0));
        assert_eq!(si_real_value("1e3m"), Some(1.0));
        // `M` is mega (2.6.2), not milli
        assert_eq!(si_real_value("1M"), Some(1e6));
        assert_eq!(si_real_value("1x"), None);
    }

    #[test]
    fn numbers() {
        assert_eq!(number_value(SyntaxKind::INT_NUMBER, "0"), Some(0.0));
        assert_eq!(number_value(SyntaxKind::STD_REAL_NUMBER, "1e-9"), Some(1e-9));
        assert_eq!(number_value(SyntaxKind::SI_REAL_NUMBER, "1n"), Some(1e-9));
        assert_eq!(number_value(SyntaxKind::STR_LIT, "\"1n\""), None);
    }
}
