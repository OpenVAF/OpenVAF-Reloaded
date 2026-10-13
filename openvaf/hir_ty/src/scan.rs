//! VAMS-2023 9.5.4: what the conversions of a scan format produce.
//!
//! Shared between type checking and lowering, because the two have to agree about
//! which argument receives which conversion. The format decides that at compile
//! time, which is why a scan's format has to be a literal here: the runtime
//! performs the conversions, and the lowering reads each result back and assigns
//! it, so it has to know in advance what kind of result to expect.

use hir_def::Type;

/// One conversion of a scan format, in the order it appears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanConv {
    /// The conversion character itself, for talking about it: the `e` of `%12e`.
    pub spec: char,
    /// What it produces, which is what gets assigned to the argument.
    pub ty: Type,
}

/// The conversions of `fmt`, in order, one per argument the scan writes to.
///
/// Only the conversions a data file is made of are recognised, which is the same
/// set the runtime converts. Anything else stops the scan there, so it takes no
/// argument either.
pub fn scan_conversions(fmt: &str) -> Vec<ScanConv> {
    let mut out = Vec::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            continue;
        }
        let mut c = match chars.next() {
            Some(c) => c,
            None => break,
        };
        // A field width belongs to the conversion, not to the count.
        while c.is_ascii_digit() {
            c = match chars.next() {
                Some(c) => c,
                None => return out,
            };
        }
        let ty = match c {
            '%' => continue,
            'd' | 'D' => Type::Integer,
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'r' | 'R' => Type::Real,
            's' | 'S' => Type::String,
            _ => return out,
        };
        out.push(ScanConv { spec: c, ty });
    }
    out
}
