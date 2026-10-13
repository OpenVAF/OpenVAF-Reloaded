//! VAMS-2023 4.5.12: the algebra behind the Z-transform filters.
//!
//! The four forms differ only in how the numerator and the denominator are
//! written. `zi_nd` gives both as coefficients of `z^-k`; `zi_zp` gives both as
//! roots; `zi_zd` and `zi_np` give one of each. Reducing a root list to
//! coefficients therefore reduces all four to the same difference equation, and
//! that is all this module does.
//!
//! The clause states the transfer function of the root forms as a product
//!
//!   H(z) = prod over k of ( 1 - z^-1 (zeta_k^r + j zeta_k^i) )
//!
//! so a root contributes the factor `1 - r z^-1` and a root list is a polynomial
//! in `z^-1` once multiplied out. "If a root is complex, its conjugate shall also
//! be present", which is what keeps those coefficients real.
//!
//! The evaluator here is the oracle the lowering is checked against, not
//! something the compiler calls at run time.

use std::fmt;

/// Why a filter was rejected.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ZiError(pub String);

impl fmt::Display for ZiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, ZiError> {
    Err(ZiError(msg.into()))
}

/// How one side of the transfer function was written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// Coefficients of `z^-k`, as `zi_nd` and the `_d`/`_n` halves give them.
    Coeffs,
    /// Pairs of real and imaginary parts, one pair per root.
    Roots,
}

/// A filter reduced to coefficients of `z^-k`.
#[derive(Clone, PartialEq, Debug)]
pub struct Filter {
    pub num: Vec<f64>,
    pub den: Vec<f64>,
}

/// Multiply out `prod (1 - r_k z^-1)` for the roots in `pairs`, which holds the
/// real and imaginary part of each root in turn.
///
/// The imaginary parts cancel because the conjugates are required to be present,
/// so this checks that they do rather than assuming it: an unpaired complex root
/// would otherwise leave a complex coefficient behind and silently produce a
/// filter nobody wrote.
pub fn roots_to_coeffs(pairs: &[f64], what: &str) -> Result<Vec<f64>, ZiError> {
    if pairs.len() % 2 != 0 {
        return err(format!(
            "the {what} vector holds {} numbers; 4.5.12 gives each root as a pair of a real and \
             an imaginary part",
            pairs.len()
        ));
    }
    // Coefficients as complex numbers, starting from the empty product.
    let mut re = vec![1.0];
    let mut im = vec![0.0];
    let mut scale: f64 = 1.0;
    for root in pairs.chunks_exact(2) {
        let (rr, ri) = (root[0], root[1]);
        scale = scale.max(rr.abs()).max(ri.abs());
        re.push(0.0);
        im.push(0.0);
        // Multiply by (1 - r z^-1): new_k = old_k - r * old_{k-1}.
        for k in (1..re.len()).rev() {
            re[k] -= rr * re[k - 1] - ri * im[k - 1];
            im[k] -= rr * im[k - 1] + ri * re[k - 1];
        }
    }
    // A conjugate-paired product is real. The tolerance rides on the size of the
    // roots so that a filter with large coefficients is not rejected for rounding.
    let tol = 1e-9 * scale.max(1.0).powi(re.len() as i32).max(1.0);
    if let Some(k) = im.iter().position(|v| v.abs() > tol) {
        return err(format!(
            "the {what} vector leaves an imaginary coefficient at z^-{k}; 4.5.12 requires the \
             conjugate of every complex root to be present"
        ));
    }
    Ok(re)
}

impl Filter {
    /// Reduce whichever forms the two sides were written in to coefficients.
    ///
    /// A null `zeros` argument is the empty product, so the numerator is unity:
    /// "The zeros argument may be represented as a null argument."
    pub fn build(
        num: Option<&[f64]>,
        num_side: Side,
        den: &[f64],
        den_side: Side,
    ) -> Result<Filter, ZiError> {
        let num = match num {
            None => vec![1.0],
            Some(num) if num.is_empty() => vec![1.0],
            Some(num) => match num_side {
                Side::Coeffs => num.to_vec(),
                Side::Roots => roots_to_coeffs(num, "zeros")?,
            },
        };
        if den.is_empty() {
            return err("the denominator of a Z-transform filter cannot be empty");
        }
        let den = match den_side {
            Side::Coeffs => den.to_vec(),
            Side::Roots => roots_to_coeffs(den, "poles")?,
        };
        // y[m] is solved for, so the leading denominator coefficient divides.
        if den[0] == 0.0 {
            return err(
                "the z^0 denominator coefficient is zero, so the filter does not determine its \
                 own output"
                    .to_owned(),
            );
        }
        Ok(Filter { num, den })
    }

    /// How many past inputs and past outputs the difference equation needs.
    pub fn order(&self) -> (usize, usize) {
        (self.num.len().saturating_sub(1), self.den.len().saturating_sub(1))
    }

    /// One sample of the difference equation, which is the clause's transfer
    /// function rearranged for the newest output:
    ///
    ///   y[m] = ( sum_k n_k x[m-k] - sum_{k>=1} d_k y[m-k] ) / d_0
    ///
    /// `xs` and `ys` hold the history newest first, `xs[0]` being the sample just
    /// taken. They are only read as far as the filter's own order.
    pub fn step(&self, xs: &[f64], ys: &[f64]) -> f64 {
        let mut acc = 0.0;
        for (k, n) in self.num.iter().enumerate() {
            acc += n * xs.get(k).copied().unwrap_or(0.0);
        }
        for (k, d) in self.den.iter().enumerate().skip(1) {
            acc -= d * ys.get(k - 1).copied().unwrap_or(0.0);
        }
        acc / self.den[0]
    }

    /// Run the filter over a sequence of samples from rest, which is how the
    /// tests and the integration oracle use it.
    pub fn run(&self, input: &[f64]) -> Vec<f64> {
        let (m, n) = self.order();
        let mut xs = vec![0.0; m + 1];
        let mut ys = vec![0.0; n.max(1)];
        let mut out = Vec::with_capacity(input.len());
        for &x in input {
            for k in (1..xs.len()).rev() {
                xs[k] = xs[k - 1];
            }
            xs[0] = x;
            let y = self.step(&xs, &ys);
            for k in (1..ys.len()).rev() {
                ys[k] = ys[k - 1];
            }
            ys[0] = y;
            out.push(y);
        }
        out
    }
}

#[cfg(test)]
mod tests;
