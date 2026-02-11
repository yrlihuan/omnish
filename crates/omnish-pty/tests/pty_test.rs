use omnish_pty::proxy::PtyProxy;
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
