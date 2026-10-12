//! VAMS-2023 9.21: the data model behind `$table_model`.
//!
//! The clause describes samples of an N-dimensional function arranged on
//! *isolines*: the independent variables are swept from the outermost (slowest
//! changing) to the innermost (fastest changing), and each isoline repeats its
//! ordinate once per sample. Isolines are ragged on purpose, "each isoline may
//! exist over a different range of x values and the number and spacing of
//! samples may be different on each isoline", so the data is a tree rather than
//! a rectangular array.
//!
//! A lookup is the recursive process 9.21 defines: bracket the outermost
//! ordinate, interpolate each bracketing isoline at the inner coordinates to
//! produce a one-dimensional set, then interpolate that. Every interpolation is
//! therefore one-dimensional, which is why the schemes are specified per
//! dimension.
//!
//! This module parses the control string and the data source, builds the tree,
//! and evaluates it. It holds no reference to the compiler: the lowering walks
//! the same tree to emit code, and these `eval` routines are the oracle the
//! emitted code is checked against.

use std::fmt;

/// Table 9-30, the interpolation control character.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Interp {
    /// `I`: ignore this input column.
    Ignore,
    /// `D`: closest point (discrete) lookup.
    Discrete,
    /// `1`: linear interpolation, the default.
    Linear,
    /// `2`: quadratic spline interpolation.
    Quadratic,
    /// `3`: cubic spline interpolation.
    Cubic,
}

/// Table 9-31, the extrapolation control character.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Extrap {
    /// `C`: return the table endpoint value.
    Constant,
    /// `L`: extend linearly at a slope consistent with the interpolation
    /// method. The default.
    Linear,
    /// `E`: report a fatal error if a lookup falls outside the table.
    Error,
}

/// The control for one dimension: how to interpolate within it, and how to
/// extrapolate off each of its two ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DimControl {
    pub interp: Interp,
    pub lo: Extrap,
    pub hi: Extrap,
}

impl Default for DimControl {
    /// "When no extrapolation method character is given, the linear
    /// extrapolation method will be used for both ends as default", and `1` is
    /// the default interpolation character.
    fn default() -> Self {
        DimControl { interp: Interp::Linear, lo: Extrap::Linear, hi: Extrap::Linear }
    }
}

/// A parsed `table_control_string`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Control {
    /// One entry per leading column of the data source, `Ignore` included, in
    /// column order. The non-ignored entries are the independent dimensions,
    /// outermost first.
    pub columns: Vec<DimControl>,
    /// The `dependent_selector`, 1-based, "This number runs 1 though M".
    pub dependent: usize,
}

/// Why a control string, a data source or a lookup was rejected.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TableError(pub String);

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, TableError> {
    Err(TableError(msg.into()))
}

impl Control {
    /// Parse `"[interp_control[;dependent_selector]]"`.
    ///
    /// `inputs` is the number of lookup expressions the call was written with,
    /// which fixes the dimensionality when the control string does not:
    /// "Null string, default linear interpolation and extrapolation.
    /// Dimensionality of the data is assumed to be N."
    pub fn parse(s: &str, inputs: usize) -> Result<Control, TableError> {
        let (interp, dependent) = match s.split_once(';') {
            Some((i, d)) => {
                let d = d.trim();
                let dependent: usize = match d.parse() {
                    Ok(n) if n >= 1 => n,
                    _ => {
                        return err(format!(
                            "'{d}' is not a dependent variable selector; 9.21.2 runs them from 1"
                        ))
                    }
                };
                (i, dependent)
            }
            // "This is the default behavior when there are multiple dependent
            // variables in the file and there is no dependent variable selector".
            None => (s, 1),
        };

        if interp.trim().is_empty() {
            return Ok(Control { columns: vec![DimControl::default(); inputs], dependent });
        }

        let mut columns = Vec::new();
        for substr in interp.split(',') {
            columns.push(Self::parse_substr(substr.trim())?);
        }

        let independent = columns.iter().filter(|c| c.interp != Interp::Ignore).count();
        if independent != inputs {
            return err(format!(
                "the control string describes {independent} independent dimension(s) but the call \
                 passes {inputs} lookup expression(s)"
            ));
        }
        if independent == 0 {
            return err("a table model needs at least one independent dimension");
        }

        Ok(Control { columns, dependent })
    }

    /// One `table_ctrl_substr`: `[table_interp_char][table_extrap_char
    /// [higher_table_extrap_char]]`.
    fn parse_substr(s: &str) -> Result<DimControl, TableError> {
        // A null substring takes every default, which is how 9.21.3's "C,,3"
        // means "1CC, 1LL, 3LL".
        let mut chars = s.chars();
        let mut out = DimControl::default();

        let mut next = chars.next();
        if let Some(c) = next {
            let interp = match c {
                'I' => Some(Interp::Ignore),
                'D' => Some(Interp::Discrete),
                '1' => Some(Interp::Linear),
                '2' => Some(Interp::Quadratic),
                '3' => Some(Interp::Cubic),
                _ => None,
            };
            if let Some(interp) = interp {
                out.interp = interp;
                next = chars.next();
            }
        }

        let extrap = |c: char| match c {
            'C' => Some(Extrap::Constant),
            'L' => Some(Extrap::Linear),
            'E' => Some(Extrap::Error),
            _ => None,
        };

        if let Some(c) = next {
            match extrap(c) {
                // "When one extrapolation method character is given, the
                // specified extrapolation method will be used for both ends."
                Some(e) => {
                    out.lo = e;
                    out.hi = e;
                }
                None => {
                    return err(format!("'{c}' is not an interpolation or extrapolation character"))
                }
            }
            if let Some(c) = chars.next() {
                match extrap(c) {
                    // "the first character specifies the extrapolation method used
                    // for the end with the lower coordinate value, and the second
                    // character is used for the end with the higher".
                    Some(e) => out.hi = e,
                    None => return err(format!("'{c}' is not an extrapolation character")),
                }
            }
            if let Some(c) = chars.next() {
                return err(format!(
                    "'{c}': a control substring holds at most an interpolation character and two \
                     extrapolation characters"
                ));
            }
        }

        Ok(out)
    }

    /// The independent dimensions, outermost first, with the ignored columns
    /// dropped.
    pub fn dims(&self) -> Vec<DimControl> {
        self.columns.iter().copied().filter(|c| c.interp != Interp::Ignore).collect()
    }

    /// The 0-based data column each independent dimension reads, outermost first.
    fn indep_columns(&self) -> Vec<usize> {
        self.columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.interp != Interp::Ignore)
            .map(|(i, _)| i)
            .collect()
    }

    /// The 0-based data column the selected dependent variable reads. The
    /// dependents follow every column the control string describes, ignored ones
    /// included: 9.21.3's "I,1CC,1CC;3" reaches column 6 of at least 6.
    fn dep_column(&self) -> usize {
        self.columns.len() + self.dependent - 1
    }
}

/// Rows of a data source, as read before the control string assigns roles to the
/// columns.
#[derive(Clone, PartialEq, Debug)]
pub struct Rows {
    pub cols: usize,
    pub rows: Vec<Vec<f64>>,
}

impl Rows {
    /// 9.21.1: "Each sample point is separated by a newline and each column is
    /// separated by one or more spaces or tabs. Comments begin with '#' and
    /// continue to the end of that line. They may appear anywhere in the file.
    /// Blank lines are ignored. The numbers shall be real or integer."
    pub fn parse(text: &str) -> Result<Rows, TableError> {
        let mut rows: Vec<Vec<f64>> = Vec::new();
        for (no, line) in text.lines().enumerate() {
            let line = match line.split_once('#') {
                Some((before, _)) => before,
                None => line,
            };
            if line.trim().is_empty() {
                continue;
            }
            let mut row = Vec::new();
            for field in line.split_whitespace() {
                match field.parse::<f64>() {
                    Ok(v) if v.is_finite() => row.push(v),
                    _ => {
                        return err(format!(
                            "line {}: '{field}' is not a real or integer number",
                            no + 1
                        ))
                    }
                }
            }
            if let Some(first) = rows.first() {
                if row.len() != first.len() {
                    return err(format!(
                        "line {}: {} columns, but the table started with {}; 9.21.1 keeps the \
                         rows at one width",
                        no + 1,
                        row.len(),
                        first.len()
                    ));
                }
            }
            rows.push(row);
        }
        match rows.first() {
            Some(first) => Ok(Rows { cols: first.len(), rows }),
            None => err("the table holds no data points"),
        }
    }

    /// A data source given as columns of equal length, which is how the array
    /// form arrives: "the isolines are laid out in conceptually the same way with
    /// each array being just as a column in the file format".
    pub fn from_columns(columns: &[Vec<f64>]) -> Result<Rows, TableError> {
        let cols = columns.len();
        if cols == 0 {
            return err("the table holds no columns");
        }
        let len = columns[0].len();
        for (i, c) in columns.iter().enumerate() {
            if c.len() != len {
                return err(format!(
                    "array {} holds {} points but array 1 holds {len}; the columns of a table \
                     model are sampled together",
                    i + 1,
                    c.len()
                ));
            }
        }
        if len == 0 {
            return err("the table holds no data points");
        }
        let rows = (0..len).map(|r| columns.iter().map(|c| c[r]).collect()).collect();
        Ok(Rows { cols, rows })
    }
}

/// A node of the isoline tree. A branch holds this dimension's ordinates in
/// ascending order, each with the sub-table sampled at it; a leaf holds a
/// sampled value of the dependent variable.
#[derive(Clone, PartialEq, Debug)]
pub enum Node {
    Branch(Vec<(f64, Node)>),
    Leaf(f64),
}

impl Node {
    #[cfg(test)]
    fn len(&self) -> usize {
        match self {
            Node::Branch(kids) => kids.len(),
            Node::Leaf(_) => 1,
        }
    }
}

/// A table ready to look up: the per-dimension control, outermost first, and the
/// isoline tree.
#[derive(Clone, PartialEq, Debug)]
pub struct Table {
    pub dims: Vec<DimControl>,
    pub root: Node,
}

impl Table {
    /// Assign roles to the columns, group the rows into isolines and sort them.
    ///
    /// 9.21.1: "While it is suggested that the user arrange the sampled isolines
    /// in sorted order [...] if the user provides the data in random order the
    /// system will sort the data into isolines in each dimension. Whether the
    /// data is sorted or not, the system determines the isoline ordinate by
    /// reading its exact value from the file or array."
    ///
    /// Returns the table and any warnings worth passing to the user.
    pub fn build(rows: &Rows, control: &Control) -> Result<(Table, Vec<String>), TableError> {
        let dims = control.dims();
        let indep = control.indep_columns();
        let dep = control.dep_column();

        if rows.cols <= dep {
            return err(format!(
                "the control string reaches column {} but the data source has {}",
                dep + 1,
                rows.cols
            ));
        }

        // Collect (independent coordinates, dependent value) and look for the two
        // kinds of duplicate 9.21 distinguishes.
        let mut points: Vec<(Vec<f64>, f64)> =
            rows.rows.iter().map(|r| (indep.iter().map(|&c| r[c]).collect(), r[dep])).collect();

        let mut warnings = Vec::new();
        let mut seen: Vec<(Vec<f64>, f64)> = Vec::with_capacity(points.len());
        let mut dropped = 0usize;
        for (coords, value) in points.drain(..) {
            match seen.iter().find(|(c, _)| *c == coords) {
                // "If there are two or more data points with the same independent
                // values but different dependent values then an error is generated."
                Some((_, prev)) if *prev != value => {
                    return err(format!(
                        "two samples at {} disagree: {prev} and {value}",
                        fmt_coords(&coords)
                    ))
                }
                // "If there are two or more data points with the same independent
                // and dependent values, then the duplicates shall be ignored and
                // the tool may generate a warning."
                Some(_) => dropped += 1,
                None => seen.push((coords, value)),
            }
        }
        if dropped != 0 {
            warnings.push(format!("ignored {dropped} duplicate sample(s)"));
        }

        let root = build_node(&mut seen, 0, dims.len())?;
        check_shape(&root, &dims, 0)?;
        Ok((Table { dims, root }, warnings))
    }

    /// Look up `inputs`, outermost coordinate first.
    pub fn eval(&self, inputs: &[f64]) -> Result<f64, TableError> {
        if inputs.len() != self.dims.len() {
            return err(format!(
                "{} lookup coordinate(s) for a {}-dimensional table",
                inputs.len(),
                self.dims.len()
            ));
        }
        eval_node(&self.root, &self.dims, inputs)
    }
}

fn fmt_coords(coords: &[f64]) -> String {
    let inner = coords.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ");
    format!("({inner})")
}

/// Group the remaining points by their `depth`-th coordinate, recursively.
fn build_node(
    points: &mut [(Vec<f64>, f64)],
    depth: usize,
    ndims: usize,
) -> Result<Node, TableError> {
    if depth == ndims {
        debug_assert_eq!(points.len(), 1);
        return Ok(Node::Leaf(points[0].1));
    }

    // Sorting by the ordinate is what turns an unordered data source into
    // isolines. `total_cmp` rather than `partial_cmp` because the ordinate is read
    // as an exact value and must order even if a source holds a signed zero.
    points.sort_by(|a, b| a.0[depth].total_cmp(&b.0[depth]));

    let mut kids = Vec::new();
    let mut start = 0;
    while start < points.len() {
        let ordinate = points[start].0[depth];
        let mut end = start;
        while end < points.len() && points[end].0[depth] == ordinate {
            end += 1;
        }
        let node = build_node(&mut points[start..end], depth + 1, ndims)?;
        kids.push((ordinate, node));
        start = end;
    }
    Ok(Node::Branch(kids))
}

/// 9.21: "The minimum data requirement is to have the product of at least two
/// points per dimension (2^N for N dimensions). In addition, the result of the
/// bracketing to produce intermediate points [...] must also produce at least two
/// points per subsequent lower dimension."
///
/// A discrete dimension selects one sample rather than bracketing two, so it
/// needs only one point; the clause's requirement follows from interpolating.
fn check_shape(node: &Node, dims: &[DimControl], depth: usize) -> Result<(), TableError> {
    let kids = match node {
        Node::Branch(kids) => kids,
        Node::Leaf(_) => return Ok(()),
    };
    let needed = match dims[depth].interp {
        Interp::Discrete => 1,
        Interp::Ignore => unreachable!("ignored columns are not dimensions"),
        Interp::Linear => 2,
        // A spline is built over a whole isoline; two points make it a line, which
        // is degenerate but well defined, so this is the same floor as linear.
        Interp::Quadratic | Interp::Cubic => 2,
    };
    if kids.len() < needed {
        return err(format!(
            "dimension {} has an isoline of {} point(s) but its interpolation needs {needed}",
            dims.len() - depth,
            kids.len()
        ));
    }
    for (_, kid) in kids {
        check_shape(kid, dims, depth + 1)?;
    }
    Ok(())
}

fn eval_node(node: &Node, dims: &[DimControl], inputs: &[f64]) -> Result<f64, TableError> {
    let kids = match node {
        Node::Leaf(v) => return Ok(*v),
        Node::Branch(kids) => kids,
    };
    let ctrl = dims[0];
    let x = inputs[0];
    let xs: Vec<f64> = kids.iter().map(|(o, _)| *o).collect();

    // "The closest point lookup algorithm returns the closest point in the
    // specified dimension. When the lookup ordinate is equidistant from two
    // bracketing samples the function snaps away from zero."
    if ctrl.interp == Interp::Discrete {
        let i = closest(&xs, x);
        return eval_node(&kids[i].1, &dims[1..], &inputs[1..]);
    }

    // Only a spline needs the whole isoline; linear and the extrapolations reach
    // for two points, so evaluating lazily keeps an `E` end in an inner dimension
    // from firing for a sub-table this lookup never needed.
    let at = |i: usize| eval_node(&kids[i].1, &dims[1..], &inputs[1..]);
    let n = kids.len();

    match ctrl.interp {
        Interp::Linear => {
            // Each arm reaches only for the samples it needs, so a `C` end does
            // not evaluate the neighbour it will not use and an `E` end deeper in
            // the table does not fire for a sub-table this lookup never wanted.
            if x < xs[0] {
                return match ctrl.lo {
                    Extrap::Constant => at(0),
                    Extrap::Linear => Ok(lerp(xs[0], at(0)?, xs[1], at(1)?, x)),
                    Extrap::Error => outside(x, dims.len()),
                };
            }
            if x > xs[n - 1] {
                return match ctrl.hi {
                    Extrap::Constant => at(n - 1),
                    Extrap::Linear => Ok(lerp(xs[n - 1], at(n - 1)?, xs[n - 2], at(n - 2)?, x)),
                    Extrap::Error => outside(x, dims.len()),
                };
            }
            let (lo, hi, _) = bracket(&xs, x);
            Ok(lerp(xs[lo], at(lo)?, xs[hi], at(hi)?, x))
        }
        Interp::Quadratic | Interp::Cubic => {
            let mut ys = Vec::with_capacity(n);
            for i in 0..n {
                ys.push(at(i)?);
            }
            let spline = if ctrl.interp == Interp::Cubic {
                Spline::cubic(&xs, &ys, ctrl)
            } else {
                Spline::quadratic(&xs, &ys, ctrl)
            };
            spline.eval(x, ctrl, dims.len())
        }
        Interp::Discrete | Interp::Ignore => unreachable!("handled above"),
    }
}

/// The index of the sample closest to `x`, snapping away from zero on a tie.
fn closest(xs: &[f64], x: f64) -> usize {
    let mut best = 0;
    for i in 1..xs.len() {
        let d = (xs[i] - x).abs();
        let b = (xs[best] - x).abs();
        if d < b {
            best = i;
        } else if d == b {
            // Away from zero: the candidate wins when it is further from the
            // origin than the one held.
            if xs[i].abs() > xs[best].abs() {
                best = i;
            }
        }
    }
    best
}

/// The pair of samples bracketing `x`, and whether `x` fell off an end.
fn bracket(xs: &[f64], x: f64) -> (usize, usize, bool) {
    let n = xs.len();
    if x < xs[0] {
        return (0, 1, true);
    }
    if x > xs[n - 1] {
        return (n - 2, n - 1, true);
    }
    let mut hi = 1;
    while hi < n - 1 && xs[hi] < x {
        hi += 1;
    }
    (hi - 1, hi, false)
}

fn lerp(x0: f64, y0: f64, x1: f64, y1: f64, x: f64) -> f64 {
    if x1 == x0 {
        return y0;
    }
    y0 + (y1 - y0) * (x - x0) / (x1 - x0)
}

/// The `E` extrapolation method: "an extrapolation error is reported if the
/// $table_model function is requested to evaluate a point beyond the
/// interpolation region".
fn outside(x: f64, dim: usize) -> Result<f64, TableError> {
    err(format!(
        "lookup at {x} falls outside dimension {dim} of the table, whose control string asks for \
         an error there"
    ))
}

/// A one-dimensional spline over a whole isoline.
struct Spline {
    xs: Vec<f64>,
    ys: Vec<f64>,
    /// The first derivative at each knot, which is what both spline kinds reduce
    /// to once their system is solved.
    d: Vec<f64>,
    cubic: bool,
}

impl Spline {
    /// A cubic spline whose end conditions come from the extrapolation choice.
    ///
    /// 9.21.4: "If the user selects linear extrapolation this leads to a natural
    /// spline. If constant extrapolation is specified the end point derivative is
    /// set to zero thus avoiding a discontinuity in the first order derivative at
    /// that end point."
    fn cubic(xs: &[f64], ys: &[f64], ctrl: DimControl) -> Spline {
        let n = xs.len();
        let mut d = vec![0.0; n];
        if n == 2 {
            let s = (ys[1] - ys[0]) / (xs[1] - xs[0]);
            d = vec![s, s];
            if ctrl.lo == Extrap::Constant {
                d[0] = 0.0;
            }
            if ctrl.hi == Extrap::Constant {
                d[1] = 0.0;
            }
            return Spline { xs: xs.to_vec(), ys: ys.to_vec(), d, cubic: true };
        }

        // Solve the tridiagonal system for the second derivatives, then read the
        // first derivatives off them.
        let h: Vec<f64> = (0..n - 1).map(|i| xs[i + 1] - xs[i]).collect();
        let slope: Vec<f64> = (0..n - 1).map(|i| (ys[i + 1] - ys[i]) / h[i]).collect();
        let mut a = vec![0.0; n];
        let mut b = vec![0.0; n];
        let mut c = vec![0.0; n];
        let mut r = vec![0.0; n];

        // Lower end.
        match ctrl.lo {
            // Natural: the second derivative is zero.
            Extrap::Linear | Extrap::Error => {
                b[0] = 1.0;
                c[0] = 0.0;
                r[0] = 0.0;
            }
            // Clamped at zero slope.
            Extrap::Constant => {
                b[0] = 2.0 * h[0];
                c[0] = h[0];
                r[0] = 6.0 * slope[0];
            }
        }
        for i in 1..n - 1 {
            a[i] = h[i - 1];
            b[i] = 2.0 * (h[i - 1] + h[i]);
            c[i] = h[i];
            r[i] = 6.0 * (slope[i] - slope[i - 1]);
        }
        match ctrl.hi {
            Extrap::Linear | Extrap::Error => {
                a[n - 1] = 0.0;
                b[n - 1] = 1.0;
                r[n - 1] = 0.0;
            }
            Extrap::Constant => {
                a[n - 1] = h[n - 2];
                b[n - 1] = 2.0 * h[n - 2];
                r[n - 1] = -6.0 * slope[n - 2];
            }
        }

        let m = thomas(&a, &b, &c, &r);
        for i in 0..n - 1 {
            d[i] = slope[i] - h[i] * (2.0 * m[i] + m[i + 1]) / 6.0;
        }
        d[n - 1] = slope[n - 2] + h[n - 2] * (m[n - 2] + 2.0 * m[n - 1]) / 6.0;

        Spline { xs: xs.to_vec(), ys: ys.to_vec(), d, cubic: true }
    }

    /// A C1 quadratic spline. One end derivative completes the system, and the
    /// rest propagate forward, which is the "more efficient evaluation with
    /// generally less favorable interpolation results" 9.21.4 describes. The
    /// clause warns that avoiding an end point discontinuity "is not always
    /// possible in this case": only the lower end can be pinned.
    fn quadratic(xs: &[f64], ys: &[f64], ctrl: DimControl) -> Spline {
        let n = xs.len();
        let mut d = vec![0.0; n];
        d[0] = match ctrl.lo {
            Extrap::Constant => 0.0,
            _ => (ys[1] - ys[0]) / (xs[1] - xs[0]),
        };
        for i in 0..n - 1 {
            let h = xs[i + 1] - xs[i];
            d[i + 1] = 2.0 * (ys[i + 1] - ys[i]) / h - d[i];
        }
        Spline { xs: xs.to_vec(), ys: ys.to_vec(), d, cubic: false }
    }

    fn eval(&self, x: f64, ctrl: DimControl, dim: usize) -> Result<f64, TableError> {
        let n = self.xs.len();
        if x < self.xs[0] {
            return match ctrl.lo {
                Extrap::Constant => Ok(self.ys[0]),
                // "a slope consistent with the selected interpolation method",
                // which for a spline is its own end derivative.
                Extrap::Linear => Ok(self.ys[0] + self.d[0] * (x - self.xs[0])),
                Extrap::Error => err(format!(
                    "lookup at {x} falls outside dimension {dim} of the table, whose control \
                     string asks for an error there"
                )),
            };
        }
        if x > self.xs[n - 1] {
            return match ctrl.hi {
                Extrap::Constant => Ok(self.ys[n - 1]),
                Extrap::Linear => Ok(self.ys[n - 1] + self.d[n - 1] * (x - self.xs[n - 1])),
                Extrap::Error => err(format!(
                    "lookup at {x} falls outside dimension {dim} of the table, whose control \
                     string asks for an error there"
                )),
            };
        }
        let (i, _, _) = bracket(&self.xs, x);
        let h = self.xs[i + 1] - self.xs[i];
        let t = (x - self.xs[i]) / h;
        let (y0, y1, d0, d1) = (self.ys[i], self.ys[i + 1], self.d[i], self.d[i + 1]);
        if self.cubic {
            // Hermite form, which needs only the two knot values and slopes.
            let h00 = 2.0 * t * t * t - 3.0 * t * t + 1.0;
            let h10 = t * t * t - 2.0 * t * t + t;
            let h01 = -2.0 * t * t * t + 3.0 * t * t;
            let h11 = t * t * t - t * t;
            Ok(h00 * y0 + h10 * h * d0 + h01 * y1 + h11 * h * d1)
        } else {
            // The quadratic through y0 with slope d0, which reaches y1 at t = 1
            // because `quadratic` chose d1 to make it so.
            let _ = d1;
            Ok(y0 + d0 * h * t + (y1 - y0 - d0 * h) * t * t)
        }
    }
}

/// One piece of a dimension's interpolant, as the lowering needs it.
///
/// Every scheme in 9.21.4 is, for fixed knots, a *linear* map from the samples
/// along the dimension to the interpolated value, with coefficients that depend
/// only on the lookup ordinate. So each piece is
///
///   value(x) = sum over j of  sample_j * P_j(x - origin)
///
/// with `P_j` of degree at most three and known at compile time. That holds for
/// the splines too, because solving their system is itself linear in the samples,
/// which is what spares the generated code a run-time solve.
#[derive(Clone, PartialEq, Debug)]
pub struct Segment {
    /// The upper end of the piece and whether it is included. `None` is the last
    /// piece, which runs to infinity.
    pub upper: Option<(f64, bool)>,
    /// `x` is measured from here, so a table far from the origin keeps its
    /// precision.
    pub origin: f64,
    /// `(sample index, coefficients of 1, u, u^2, u^3)` for `u = x - origin`.
    pub terms: Vec<(usize, [f64; 4])>,
    /// This piece is an `E` extrapolation: reaching it is a fatal error.
    pub error: bool,
}

impl Segment {
    /// Evaluate against sampled values, which is how the tests check that the
    /// pieces agree with [`Table::eval`]. The lowering walks the coefficients
    /// instead of calling this.
    #[cfg(test)]
    fn eval(&self, ys: &[f64], x: f64) -> f64 {
        let u = x - self.origin;
        let mut acc = 0.0;
        for (j, c) in &self.terms {
            acc += ys[*j] * (c[0] + u * (c[1] + u * (c[2] + u * c[3])));
        }
        acc
    }
}

/// The pieces of one dimension's interpolant, in ascending order of `x`.
pub fn segments(xs: &[f64], ctrl: DimControl) -> Vec<Segment> {
    let n = xs.len();
    let mut out = Vec::new();

    if ctrl.interp == Interp::Discrete {
        // Closest point: the pieces meet at the midpoints. A lookup exactly on a
        // midpoint "snaps away from zero", so whether the boundary belongs to the
        // lower sample depends on which of the two sits further from the origin.
        for i in 0..n - 1 {
            let mid = 0.5 * (xs[i] + xs[i + 1]);
            let lower_wins = xs[i].abs() > xs[i + 1].abs();
            out.push(Segment {
                upper: Some((mid, lower_wins)),
                origin: xs[i],
                terms: vec![(i, [1.0, 0.0, 0.0, 0.0])],
                error: false,
            });
        }
        out.push(Segment {
            upper: None,
            origin: xs[n - 1],
            terms: vec![(n - 1, [1.0, 0.0, 0.0, 0.0])],
            error: false,
        });
        return out;
    }

    // The first derivative at each knot, as a linear functional of the samples.
    // For linear interpolation the knot slopes are not needed, so this is only
    // built for the splines.
    let dmat = match ctrl.interp {
        Interp::Quadratic | Interp::Cubic => Some(derivative_weights(xs, ctrl)),
        _ => None,
    };

    // The lower extrapolation piece.
    out.push(end_segment(xs, ctrl.lo, dmat.as_deref(), true, Some((xs[0], false))));

    for i in 0..n - 1 {
        let h = xs[i + 1] - xs[i];
        let last = i == n - 2;
        let upper = if last { None } else { Some((xs[i + 1], false)) };
        let terms = match ctrl.interp {
            Interp::Linear => {
                // (1 - u/h) on the left sample, u/h on the right.
                vec![(i, [1.0, -1.0 / h, 0.0, 0.0]), (i + 1, [0.0, 1.0 / h, 0.0, 0.0])]
            }
            Interp::Cubic | Interp::Quadratic => {
                let d = dmat.as_deref().unwrap();
                let mut terms: Vec<(usize, [f64; 4])> = (0..n).map(|j| (j, [0.0; 4])).collect();
                if ctrl.interp == Interp::Cubic {
                    // Hermite in t = u/h:
                    //   h00 = 2t^3 - 3t^2 + 1      h10 = t^3 - 2t^2 + t
                    //   h01 = -2t^3 + 3t^2         h11 = t^3 - t^2
                    let (h2, h3) = (h * h, h * h * h);
                    add(&mut terms, i, [1.0, 0.0, -3.0 / h2, 2.0 / h3]);
                    add(&mut terms, i + 1, [0.0, 0.0, 3.0 / h2, -2.0 / h3]);
                    for j in 0..n {
                        // h * h10(t) and h * h11(t), weighted by the knot slopes.
                        let w0 = d[i][j];
                        let w1 = d[i + 1][j];
                        let c = [0.0, w0, -2.0 * w0 / h - w1 / h, w0 / h2 + w1 / h2];
                        add(&mut terms, j, c);
                    }
                } else {
                    // y_i + d_i*u + (y_{i+1} - y_i - d_i*h)*u^2/h^2.
                    add(&mut terms, i, [1.0, 0.0, -1.0 / (h * h), 0.0]);
                    add(&mut terms, i + 1, [0.0, 0.0, 1.0 / (h * h), 0.0]);
                    for j in 0..n {
                        let w = d[i][j];
                        add(&mut terms, j, [0.0, w, -w / h, 0.0]);
                    }
                }
                terms
            }
            Interp::Discrete | Interp::Ignore => unreachable!("handled above"),
        };
        out.push(Segment { upper, origin: xs[i], terms: prune(terms), error: false });
    }

    // The last interval runs to the final knot inclusive, and the upper
    // extrapolation piece takes everything past it.
    if let Some(last) = out.last_mut() {
        last.upper = Some((xs[n - 1], true));
    }
    out.push(end_segment(xs, ctrl.hi, dmat.as_deref(), false, None));
    out
}

/// The piece holding `x`, which is the first whose upper bound it is under.
pub fn segment_at(segs: &[Segment], x: f64) -> &Segment {
    for seg in segs {
        match seg.upper {
            Some((bound, true)) if x <= bound => return seg,
            Some((bound, false)) if x < bound => return seg,
            Some(_) => continue,
            None => return seg,
        }
    }
    segs.last().expect("a dimension always has a piece")
}

fn add(terms: &mut [(usize, [f64; 4])], j: usize, c: [f64; 4]) {
    for k in 0..4 {
        terms[j].1[k] += c[k];
    }
}

fn prune(terms: Vec<(usize, [f64; 4])>) -> Vec<(usize, [f64; 4])> {
    terms.into_iter().filter(|(_, c)| c.iter().any(|v| *v != 0.0)).collect()
}

/// An extrapolation piece off one end of a dimension.
fn end_segment(
    xs: &[f64],
    mode: Extrap,
    dmat: Option<&[Vec<f64>]>,
    low: bool,
    upper: Option<(f64, bool)>,
) -> Segment {
    let n = xs.len();
    let (end, inward) = if low { (0, 1) } else { (n - 1, n - 2) };
    let origin = xs[end];
    let terms = match mode {
        // "The constant extrapolation method returns the table endpoint value."
        Extrap::Constant => vec![(end, [1.0, 0.0, 0.0, 0.0])],
        // "Linear extrapolation extends linearly to the requested point from the
        // endpoint using a slope consistent with the selected interpolation
        // method", which for a spline is its own end derivative and for linear
        // interpolation is the end secant.
        Extrap::Linear => match dmat {
            Some(d) => {
                let mut terms: Vec<(usize, [f64; 4])> = (0..n).map(|j| (j, [0.0; 4])).collect();
                add(&mut terms, end, [1.0, 0.0, 0.0, 0.0]);
                for j in 0..n {
                    add(&mut terms, j, [0.0, d[end][j], 0.0, 0.0]);
                }
                prune(terms)
            }
            None => {
                let h = xs[inward] - xs[end];
                vec![(end, [1.0, -1.0 / h, 0.0, 0.0]), (inward, [0.0, 1.0 / h, 0.0, 0.0])]
            }
        },
        // Nothing is evaluated here, but a value keeps the shape uniform.
        Extrap::Error => vec![(end, [1.0, 0.0, 0.0, 0.0])],
    };
    Segment { upper, origin, terms, error: mode == Extrap::Error }
}

/// The first derivative at each knot, as a row of weights over the samples.
///
/// Both spline kinds reduce to knot derivatives, and both systems are linear in
/// the samples, so solving each once per unit sample gives the weights.
fn derivative_weights(xs: &[f64], ctrl: DimControl) -> Vec<Vec<f64>> {
    let n = xs.len();
    let mut rows = vec![vec![0.0; n]; n];
    for j in 0..n {
        let mut unit = vec![0.0; n];
        unit[j] = 1.0;
        let spline = if ctrl.interp == Interp::Cubic {
            Spline::cubic(xs, &unit, ctrl)
        } else {
            Spline::quadratic(xs, &unit, ctrl)
        };
        for i in 0..n {
            rows[i][j] = spline.d[i];
        }
    }
    rows
}

/// The Thomas algorithm for a tridiagonal system.
fn thomas(a: &[f64], b: &[f64], c: &[f64], r: &[f64]) -> Vec<f64> {
    let n = b.len();
    let mut cp = vec![0.0; n];
    let mut rp = vec![0.0; n];
    cp[0] = c[0] / b[0];
    rp[0] = r[0] / b[0];
    for i in 1..n {
        let m = b[i] - a[i] * cp[i - 1];
        cp[i] = c[i] / m;
        rp[i] = (r[i] - a[i] * rp[i - 1]) / m;
    }
    let mut out = vec![0.0; n];
    out[n - 1] = rp[n - 1];
    for i in (0..n - 1).rev() {
        out[i] = rp[i] - cp[i] * out[i + 1];
    }
    out
}

#[cfg(test)]
mod tests;
