use basedb::{diagnostics, BaseDB};
use expect_test::{expect, Expect};
use hir_def::db::HirDefDB;
use hir_def::nameres::ScopeDefItem;
use hir_ty::db::HirTyDB;
use hir_ty::validation::{BodyValidationDiagnostic, TypeValidationDiagnostic};
use stdx::format_to;

use crate::body::MirBuilder;
use crate::tests::TestDataBase;
use crate::PlaceKind;

fn check_with_diagnostics(src: &str, diagnostics: Expect, body: Expect) {
    let db = TestDataBase::new("/root.va", src);
    let def_map = db.def_map(db.root_file());

    let def = *def_map[def_map.root()]
        .declarations
        .values()
        .find_map(|def| if let ScopeDefItem::ModuleId(id) = def { Some(id) } else { None })
        .unwrap();
    let def = def.into();
    let mut actual = String::new();

    let res = &db.parse(db.root_file());
    if !res.errors().is_empty() {
        diagnostics::assert_empty_diagnostics(&db, db.root_file(), res.errors())
    }
    let res = &db.inference_result(def);
    if !res.diagnostics.is_empty() {
        format_to!(actual, "{:#?}", res.diagnostics);
    }

    let validation_res = TypeValidationDiagnostic::collect(&db, db.root_file());
    if !validation_res.is_empty() {
        format_to!(actual, "{:#?}", validation_res);
    }

    let validation_res = BodyValidationDiagnostic::collect(&db, def);
    if !validation_res.is_empty() {
        format_to!(actual, "{:#?}", validation_res);
    }

    diagnostics.assert_eq(&actual);

    let res = MirBuilder::new(&db, def, &|kind| {
        matches!(
            kind,
            PlaceKind::Var(_)
                | PlaceKind::BranchVoltage(_)
                | PlaceKind::ImplicitBranchVoltage { .. }
                | PlaceKind::BranchCurrent(_)
                | PlaceKind::ImplicitBranchCurrent { .. }
        )
    })
    .build();

    eprintln!("{:#?}", res.1);
    body.assert_eq(&res.0.to_debug_string());
}

fn check(src: &str, cfg: Expect) {
    check_with_diagnostics(src, expect![[r#""#]], cfg)
}

#[test]
fn case() {
    let src = r#"
module test;
    parameter integer foo = 0;
    parameter integer bar = 0;
    real test;
    real test2;
    analog case(abs(foo)+bar)
        0: test = 3.141;
        1,2,3: begin
            test = foo / 3.141;
            test2 = sin(test);
        end
        default: test = 0;
    endcase

endmodule
    "#;
    let mir = expect![[r#"
        function %(v13, v17, v21, v30) {
            v4 = iconst 0
            v5 = iconst 1
            v20 = fconst 0x1.920c49ba5e354p1
            v23 = iconst 2
            v25 = iconst 3
                                        block0:
        @0002                               v14 = ilt v13, v4
        @0002                               br v14, block2, block3

                                        block2:
        @0002                               v15 = ineg v13
        @0002                               jmp block4

                                        block3:
        @0002                               jmp block4

                                        block4:
        @0002                               v16 = phi [v15, block2], [v13, block3]
        @0004                               v18 = iadd v16, v17
        @0005                               v19 = ieq v4, v18
                                            br v19, block6, block7

                                        block6:
                                            jmp block5

                                        block7:
        @0008                               v22 = ieq v5, v18
                                            br v22, block8, block9

                                        block9:
        @0009                               v24 = ieq v23, v18
                                            br v24, block8, block10

                                        block10:
        @000a                               v26 = ieq v25, v18
                                            br v26, block8, block11

                                        block8:
        @000c                               v27 = ifcast v13
        @000e                               v28 = fdiv v27, v20
        @0011                               v29 = sin v28
                                            jmp block5

                                        block11:
        @0013                               v31 = ifcast v4
                                            jmp block5

                                        block5:
                                            v34 = phi [v30, block6], [v29, block8], [v30, block11]
                                            v32 = phi [v20, block6], [v28, block8], [v31, block11]
                                            v33 = optbarrier v32
                                            v36 = optbarrier v34
                                            jmp block1

                                        block1:
        }
    "#]];
    check(src, mir)
}

// #[test]
// fn multi_var() {
//     let src = r#"
// module test;
//     parameter integer foo = 0;
//     parameter integer bar = 0;
//     real test;
//     real test2;
//     real test3;
//     real test4;
//     real test5;
//     analog begin
//     if (foo == 3) begin
//         if (bar == 4) begin
//             $write("foo");
//         end
//         else begin
//             $write("bar");
//         end
//     end
//      if (foo == 0) begin
//         test4 = 0.0;
//      end
//      test2 = 0.0;
//      test3 = 0.0;
//      test4 = test4 + 0.0;
//      test5 = 0.0;

//      if (foo == 0) begin
//         test3 = test2 + 1;
//     end else  begin
//         test2 = 3.141;
//     end

//     test5 = test2 + test3 + test4;
//     end

// endmodule
//     "#;
//     let mir = expect![[r#"
//         function %(v12, v16, v20, v29) {
//             v4 = iconst 0
//             v5 = iconst 1
//             v19 = fconst 0x1.920c49ba5e354p1
//             v22 = iconst 2
//             v24 = iconst 3
//                                         block0:
//         @0002                               v13 = ilt v12, v4
//         @0002                               br v13, block2, block3

//                                         block2:
//         @0002                               v14 = ineg v12
//         @0002                               jmp block4

//                                         block3:
//         @0002                               jmp block4

//                                         block4:
//         @0002                               v15 = phi [v14, block2], [v12, block3]
//         @0004                               v17 = iadd v15, v16
//         @0005                               v18 = ieq v4, v17
//                                             br v18, block6, block7

//                                         block6:
//                                             jmp block5

//                                         block7:
//         @0008                               v21 = ieq v5, v17
//                                             br v21, block8, block9

//                                         block9:
//         @0009                               v23 = ieq v22, v17
//                                             br v23, block8, block10

//                                         block10:
//         @000a                               v25 = ieq v24, v17
//                                             br v25, block8, block11

//                                         block8:
//         @000c                               v26 = ifcast v12
//         @000e                               v27 = fdiv v26, v19
//         @0011                               v28 = sin v27
//                                             jmp block5

//                                         block11:
//         @0013                               v30 = ifcast v4
//                                             jmp block5

//                                         block5:
//                                             v32 = phi [v29, block6], [v28, block8], [v29, block11]
//                                             v31 = phi [v19, block6], [v27, block8], [v30, block11]
//                                             jmp block1

//                                         block1:
//         }
//     "#]];
//     check(src, mir)
// }
