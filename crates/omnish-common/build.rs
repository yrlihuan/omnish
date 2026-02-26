fn main() {
    // Try to get version from git describe
    let version = std::process::Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|v| {
            let v = v.trim().to_string();
            v.strip_prefix('v').map(|s| s.to_string()).unwrap_or(v)
        })
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    println!("cargo:rustc-env=OMNISH_VERSION={}", version);
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}
