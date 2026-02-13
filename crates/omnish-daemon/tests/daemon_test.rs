use omnish_daemon::session_mgr::SessionManager;
use std::collections::HashMap;

#[tokio::test]
async fn test_session_register_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("sess1", HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("pid".to_string(), "100".to_string()),
        ("tty".to_string(), "/dev/pts/0".to_string()),
        ("cwd".to_string(), "/home/user".to_string()),
    ])).await.unwrap();
    mgr.register("sess2", HashMap::from([
        ("shell".to_string(), "/bin/zsh".to_string()),
        ("pid".to_string(), "101".to_string()),
        ("tty".to_string(), "/dev/pts/1".to_string()),
        ("cwd".to_string(), "/tmp".to_string()),
    ])).await.unwrap();

    let sessions = mgr.list_active().await;
    assert_eq!(sessions.len(), 2);
}

#[tokio::test]
async fn test_session_end() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("sess1", HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("pid".to_string(), "100".to_string()),
        ("tty".to_string(), "/dev/pts/0".to_string()),
        ("cwd".to_string(), "/home/user".to_string()),
    ])).await.unwrap();
    mgr.end_session("sess1").await.unwrap();

    let sessions = mgr.list_active().await;
    assert_eq!(sessions.len(), 0);
}

#[tokio::test]
async fn test_command_recording_through_session_manager() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("sess1", HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("cwd".to_string(), "/home/user".to_string()),
    ])).await.unwrap();

    // Simulate: prompt → input → output → prompt
    mgr.write_io("sess1", 1000, 1, b"user@host:~$ ").await.unwrap();
    mgr.write_io("sess1", 1001, 0, b"ls -la\r\n").await.unwrap();
    mgr.write_io("sess1", 1002, 1, b"total 0\r\nfile.txt\r\nuser@host:~$ ").await.unwrap();

    let commands = mgr.get_commands("sess1").await.unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].command_line.as_deref(), Some("ls -la"));
    assert_eq!(commands[0].session_id, "sess1");
    assert_eq!(commands[0].cwd.as_deref(), Some("/home/user"));
}

#[tokio::test]
async fn test_commands_persisted_on_session_end() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("sess1", HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("cwd".to_string(), "/tmp".to_string()),
    ])).await.unwrap();

    mgr.write_io("sess1", 1000, 1, b"$ ").await.unwrap();
    mgr.write_io("sess1", 1001, 0, b"echo hi\r\n").await.unwrap();
    mgr.write_io("sess1", 1002, 1, b"hi\r\n$ ").await.unwrap();

    mgr.end_session("sess1").await.unwrap();

    // After session ends, commands.json should exist on disk
    let mut session_dirs: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(session_dirs.len(), 1);

    let session_dir = session_dirs.remove(0).path();
    let commands = omnish_store::command::CommandRecord::load_all(&session_dir).unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].command_line.as_deref(), Some("echo hi"));
}
