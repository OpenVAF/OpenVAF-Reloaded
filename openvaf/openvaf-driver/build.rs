// build.rs for openvaf-driver

fn git_version() -> String {
    let output = std::process::Command::new("git")
        .args(["describe", "--tags", "--long", "--dirty", "--match", "_*"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8(o.stdout).unwrap_or_default();
            let s = s.trim();
            s.strip_prefix('_').unwrap_or(s).to_string()
        }
        _ => "unknown".to_string(),
    }
}

fn main() {
    println!("cargo:rustc-env=OPENVAF_GIT_VERSION={}", git_version());
    // Rebuild if HEAD or any tag changes.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/tags");
    // Add rpath for LLVM on macOS
    #[cfg(target_os = "macos")]
    {
        // Official LLVM release tarballs ship only static component libs (no
        // libLLVM.dylib), so llvm-sys's `prefer-dynamic` falls back to static
        // linking. Those archives are C++ and need libc++ for their runtime
        // symbols (operator new, __cxa_guard_*, __cxa_pure_virtual). Homebrew's
        // LLVM has libLLVM.dylib so it never hits this, but link libc++ here so
        // building against a static LLVM works too.
        println!("cargo:rustc-link-lib=dylib=c++");
        println!("cargo:rustc-link-lib=dylib=c++abi");
        if let Ok(output) = std::process::Command::new("llvm-config").arg("--libdir").output() {
            if output.status.success() {
                let libdir = String::from_utf8_lossy(&output.stdout).trim().to_string();
                println!("cargo:rustc-link-arg=-Wl,-rpath,{}", libdir);
            }
        }
    }
}
