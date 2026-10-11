//! VAMS-2023 9.21 worked against the clause's own example and tables.

use super::*;

/// 9.21.1's sample file: f(x,y) = 0.5x + y on three ragged isolines in y.
const SAMPLE: &str = "\
# 2-D table model sample example
#
#  y   x   f(x,y)
#y=0 isoline
0.0 1.0 0.5
0.0 2.0 1.0
0.0 3.0 1.5
0.0 4.0 2.0
0.0 5.0 2.5
0.0 6.0 3.0
#y=0.5 isoline
0.5 1.0 1.0
0.5 3.0 2.0
0.5 5.0 3.0
#y=1.0 isoline
1.0 1.0 1.5
1.0 2.0 2.0
1.0 4.0 3.0
";

fn sample() -> Table {
    let rows = Rows::parse(SAMPLE).unwrap();
    let control = Control::parse("", 2).unwrap();
    Table::build(&rows, &control).unwrap().0
}

fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-12, "{a} != {b}");
}

#[test]
fn parses_the_sample_file() {
    let rows = Rows::parse(SAMPLE).unwrap();
    // Comments, the blank comment line and the isoline headers all drop out.
    assert_eq!(rows.cols, 3);
    assert_eq!(rows.rows.len(), 12);
    assert_eq!(rows.rows[0], vec![0.0, 1.0, 0.5]);
    assert_eq!(rows.rows[11], vec![1.0, 4.0, 3.0]);
}

#[test]
fn rejects_ragged_rows() {
    let e = Rows::parse("1 2 3\n4 5\n").unwrap_err();
    assert!(e.0.contains("2 columns"), "{}", e.0);
}

#[test]
fn rejects_non_numbers() {
    let e = Rows::parse("1 2 three\n").unwrap_err();
    assert!(e.0.contains("'three'"), "{}", e.0);
}

#[test]
fn builds_ragged_isolines() {
    let t = sample();
    let kids = match &t.root {
        Node::Branch(kids) => kids,
        Node::Leaf(_) => panic!("expected a branch"),
    };
    // Three isolines in y, of six, three and three points: ragged, as drawn.
    assert_eq!(kids.len(), 3);
    assert_eq!(kids.iter().map(|(o, _)| *o).collect::<Vec<_>>(), vec![0.0, 0.5, 1.0]);
    assert_eq!(kids.iter().map(|(_, n)| n.len()).collect::<Vec<_>>(), vec![6, 3, 3]);
}

/// 9.21's own walked-through lookup. Figure 9-2 brackets y1 = 0.25 between the
/// y = 0 and y = 0.5 isolines, interpolates each at x1 = 3.5, then interpolates
/// those two results, and states the answer: "f(x1,y1)=2.0".
///
/// The two intermediate values follow from the isolines themselves. On y = 0,
/// sampled every 1.0, x = 3.5 interpolates to 1.75, which is also the exact
/// 0.5x + y. On y = 0.5, sampled at x = 1, 3, 5, x = 3.5 brackets 3 and 5 and
/// interpolates to 2.25. Interpolating 1.75 and 2.25 at a quarter of the way up
/// gives the 2.0 the clause states.
#[test]
fn the_clause_worked_example() {
    let t = sample();
    close(t.eval(&[0.0, 3.5]).unwrap(), 1.75);
    close(t.eval(&[0.5, 3.5]).unwrap(), 2.25);
    close(t.eval(&[0.25, 3.5]).unwrap(), 2.0);
}

#[test]
fn samples_come_back_exactly() {
    let t = sample();
    close(t.eval(&[0.0, 1.0]).unwrap(), 0.5);
    close(t.eval(&[0.5, 3.0]).unwrap(), 2.0);
    close(t.eval(&[1.0, 4.0]).unwrap(), 3.0);
}

#[test]
fn the_array_form_is_the_same_table() {
    let y = vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5, 0.5, 1.0, 1.0, 1.0];
    let x = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 1.0, 3.0, 5.0, 1.0, 2.0, 4.0];
    let f = vec![0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 1.0, 2.0, 3.0, 1.5, 2.0, 3.0];
    let rows = Rows::from_columns(&[y, x, f]).unwrap();
    let control = Control::parse("", 2).unwrap();
    let t = Table::build(&rows, &control).unwrap().0;
    assert_eq!(t, sample());
}

#[test]
fn unsorted_data_is_sorted_into_isolines() {
    // The same twelve samples, shuffled. 9.21.1 requires the tool to sort.
    let mut lines: Vec<&str> =
        SAMPLE.lines().filter(|l| !l.trim_start().starts_with('#')).collect();
    lines.reverse();
    let shuffled = lines.join("\n");
    let rows = Rows::parse(&shuffled).unwrap();
    let control = Control::parse("", 2).unwrap();
    let t = Table::build(&rows, &control).unwrap().0;
    assert_eq!(t, sample());
}

#[test]
fn duplicate_samples_warn_and_conflicting_ones_error() {
    let dup = format!("{SAMPLE}0.0 1.0 0.5\n");
    let rows = Rows::parse(&dup).unwrap();
    let (t, warnings) = Table::build(&rows, &Control::parse("", 2).unwrap()).unwrap();
    assert_eq!(t, sample());
    assert_eq!(warnings, vec!["ignored 1 duplicate sample(s)".to_owned()]);

    let bad = format!("{SAMPLE}0.0 1.0 99.0\n");
    let rows = Rows::parse(&bad).unwrap();
    let e = Table::build(&rows, &Control::parse("", 2).unwrap()).unwrap_err();
    assert!(e.0.contains("disagree"), "{}", e.0);
}

// -- the control string, against Table 9-32 -------------------------------------

fn ctrl(s: &str, inputs: usize) -> Control {
    Control::parse(s, inputs).unwrap()
}

#[test]
fn null_control_string_is_linear_in_n_dimensions() {
    let c = ctrl("", 2);
    assert_eq!(c.columns, vec![DimControl::default(); 2]);
    assert_eq!(c.dependent, 1);
    // "Column N+1 is taken as the dependent": 0-based column 2 of a 2-D table.
    assert_eq!(c.dep_column(), 2);
}

#[test]
fn table_9_32_examples() {
    let lin = DimControl { interp: Interp::Linear, lo: Extrap::Linear, hi: Extrap::Linear };
    let con = DimControl { interp: Interp::Linear, lo: Extrap::Constant, hi: Extrap::Constant };

    assert_eq!(ctrl("1L,1L", 2).columns, vec![lin, lin]);
    assert_eq!(ctrl("1LL,1LL", 2).columns, vec![lin, lin]);
    assert_eq!(ctrl("1LL,1LL;1", 2).dependent, 1);

    // "D,1,3": closest point outermost, linear, cubic innermost.
    let c = ctrl("D,1,3", 3);
    assert_eq!(c.columns[0].interp, Interp::Discrete);
    assert_eq!(c.columns[1].interp, Interp::Linear);
    assert_eq!(c.columns[2].interp, Interp::Cubic);

    // "I,1CC,1CC;3": ignore column 1, and the dependent is column 6 of at least 6.
    let c = ctrl("I,1CC,1CC;3", 2);
    assert_eq!(c.columns[0].interp, Interp::Ignore);
    assert_eq!(c.dims(), vec![con, con]);
    assert_eq!(c.dependent, 3);
    assert_eq!(c.dep_column(), 5);

    // "3,D,I,1;3": three independent dimensions, one ignored column, dependent 3
    // at column 7 of at least 7.
    let c = ctrl("3,D,I,1;3", 3);
    assert_eq!(c.indep_columns(), vec![0, 1, 3]);
    assert_eq!(c.dep_column(), 6);

    // "C,,3" is "1CC, 1LL, 3LL".
    let c = ctrl("C,,3", 3);
    assert_eq!(c.columns[0], con);
    assert_eq!(c.columns[1], lin);
    assert_eq!(
        c.columns[2],
        DimControl { interp: Interp::Cubic, lo: Extrap::Linear, hi: Extrap::Linear }
    );
}

#[test]
fn asymmetric_extrapolation_ends() {
    let c = ctrl("1CE", 1);
    assert_eq!(c.columns[0].lo, Extrap::Constant);
    assert_eq!(c.columns[0].hi, Extrap::Error);
}

#[test]
fn control_string_errors() {
    assert!(Control::parse("1L,1L", 3).unwrap_err().0.contains("2 independent"));
    assert!(Control::parse("1Q", 1).unwrap_err().0.contains("'Q'"));
    assert!(Control::parse("1LLL", 1).unwrap_err().0.contains("at most"));
    assert!(Control::parse(";0", 1).unwrap_err().0.contains("selector"));
    assert!(Control::parse("I", 0).unwrap_err().0.contains("at least one"));
}

// -- interpolation and extrapolation per dimension -----------------------------

fn table_1d(xs: &[f64], ys: &[f64], control: &str) -> Table {
    let cols = vec![xs.to_vec(), ys.to_vec()];
    let rows = Rows::from_columns(&cols).unwrap();
    Table::build(&rows, &Control::parse(control, 1).unwrap()).unwrap().0
}

#[test]
fn linear_interpolation_and_the_default_extrapolation() {
    let t = table_1d(&[0.0, 1.0, 2.0], &[0.0, 10.0, 20.0], "1");
    close(t.eval(&[0.5]).unwrap(), 5.0);
    close(t.eval(&[1.25]).unwrap(), 12.5);
    // Linear is the default off both ends, at the end slope.
    close(t.eval(&[-1.0]).unwrap(), -10.0);
    close(t.eval(&[3.0]).unwrap(), 30.0);
}

#[test]
fn constant_and_error_extrapolation() {
    let t = table_1d(&[0.0, 1.0, 2.0], &[0.0, 10.0, 20.0], "1CC");
    close(t.eval(&[-5.0]).unwrap(), 0.0);
    close(t.eval(&[5.0]).unwrap(), 20.0);

    let t = table_1d(&[0.0, 1.0, 2.0], &[0.0, 10.0, 20.0], "1EE");
    assert!(t.eval(&[-0.5]).is_err());
    assert!(t.eval(&[2.5]).is_err());
    // Inside the table it interpolates as usual.
    close(t.eval(&[1.5]).unwrap(), 15.0);

    // One end each way.
    let t = table_1d(&[0.0, 1.0, 2.0], &[0.0, 10.0, 20.0], "1CE");
    close(t.eval(&[-5.0]).unwrap(), 0.0);
    assert!(t.eval(&[5.0]).is_err());
}

#[test]
fn discrete_lookup_snaps_away_from_zero_on_a_tie() {
    // Not a tie: the nearer sample wins either side.
    let t = table_1d(&[-1.0, 1.0], &[7.0, 9.0], "D");
    close(t.eval(&[0.4]).unwrap(), 9.0);
    close(t.eval(&[-0.4]).unwrap(), 7.0);
    // A discrete dimension never extrapolates: it has a closest point everywhere.
    close(t.eval(&[100.0]).unwrap(), 9.0);
    close(t.eval(&[-100.0]).unwrap(), 7.0);

    // "When the lookup ordinate is equidistant from two bracketing samples the
    // function snaps away from zero", so the sample of larger magnitude wins.
    let t = table_1d(&[1.0, 3.0], &[7.0, 9.0], "D");
    close(t.eval(&[2.0]).unwrap(), 9.0);
    let t = table_1d(&[-3.0, -1.0], &[7.0, 9.0], "D");
    close(t.eval(&[-2.0]).unwrap(), 7.0);

    // A tie that straddles zero symmetrically has no larger magnitude to move to,
    // and the clause does not say which way to go, so this only pins that it
    // picks a sample rather than which one.
    let t = table_1d(&[-1.0, 1.0], &[7.0, 9.0], "D");
    let v = t.eval(&[0.0]).unwrap();
    assert!(v == 7.0 || v == 9.0, "{v}");
}

#[test]
fn a_spline_passes_through_its_samples() {
    let xs = [0.0, 1.0, 2.0, 3.0, 4.0];
    let ys = [0.0, 1.0, 8.0, 27.0, 64.0];
    for mode in ["2", "3"] {
        let t = table_1d(&xs, &ys, mode);
        for (x, y) in xs.iter().zip(ys.iter()) {
            close(t.eval(&[*x]).unwrap(), *y);
        }
    }
}

#[test]
fn a_cubic_spline_reproduces_a_cubic() {
    // A natural cubic spline is exact on a cubic only if the end conditions
    // happen to match, so use the quadratic-free case: a straight line, which
    // every scheme must reproduce.
    let t = table_1d(&[0.0, 1.0, 2.0, 3.0], &[1.0, 3.0, 5.0, 7.0], "3");
    close(t.eval(&[0.5]).unwrap(), 2.0);
    close(t.eval(&[2.25]).unwrap(), 5.5);
    // And linear extrapolation off a straight table stays on the line.
    close(t.eval(&[4.0]).unwrap(), 9.0);
}

#[test]
fn constant_extrapolation_flattens_a_spline_end() {
    // 9.21.4: "If constant extrapolation is specified the end point derivative is
    // set to zero thus avoiding a discontinuity in the first order derivative".
    let t = table_1d(&[0.0, 1.0, 2.0, 3.0], &[0.0, 1.0, 4.0, 9.0], "3CC");
    let y0 = t.eval(&[0.0]).unwrap();
    // Approaching the end, the slope must fall away to zero.
    let near = (t.eval(&[1e-6]).unwrap() - y0) / 1e-6;
    assert!(near.abs() < 1e-5, "end slope {near} should be ~0");
    // Outside, it holds the endpoint.
    close(t.eval(&[-1.0]).unwrap(), 0.0);
}

#[test]
fn a_natural_spline_has_zero_second_derivative_at_its_ends() {
    let t = table_1d(&[0.0, 1.0, 2.0, 3.0], &[0.0, 1.0, 4.0, 9.0], "3LL");
    let h = 1e-4;
    let second = (t.eval(&[2.0 * h]).unwrap() - 2.0 * t.eval(&[h]).unwrap()
        + t.eval(&[0.0]).unwrap())
        / (h * h);
    assert!(second.abs() < 1e-3, "second derivative {second} should be ~0");
}

// -- dimensionality -------------------------------------------------------------

#[test]
fn a_one_dimensional_table() {
    let t = table_1d(&[0.0, 2.0], &[0.0, 1.0], "1");
    close(t.eval(&[1.0]).unwrap(), 0.5);
}

#[test]
fn a_three_dimensional_table() {
    // f(z,y,x) = 100z + 10y + x on a full 2x2x2 grid, the clause's 2^N minimum.
    let mut cols = vec![Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for z in [0.0, 1.0] {
        for y in [0.0, 1.0] {
            for x in [0.0, 1.0] {
                cols[0].push(z);
                cols[1].push(y);
                cols[2].push(x);
                cols[3].push(100.0 * z + 10.0 * y + x);
            }
        }
    }
    let rows = Rows::from_columns(&cols).unwrap();
    let t = Table::build(&rows, &Control::parse("", 3).unwrap()).unwrap().0;
    close(t.eval(&[0.0, 0.0, 0.0]).unwrap(), 0.0);
    close(t.eval(&[1.0, 1.0, 1.0]).unwrap(), 111.0);
    // Trilinear interpolation is exact on a function linear in each coordinate.
    close(t.eval(&[0.5, 0.5, 0.5]).unwrap(), 55.5);
    close(t.eval(&[0.25, 0.5, 0.75]).unwrap(), 30.75);
}

#[test]
fn an_ignored_column_is_skipped() {
    // Four columns: an ignored one, then the independent, then two dependents.
    let ignored = vec![9.0, 9.0, 9.0];
    let x = vec![0.0, 1.0, 2.0];
    let d1 = vec![0.0, 10.0, 20.0];
    let d2 = vec![0.0, 100.0, 200.0];
    let rows = Rows::from_columns(&[ignored, x, d1, d2]).unwrap();

    let t = Table::build(&rows, &Control::parse("I,1", 1).unwrap()).unwrap().0;
    close(t.eval(&[0.5]).unwrap(), 5.0);

    // The selector reaches past the described columns to the second dependent.
    let t = Table::build(&rows, &Control::parse("I,1;2", 1).unwrap()).unwrap().0;
    close(t.eval(&[0.5]).unwrap(), 50.0);
}

#[test]
fn too_few_points_for_the_scheme() {
    let rows = Rows::from_columns(&[vec![0.0], vec![1.0]]).unwrap();
    let e = Table::build(&rows, &Control::parse("1", 1).unwrap()).unwrap_err();
    assert!(e.0.contains("needs 2"), "{}", e.0);
    // A discrete dimension is content with one sample.
    assert!(Table::build(&rows, &Control::parse("D", 1).unwrap()).is_ok());
}

#[test]
fn a_selector_past_the_last_column() {
    let rows = Rows::from_columns(&[vec![0.0, 1.0], vec![0.0, 1.0]]).unwrap();
    let e = Table::build(&rows, &Control::parse("1;4", 1).unwrap()).unwrap_err();
    assert!(e.0.contains("reaches column 5"), "{}", e.0);
}

#[test]
fn a_mixed_scheme_table() {
    // Discrete outermost, linear innermost: the outer coordinate snaps to an
    // isoline and the inner interpolates along it.
    let y = vec![0.0, 0.0, 10.0, 10.0];
    let x = vec![0.0, 1.0, 0.0, 1.0];
    let f = vec![0.0, 1.0, 100.0, 101.0];
    let rows = Rows::from_columns(&[y, x, f]).unwrap();
    let t = Table::build(&rows, &Control::parse("D,1", 2).unwrap()).unwrap().0;
    close(t.eval(&[0.0, 0.5]).unwrap(), 0.5);
    // 4 is nearer 0 than 10, so it snaps to the y = 0 isoline.
    close(t.eval(&[4.0, 0.5]).unwrap(), 0.5);
    close(t.eval(&[6.0, 0.5]).unwrap(), 100.5);
}

// -- the piecewise form the lowering emits -------------------------------------

/// Every scheme must agree with [`Table::eval`] when evaluated through
/// [`segments`], because the lowering emits the pieces rather than the
/// recursion. This is the check that the polynomial coefficients are right.
#[test]
fn the_segments_agree_with_eval() {
    let xs = [-2.0, -0.5, 1.0, 1.25, 4.0];
    let ys = [3.0, -1.0, 0.5, 2.0, -4.0];

    for interp in ["D", "1", "2", "3"] {
        for ext in ["LL", "CC", "CL", "LC"] {
            let spec = format!("{interp}{ext}");
            let t = table_1d(&xs, &ys, &spec);
            let ctrl = Control::parse(&spec, 1).unwrap().columns[0];
            let segs = segments(&xs, ctrl);

            // Inside, on every knot, and well outside both ends.
            let mut probes: Vec<f64> = vec![-5.0, -2.0, 4.0, 9.0];
            for k in 0..=200 {
                probes.push(-3.0 + 0.05 * f64::from(k));
            }
            for x in probes {
                let want = t.eval(&[x]).unwrap();
                let got = segment_at(&segs, x).eval(&ys, x);
                assert!(
                    (want - got).abs() < 1e-9,
                    "{spec} at x={x}: segments gave {got}, eval gave {want}"
                );
            }
        }
    }
}

#[test]
fn an_error_end_is_marked_on_its_piece() {
    let xs = [0.0, 1.0, 2.0];
    let segs = segments(&xs, Control::parse("1CE", 1).unwrap().columns[0]);
    // The lower piece extrapolates, the upper one is fatal.
    assert!(!segment_at(&segs, -1.0).error);
    assert!(segment_at(&segs, 3.0).error);
    assert!(!segment_at(&segs, 1.5).error);
    // The knots themselves are inside the table.
    assert!(!segment_at(&segs, 0.0).error);
    assert!(!segment_at(&segs, 2.0).error);
}

#[test]
fn a_linear_dimension_reaches_only_two_samples() {
    // Code size matters: a linear piece must not carry weights for the whole
    // isoline the way a spline does.
    let xs = [0.0, 1.0, 2.0, 3.0, 4.0];
    let segs = segments(&xs, Control::parse("1", 1).unwrap().columns[0]);
    for seg in &segs {
        assert!(seg.terms.len() <= 2, "{} terms", seg.terms.len());
    }
    // A discrete piece reaches exactly one.
    let segs = segments(&xs, Control::parse("D", 1).unwrap().columns[0]);
    for seg in &segs {
        assert_eq!(seg.terms.len(), 1);
    }
}
