use core::slice;
use std::mem::{size_of, size_of_val};

use basedb::lints::LintLevel;
use basedb::{BaseDB, VfsStorage};
use hir::CompilationDB;

use crate::{LLVMCodeGenOptLevel, Opts, Target};

fn consume_str(hash: &mut md5::Context, s: &str) {
    // length prefix so adjacent fields cannot collide by concatenation
    hash.consume(s.len().to_ne_bytes());
    hash.consume(s);
}

// split from `hash` so the settings part can be tested without a database
fn consume_codegen_settings(
    hash: &mut md5::Context,
    target: &Target,
    target_cpu: &str,
    opt_lvl: LLVMCodeGenOptLevel,
    codegen_opts: &[String],
) {
    consume_str(hash, &target.llvm_target);
    consume_str(hash, &target.data_layout);
    consume_str(hash, target_cpu);
    hash.consume((opt_lvl as u32).to_ne_bytes());
    hash.consume(codegen_opts.len().to_ne_bytes());
    for opt in codegen_opts {
        consume_str(hash, opt);
    }
}

// TODO: use high level hir API instead of low leve database API
fn hash(db: &CompilationDB, opts: &Opts) -> md5::Digest {
    let mut hash_builder = md5::Context::new();
    let cu = db.compilation_unit();

    // hash settings
    hash_builder.consume(cu.root_file().0.to_ne_bytes());

    hash_builder.consume(opts.defines.len().to_ne_bytes());
    for def in &opts.defines {
        hash_builder.consume(def)
    }

    hash_builder.consume(env!("CARGO_PKG_VERSION"));
    let lints = db.global_lint_overwrites(cu.root_file());
    if cfg!(debug_assertions) && !lints.is_empty() {
        assert_eq!(size_of::<Option<LintLevel>>(), size_of_val(&lints.raw[0]));
    }
    let lints = unsafe {
        slice::from_raw_parts(
            lints.as_ptr() as *const u8,
            size_of::<Option<LintLevel>>() * lints.len(),
        )
    };
    hash_builder.consume(lints);

    consume_codegen_settings(
        &mut hash_builder,
        &opts.target,
        &opts.target_cpu,
        opts.opt_lvl,
        &opts.codegen_opts,
    );

    // Hash the full preprocessor result
    let preprocess = cu.preprocess(db);
    let vfs = db.vfs().read();
    for token in &*preprocess.ts {
        if !token.kind.is_trivia() {
            let filespan = token.span.to_file_span(&preprocess.sm);
            let src = vfs.file_contents_unchecked(filespan.file);
            hash_builder.consume(&src[filespan.range]);
            hash_builder.consume(" ");
        }
    }

    hash_builder.compute()
}

pub fn file_name(db: &CompilationDB, opts: &Opts) -> String {
    let hash = u128::from_ne_bytes(*hash(db, opts));
    let hash = base_n::encode(hash, base_n::CASE_INSENSITIVE);
    format!("{}.osdi", hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_digest(
        target: &Target,
        target_cpu: &str,
        opt_lvl: LLVMCodeGenOptLevel,
        codegen_opts: &[String],
    ) -> md5::Digest {
        let mut hash = md5::Context::new();
        consume_codegen_settings(&mut hash, target, target_cpu, opt_lvl, codegen_opts);
        hash.compute()
    }

    fn strings(parts: &[&str]) -> Vec<String> {
        parts.iter().copied().map(str::to_owned).collect()
    }

    #[test]
    fn opt_level_changes_hash() {
        let target = Target::search("aarch64-apple-darwin").unwrap();
        let none =
            settings_digest(&target, "generic", LLVMCodeGenOptLevel::LLVMCodeGenLevelNone, &[]);
        let aggressive = settings_digest(
            &target,
            "generic",
            LLVMCodeGenOptLevel::LLVMCodeGenLevelAggressive,
            &[],
        );
        assert_ne!(none, aggressive);
    }

    #[test]
    fn target_changes_hash() {
        let aarch64 = Target::search("aarch64-apple-darwin").unwrap();
        let x86_64 = Target::search("x86_64-apple-darwin").unwrap();
        let first =
            settings_digest(&aarch64, "generic", LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault, &[]);
        let second =
            settings_digest(&x86_64, "generic", LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault, &[]);
        assert_ne!(first, second);
    }

    #[test]
    fn target_cpu_and_codegen_opts_change_hash() {
        let target = Target::search("aarch64-apple-darwin").unwrap();
        let base =
            settings_digest(&target, "generic", LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault, &[]);
        let other_cpu =
            settings_digest(&target, "native", LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault, &[]);
        assert_ne!(base, other_cpu);

        let with_opts = settings_digest(
            &target,
            "generic",
            LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault,
            &strings(&["unroll-loops"]),
        );
        assert_ne!(base, with_opts);
    }

    #[test]
    fn codegen_opts_length_prefix_prevents_concat_collisions() {
        let target = Target::search("aarch64-apple-darwin").unwrap();
        let first = settings_digest(
            &target,
            "generic",
            LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault,
            &strings(&["ab", "c"]),
        );
        let second = settings_digest(
            &target,
            "generic",
            LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault,
            &strings(&["a", "bc"]),
        );
        assert_ne!(first, second);
    }
}
