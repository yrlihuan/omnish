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
