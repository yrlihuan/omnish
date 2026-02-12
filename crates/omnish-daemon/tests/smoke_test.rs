// Integration smoke test: verify the daemon binary starts, creates its socket,
// and can be stopped cleanly.

use std::process::Command;
use std::time::Duration;

#[test]
fn test_omnishd_starts_and_stops() {
    let socket_path = format!("/tmp/omnish-test-{}.sock", std::process::id());

    let mut child = Command::new(env!("CARGO_BIN_EXE_omnish-daemon"))
        .env("OMNISH_SOCKET", &socket_path)
        .spawn()
        .expect("failed to start omnishd");

    std::thread::sleep(Duration::from_millis(500));

    // Verify socket exists
    assert!(
        std::path::Path::new(&socket_path).exists(),
        "socket file should exist at {}",
        socket_path
    );

    child.kill().ok();
    child.wait().ok();
    std::fs::remove_file(&socket_path).ok();
}
