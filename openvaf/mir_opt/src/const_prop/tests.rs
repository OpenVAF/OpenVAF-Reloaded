use expect_test::{expect, Expect};
use mir::ControlFlowGraph;
use mir_reader::parse_function;

use crate::sparse_conditional_constant_propagation;

fn check(src: &str, data_flow_result: Expect) {
    let (mut func, _) = parse_function(src).unwrap();
    let mut cfg = ControlFlowGraph::new();
    cfg.compute(&func);
    sparse_conditional_constant_propagation(&mut func, &cfg);
    data_flow_result.assert_eq(&func.to_debug_string());
}

#[test]
pub fn const_phi() {
    let raw = r##"
        function %bar(v20) {
        block0:
            v21 = imul v4, v20
            v22 = ieq v4, v21
            br v22, block1, block2
        block1:
            v23 = iadd v5, v5
            jmp block3
        block2:
            v24 = isub v5, v5
            jmp block3
        block3:
            v25 = phi [v23, block1], [v24, block2]
            v26 = imul v25, v20
        }
    "##;

    let expect = expect![[r#"
        function %bar(v20) {
            // v2 = bconst true
            v27 = iconst 2
        block0:
            br v2, block1, block2

        block1:
            jmp block3

        block2:
            jmp block3

        block3:
            v26 = imul v27, v20
        }
    "#]];

    check(raw, expect)
}

/// `expm1`/`ln1p` must be const folded rather than hitting the `unreachable!` arm
/// in `eval_unary`.
#[test]
pub fn const_expm1_ln1p() {
    let raw = r##"
        function %bar(v20) {
            v30 = fconst 0x1.0000000000000p0
        block0:
            v21 = expm1 v30
            v22 = ln1p v30
            v23 = fadd v21, v22
            v24 = fmul v23, v20
        }
    "##;

    let expect = expect![[r#"
        function %bar(v20) {
            v32 = fconst 0x1.34a9b4ad2e5e5p1
        block0:
            v24 = fmul v32, v20
        }
    "#]];

    check(raw, expect)
}

/// `ln1p` exists precisely because `ln(1+x)` loses all precision for small `x`.
/// Const folding must keep that property: with x = 1e-16 the `ln1p` result stays
/// 1e-16 while the naive `ln(1+x)` in the same function folds to 0.
#[test]
pub fn const_ln1p_precision() {
    let raw = r##"
        function %bar(v20) {
            v30 = fconst 0x1.cd2b297d889bcp-54
            v31 = fconst 0x1.0000000000000p0
        block0:
            v21 = ln1p v30
            v32 = fadd v31, v30
            v33 = ln v32
            v22 = fmul v21, v20
            v23 = fmul v33, v20
        }
    "##;

    let expect = expect![[r#"
        function %bar(v20) {
            v30 = fconst 0x1.cd2b297d889bcp-54
        block0:
            v22 = fmul v30, v20
        }
    "#]];

    check(raw, expect)
}

#[test]
pub fn no_const_backward_edge() {
    let raw = r##"
        function %bar(v20) {
            v30 = iconst 20
        block0:
            jmp block1
        block1:
            v21 = phi [v4, block0], [v23, block2]
            v22 = ilt v21, v30
            br v22, block2, block3
        block2:
            v23 = iadd v21, v5
            jmp block1
        block3:
            v24 = imul v22, v20
        }
    "##;

    let expect = expect![[r#"
        function %bar(v20) {
            v4 = iconst 0
            v5 = iconst 1
            v30 = iconst 20
        block0:
            jmp block1

        block1:
            v21 = phi [v4, block0], [v23, block2]
            v22 = ilt v21, v30
            br v22, block2, block3

        block2:
            v23 = iadd v21, v5
            jmp block1

        block3:
            v24 = imul v22, v20
        }
    "#]];

    check(raw, expect)
}

#[test]
pub fn const_backward_edge() {
    let raw = r##"
        function %bar(v20) {
            v30 = iconst 20
        block0:
            jmp block1
        block1:
            v21 = phi [v30, block0], [v23, block2]
            v22 = ilt v21, v30
            br v22, block2, block3
        block2:
            v23 = iadd v21, v5
            jmp block1
        block3:
            v24 = imul v21, v20
        }
    "##;

    let expect = expect![[r#"
        function %bar(v20) {
            // v1 = bconst false
            v30 = iconst 20
        block0:
            jmp block1

        block1:
            br v1, block2, block3

        block2:
            jmp block1

        block3:
            v24 = imul v30, v20
        }
    "#]];

    check(raw, expect)
}
