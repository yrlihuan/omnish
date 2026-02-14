use omnish_pty::proxy::PtyProxy;
use std::collections::HashMap;
use std::time::Duration;

#[test]
fn test_pty_spawn_and_read_output() {
    let proxy = PtyProxy::spawn("/bin/echo", &["hello_from_pty"]).unwrap();
    let mut buf = vec![0u8; 256];
    std::thread::sleep(Duration::from_millis(200));
    let n = proxy.read(&mut buf).unwrap();
    let output = String::from_utf8_lossy(&buf[..n]);
    assert!(output.contains("hello_from_pty"), "got: {}", output);
    proxy.wait().unwrap();
}

#[test]
fn test_pty_spawn_returns_child_pid() {
    let proxy = PtyProxy::spawn("/bin/true", &[]).unwrap();
    assert!(proxy.child_pid() > 0);
}

#[test]
fn test_pty_env_var_propagated() {
    let env = HashMap::from([("OMNISH_SESSION_ID".to_string(), "test123".to_string())]);
    let proxy = PtyProxy::spawn_with_env("/bin/sh", &["-c", "echo $OMNISH_SESSION_ID"], env).unwrap();
    let mut buf = [0u8; 256];
    std::thread::sleep(Duration::from_millis(500));
    let n = proxy.read(&mut buf).unwrap_or(0);
    let output = String::from_utf8_lossy(&buf[..n]);
    assert!(output.contains("test123"), "env var should be propagated to child, got: {}", output);
    proxy.wait().ok();
}
