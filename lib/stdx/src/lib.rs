use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{env, fs};

mod ieee64;
pub mod iter;
mod macros;
pub mod packed_option;
pub mod pretty;
pub mod vec;
pub use crate::ieee64::Ieee64;

pub const IS_CI: bool = option_env!("CI").is_some();
pub const SKIP_HOST_TESTS: bool = option_env!("CI").is_some() && cfg!(windows);

static UNIQUE_SUFFIX_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Process-unique file name fragment (pid plus a monotonic counter).
/// Batch mode derives output names from a content hash, so concurrent
/// compiles share an output directory and intermediates must not collide.
pub fn unique_suffix() -> String {
    let n = UNIQUE_SUFFIX_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{n}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_suffix_is_unique_and_contains_pid() {
        let pid = format!("{}-", std::process::id());
        let a = unique_suffix();
        let b = unique_suffix();
        assert!(a.starts_with(&pid));
        assert!(b.starts_with(&pid));
        assert_ne!(a, b);
    }
}

pub fn project_root() -> PathBuf {
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut res = PathBuf::from(dir);
    while !res.join("README.md").exists() {
        res = res.parent().expect("reached fs root without finding project root").to_owned()
    }
    res
}

/// Returns `false` if slow tests should not run, otherwise returns `true` and
/// also creates a file at `./target/.slow_tests_cookie` which serves as a flag
/// that slow tests did run.
pub fn skip_slow_tests() -> bool {
    let should_skip = option_env!("CI").is_none() && env::var("RUN_SLOW_TESTS").is_err();
    if should_skip {
        std::eprintln!("ignoring slow test");
    } else {
        let path = project_root().join("./target/.slow_tests_cookie");
        fs::write(path, ".").unwrap();
    }
    should_skip
}

pub fn ignore_dev_tests<T: ?Sized>(_: &T) -> bool {
    env::var("RUN_DEV_TESTS").is_err()
}

pub fn ignore_slow_tests<T: ?Sized>(_: &T) -> bool {
    skip_slow_tests()
}

pub fn ignore_never<T: ?Sized>(_: &T) -> bool {
    false
}

pub fn openvaf_test_data(test: &str) -> PathBuf {
    project_root().join("openvaf").join("test_data").join(test)
}

pub fn integration_test_dir(test: &str) -> PathBuf {
    project_root().join("integration_tests").join(test)
}

pub fn is_va_file(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).map_or(false, |ext| ext == "va")
}

pub trait Upcast<T: ?Sized> {
    fn upcast(&self) -> &T;
}
