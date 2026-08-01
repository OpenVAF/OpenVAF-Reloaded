use std::sync::Arc;
use std::{cell::RefCell, path::PathBuf};

use expect_test::{expect, expect_file};
use vfs::{FileId, Vfs, VfsPath};

use crate::{preprocess, Preprocess, SourceProvider};

struct TestSourceProvider {
    vfs: RefCell<Vfs>,
    include_dirs: Arc<[VfsPath]>,
}

impl TestSourceProvider {
    pub fn new(mut include_dirs: Vec<VfsPath>) -> Self {
        let mut vfs = Vfs::default();
        vfs.insert_std_lib();
        include_dirs.push(VfsPath::new_virtual_path("/std".to_owned()));
        Self { vfs: RefCell::new(vfs), include_dirs: Arc::from(include_dirs) }
    }
}

impl SourceProvider for TestSourceProvider {
    fn include_dirs(&self, _root_file: FileId) -> Arc<[VfsPath]> {
        self.include_dirs.clone()
    }

    fn macro_flags(&self, _file_root: FileId) -> Arc<[Arc<str>]> {
        Arc::new([])
    }

    fn file_text(&self, file: FileId) -> Result<Arc<str>, crate::FileReadError> {
        let vfs = self.vfs.borrow();
        vfs.file_contents(file).map(Arc::from)
    }

    fn file_path(&self, file: FileId) -> VfsPath {
        self.vfs.borrow().file_path(file)
    }

    fn file_id(&self, path: VfsPath) -> FileId {
        self.vfs.borrow_mut().ensure_file_id(path)
    }

    fn allocate_virtual_file(&self, path: &str, contents: Arc<str>) -> FileId {
        self.vfs.borrow_mut().add_virt_file(path, contents.to_string().into())
    }
}

fn check_prepocessor(sources: TestSourceProvider, root_file: FileId, test_name: &'static str) {
    let Preprocess { ts, diagnostics, sm } = preprocess(&sources, root_file);
    assert_eq!(diagnostics.as_slice(), &[]);
    let actual_tokens: String = ts.iter().map(|token| format!("{:?}\n", token.kind,)).collect();
    let expected = PathBuf::from(".").join("test_data").join(format!("{}.tokens", test_name));
    expect_file![expected].assert_eq(&actual_tokens);

    let vfs = sources.vfs.borrow();
    let actual_content: String = ts
        .iter()
        .map(|token| {
            let filespan = token.span.to_file_span(&sm);
            let src = vfs.file_contents(filespan.file).unwrap();
            &src[filespan.range]
        })
        .collect();

    let expected = PathBuf::from(".").join("test_data").join(format!("{}_expanded.va", test_name));
    expect_file![expected].assert_eq(&actual_content);
}

fn check_prepocessor_single_file(src: &str, test_name: &'static str) {
    let sources = TestSourceProvider::new(vec![]);
    let file =
        sources.vfs.borrow_mut().add_virt_file("/macro_expansion_test.va", src.to_owned().into());
    check_prepocessor(sources, file, test_name)
}

#[test]
pub fn smoke_test() {
    const SRC: &str = r#"
`define test5(x,y) (x)+(y)
`ifdef test1 ERROR
`endif
`define test2
`ifdef test2 OK1
`endif,
    `ifdef test2 OK2,`define test3 OK3\
OK3L
    `endif
`ifdef test4 ERROR
`else
`define test7(x,y,z) \
/* foo */ \
x*(y%z)\
/* bar */
SMS__
`endif

`ifndef test4
`define test4 OK4
                                            `endif
`test3

,

`ifndef test4
ERROR
`else
`test4
`endif
`test5(Sum1,Sum2)
`define test6(x,y,z) `test5(`test7(x,y,z),f(x/y)*z)
`test6(a,b,c)
"#;

    check_prepocessor_single_file(SRC, "smoke_test")
}

#[test]
fn whitespaces() {
    check_prepocessor_single_file(
        r#"
        `define FOO BAR
        // foo
        `define TEST `FOO\
        BAR
        "#,
        "whitespaces",
    )
}

#[test]
fn condition_enabled() {
    check_prepocessor_single_file(
        r#"
`ifdef DISABLE_STROBE
	`define STROBE(X)
	`define STROBE2(X,Y)
`else
	`define STROBE(X) $strobe(X)
	`define STROBE2(X,Y) $strobe(X,Y)
`endif

`STROBE(foo)
`STROBE2(bar,test)

        "#,
        "condition_enabled",
    )
}

#[test]
fn condition_disabled() {
    check_prepocessor_single_file(
        r#"
`define DISABLE_STROBE
`ifdef DISABLE_STROBE
	`define STROBE(X)
	`define STROBE2(X,Y)
`else
	`define STROBE(X) $strobe(X)
	`define STROBE2(X,Y) $strobe(X,Y)
`endif

`STROBE(foo)
`STROBE2(bar,test)

        "#,
        "condition_disacled",
    )
}

#[test]
fn source_map_triple_replacement() {
    check_prepocessor_single_file(
        r#"
`include "constants.va"

`define y_fv(fv,y);

`define expLin(result, x);

// foo
"#,
        "source_map_triple_replacement",
    )
}

fn preprocessor_diagnostics(src: &str) -> String {
    let sources = TestSourceProvider::new(vec![]);
    let file = sources.vfs.borrow_mut().add_virt_file("/keywords_test.va", src.to_owned().into());
    let Preprocess { diagnostics, .. } = preprocess(&sources, file);
    diagnostics.iter().map(|diagnostic| format!("{diagnostic}\n")).collect()
}

/// VAMS-2023 10.6: `` `begin_keywords "1364-2005" `` releases the Verilog-AMS
/// keywords, so `from`, `string` and `ground` lex as plain identifiers until the
/// matching `` `end_keywords ``.
#[test]
fn begin_keywords_1364_2005() {
    check_prepocessor_single_file(
        r#"
`begin_keywords "1364-2005"
module legacy(from, string, ground);
    input from, string, ground;
endmodule
`end_keywords
module ams(a);
    analog begin end
endmodule
"#,
        "begin_keywords_1364_2005",
    )
}

/// Nested directives form a stack: `` `end_keywords `` restores the enclosing
/// keyword set rather than the default one.
#[test]
fn begin_keywords_nested() {
    check_prepocessor_single_file(
        r#"
`begin_keywords "VAMS-2023"
`begin_keywords "1364-1995"
localparam genvar
`end_keywords
localparam genvar
`end_keywords
localparam genvar
"#,
        "begin_keywords_nested",
    )
}

/// The directive "affects all source code that follows the directive, even
/// across source code file boundaries".
#[test]
fn begin_keywords_across_include() {
    let sources = TestSourceProvider::new(vec![]);
    let root = {
        let mut vfs = sources.vfs.borrow_mut();
        vfs.add_virt_file("/inc.va", "analog string\n".to_owned().into());
        vfs.add_virt_file(
            "/parent.va",
            concat!(
                "`begin_keywords \"1364-2005\"\n",
                "`include \"inc.va\"\n",
                "analog string\n",
                "`end_keywords\n",
                "analog string\n"
            )
            .to_owned()
            .into(),
        )
    };
    check_prepocessor(sources, root, "begin_keywords_across_include");
}

/// Only the specifiers listed in VAMS-2023 10.6 are accepted; an unknown one is
/// reported and leaves the active keyword set alone.
#[test]
fn begin_keywords_unknown_version() {
    expect![[r#"
        unknown keyword version specifier "VAMS-2.5"
    "#]]
    .assert_eq(&preprocessor_diagnostics("`begin_keywords \"VAMS-2.5\"\nanalog\n"));
}

/// An unmatched `` `end_keywords `` and a `` `begin_keywords `` that is never
/// closed are both reported.
#[test]
fn begin_keywords_unbalanced() {
    expect![[r#"
        '`end_keywords' without a matching '`begin_keywords'
    "#]]
    .assert_eq(&preprocessor_diagnostics("`end_keywords\n"));

    expect![[r#"
        '`begin_keywords' without a matching '`end_keywords'
    "#]]
    .assert_eq(&preprocessor_diagnostics("`begin_keywords \"1364-2005\"\nanalog\n"));
}

/// A directive inside a disabled `` `ifdef `` branch is never taken.
#[test]
fn begin_keywords_in_disabled_branch() {
    check_prepocessor_single_file(
        r#"
`ifdef NOT_DEFINED
`begin_keywords "1364-2005"
`endif
analog string
"#,
        "begin_keywords_in_disabled_branch",
    )
}

/// A keyword directive inside a `` `define `` body is rejected. The parser has
/// to consume it: leaving it in place used to spin the macro-body loop forever.
#[test]
fn begin_keywords_inside_define() {
    expect![[r#"
        encountered unexpected token!
    "#]]
    .assert_eq(&preprocessor_diagnostics("`define BAD `begin_keywords \"1364-2005\"\n`BAD\n"));
}

/// VAMS-2023 10.6: the directives may only be specified outside of a design
/// element.
#[test]
fn begin_keywords_inside_module() {
    expect![[r#"
        '`begin_keywords' is not allowed inside a design element
    "#]]
    .assert_eq(&preprocessor_diagnostics(
        "module m;\n`begin_keywords \"1364-2005\"\nendmodule\n",
    ));
}
/// VAMS-2023 §10.7: `` `__FILE__ `` / `` `__LINE__ `` expand to string / decimal
/// literals of the current input file and line.
#[test]
fn file_line_directives() {
    // Keep the directives on known lines so the expanded decimals are stable.
    // Line 1 is blank after the raw-string newline; line 2 is the display call.
    check_prepocessor_single_file(
        r#"
$display("at %s:%d", `__FILE__, `__LINE__);
"#,
        "file_line_directives",
    )
}

/// After `` `include ``, `` `__FILE__ `` / `` `__LINE__ `` must report the included
/// file; once the include ends they revert to the parent.
#[test]
fn file_line_across_include() {
    let sources = TestSourceProvider::new(vec![]);
    let root = {
        let mut vfs = sources.vfs.borrow_mut();
        vfs.add_virt_file(
            "/inc.va",
            concat!("// included\n", "$display(`__FILE__, `__LINE__);\n").to_owned().into(),
        );
        vfs.add_virt_file(
            "/parent.va",
            concat!("`include \"inc.va\"\n", "$display(`__FILE__, `__LINE__);\n")
                .to_owned()
                .into(),
        )
    };
    check_prepocessor(sources, root, "file_line_across_include");
}

/// Nested appearance inside a `` `define `` body expands at the call site.
#[test]
fn file_line_inside_define() {
    check_prepocessor_single_file(
        r#"
`define LOC `__FILE__, `__LINE__
$display(`LOC);
"#,
        "file_line_inside_define",
    )
}
