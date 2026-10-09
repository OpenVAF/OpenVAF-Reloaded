//! VAMS-2023 4.5.12, against the transfer functions the clause states.

use super::*;

fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-12, "{a} != {b}");
}

fn closev(a: &[f64], b: &[f64]) {
    assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() < 1e-12, "{a:?} != {b:?}");
    }
}

#[test]
fn a_real_root_is_one_factor() {
    // (1 - 0.5 z^-1)
    closev(&roots_to_coeffs(&[0.5, 0.0], "zeros").unwrap(), &[1.0, -0.5]);
}

#[test]
fn a_conjugate_pair_has_real_coefficients() {
    // (1 - (0.5+0.5j) z^-1)(1 - (0.5-0.5j) z^-1) = 1 - z^-1 + 0.5 z^-2
    closev(&roots_to_coeffs(&[0.5, 0.5, 0.5, -0.5], "poles").unwrap(), &[1.0, -1.0, 0.5]);
}

#[test]
fn a_root_at_the_origin_contributes_unity() {
    // The clause's product form is `1 - z^-1 r`, which at r = 0 is just 1, so a
    // root at the origin leaves the polynomial alone.
    closev(&roots_to_coeffs(&[0.0, 0.0], "zeros").unwrap(), &[1.0, 0.0]);
}

#[test]
fn several_real_roots_multiply_out() {
    // (1 - 0.5 z^-1)(1 - 0.25 z^-1) = 1 - 0.75 z^-1 + 0.125 z^-2
    closev(&roots_to_coeffs(&[0.5, 0.0, 0.25, 0.0], "poles").unwrap(), &[1.0, -0.75, 0.125]);
}

#[test]
fn an_odd_root_vector_is_rejected() {
    let e = roots_to_coeffs(&[0.5, 0.0, 0.25], "zeros").unwrap_err();
    assert!(e.0.contains("pair of a real and an imaginary part"), "{}", e.0);
}

#[test]
fn an_unpaired_complex_root_is_rejected() {
    let e = roots_to_coeffs(&[0.5, 0.5], "poles").unwrap_err();
    assert!(e.0.contains("conjugate"), "{}", e.0);
}

#[test]
fn a_null_zeros_argument_is_a_unity_numerator() {
    let f = Filter::build(None, Side::Roots, &[1.0, -0.5], Side::Coeffs).unwrap();
    closev(&f.num, &[1.0]);
}

#[test]
fn a_zero_leading_denominator_is_rejected() {
    let e = Filter::build(Some(&[1.0]), Side::Coeffs, &[0.0, 1.0], Side::Coeffs).unwrap_err();
    assert!(e.0.contains("does not determine its own output"), "{}", e.0);
}

#[test]
fn the_four_forms_agree_on_the_same_filter() {
    // One real zero at 0.5 and one real pole at 0.25, written four ways.
    let nd = Filter::build(Some(&[1.0, -0.5]), Side::Coeffs, &[1.0, -0.25], Side::Coeffs).unwrap();
    let zd = Filter::build(Some(&[0.5, 0.0]), Side::Roots, &[1.0, -0.25], Side::Coeffs).unwrap();
    let np = Filter::build(Some(&[1.0, -0.5]), Side::Coeffs, &[0.25, 0.0], Side::Roots).unwrap();
    let zp = Filter::build(Some(&[0.5, 0.0]), Side::Roots, &[0.25, 0.0], Side::Roots).unwrap();
    assert_eq!(nd, zd);
    assert_eq!(nd, np);
    assert_eq!(nd, zp);
}

// -- the difference equation ----------------------------------------------------

#[test]
fn unity_is_a_sample_and_hold() {
    // 4.5.12: "A filter with unity transfer function acts like a simple
    // sample-and-hold which samples every T seconds and exhibits no delay."
    let f = Filter::build(Some(&[1.0]), Side::Coeffs, &[1.0], Side::Coeffs).unwrap();
    let input = [3.0, -1.0, 0.0, 7.5];
    closev(&f.run(&input), &input);
}

#[test]
fn a_pure_delay() {
    // H(z) = z^-1
    let f = Filter::build(Some(&[0.0, 1.0]), Side::Coeffs, &[1.0], Side::Coeffs).unwrap();
    closev(&f.run(&[1.0, 2.0, 3.0]), &[0.0, 1.0, 2.0]);
}

#[test]
fn a_one_pole_accumulator() {
    // H(z) = 1 / (1 - a z^-1): y[m] = x[m] + a y[m-1].
    let a = 0.5;
    let f = Filter::build(Some(&[1.0]), Side::Coeffs, &[1.0, -a], Side::Coeffs).unwrap();
    let got = f.run(&[1.0, 1.0, 1.0, 1.0]);
    let mut want = Vec::new();
    let mut y = 0.0;
    for _ in 0..4 {
        y = 1.0 + a * y;
        want.push(y);
    }
    closev(&got, &want);
    // The step response of a stable one-pole tends to 1/(1-a).
    let long = f.run(&vec![1.0; 200]);
    assert!((long[199] - 1.0 / (1.0 - a)).abs() < 1e-9);
}

#[test]
fn the_impulse_response_is_the_numerator_for_a_fir() {
    let num = [0.25, 0.5, 0.25];
    let f = Filter::build(Some(&num), Side::Coeffs, &[1.0], Side::Coeffs).unwrap();
    let mut impulse = vec![0.0; 6];
    impulse[0] = 1.0;
    let got = f.run(&impulse);
    closev(&got[..3], &num);
    closev(&got[3..], &[0.0, 0.0, 0.0]);
}

#[test]
fn a_complex_pole_pair_oscillates_and_decays() {
    // Poles at r*exp(+-j*theta) give y[m] = 2 r cos(theta) y[m-1] - r^2 y[m-2].
    let (r, theta) = (0.9_f64, 0.7_f64);
    let (pr, pi) = (r * theta.cos(), r * theta.sin());
    let f = Filter::build(Some(&[1.0]), Side::Coeffs, &[pr, pi, pr, -pi], Side::Roots).unwrap();
    closev(&f.den, &[1.0, -2.0 * r * theta.cos(), r * r]);

    let mut impulse = vec![0.0; 40];
    impulse[0] = 1.0;
    let got = f.run(&impulse);
    // The impulse response of such a pair is r^m * sin((m+1)theta)/sin(theta).
    for (m, y) in got.iter().enumerate() {
        let want = r.powi(m as i32) * ((m as f64 + 1.0) * theta).sin() / theta.sin();
        assert!((y - want).abs() < 1e-9, "sample {m}: {y} != {want}");
    }
}

#[test]
fn the_order_is_read_from_the_coefficients() {
    let f =
        Filter::build(Some(&[1.0, 2.0, 3.0]), Side::Coeffs, &[1.0, -0.5], Side::Coeffs).unwrap();
    assert_eq!(f.order(), (2, 1));
    close(f.step(&[1.0, 0.0, 0.0], &[0.0]), 1.0);
}
