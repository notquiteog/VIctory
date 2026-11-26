//! Link settings for the Windows executable, and which commit it is built
//! from.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// For `victory --version`: `GIT_COMMIT` / `GIT_COMMIT_DATE` if set (the
/// release workflow sets them), else what git says of the checkout, else
/// "unknown".
fn commit() {
    println!("cargo:rerun-if-env-changed=GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=GIT_COMMIT_DATE");
    // (A commit or a checkout writes to the log of HEAD.)
    for file in ["HEAD", "logs/HEAD"] {
        if let Some(path) = git(&["rev-parse", "--git-path", file]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let commit = env("GIT_COMMIT").or_else(|| git(&["rev-parse", "HEAD"])).unwrap_or_else(|| "unknown".to_owned());
    let date = env("GIT_COMMIT_DATE")
        .or_else(|| git(&["show", "-s", "--format=%cs", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=VICTORY_COMMIT={}", commit.trim());
    println!("cargo:rustc-env=VICTORY_COMMIT_DATE={}", date.trim());
}

fn main() {
    commit();
    // Threads created without a stack size take the executable's default.
    // Host graphics drivers compiling the title's large shaders on such
    // threads (vkd3d-proton's and the drivers' own, under Wine too) overflow
    // the linker's default of 1 MiB: reserve 16 MiB instead.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:16777216");
    }
}
