//! Gives `chiika --version` the commit it was built from, when built from a git checkout.

fn main() {
    // Only our own checkout: a crate unpacked by `cargo install` may sit inside another repository.
    let commit = std::path::Path::new(".git")
        .exists()
        .then(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
        })
        .and_then(Result::ok)
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default();
    let version = match commit.as_str() {
        "" => env!("CARGO_PKG_VERSION").to_string(),
        commit => format!("{} ({commit})", env!("CARGO_PKG_VERSION")),
    };
    println!("cargo:rustc-env=CHIIKA_VERSION={version}");
    // A new commit moves the branch the HEAD points to.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
}
