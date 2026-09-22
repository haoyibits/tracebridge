use std::process::Command;

fn main() {
    let hash = std::env::var("TRACEBRIDGE_GIT_HASH")
        .ok()
        .filter(|h| !h.is_empty());
    let hash = hash.unwrap_or_else(git_hash);
    println!("cargo:rustc-env=TRACEBRIDGE_GIT_HASH={hash}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
    println!("cargo:rerun-if-env-changed=TRACEBRIDGE_GIT_HASH");
}

fn git_hash() -> String {
    Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}
