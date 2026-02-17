use omnish_daemon::session_mgr::SessionManager;
#[allow(unused_imports)]
use omnish_store::session::SessionMeta;
use omnish_store::command::CommandRecord;
use std::collections::HashMap;

#[tokio::test]
async fn test_session_register_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("sess1", None, HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("pid".to_string(), "100".to_string()),
        ("tty".to_string(), "/dev/pts/0".to_string()),
        ("cwd".to_string(), "/home/user".to_string()),
    ])).await.unwrap();
    mgr.register("sess2", None, HashMap::from([
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

    mgr.register("sess1", None, HashMap::from([
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
async fn test_command_recording_via_receive_command() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("sess1", None, HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("cwd".to_string(), "/home/user".to_string()),
    ])).await.unwrap();

    // Simulate IO written to stream (daemon still stores raw stream)
    mgr.write_io("sess1", 1000, 1, b"user@host:~$ ").await.unwrap();
    mgr.write_io("sess1", 1001, 0, b"ls -la\r\n").await.unwrap();
    mgr.write_io("sess1", 1002, 1, b"total 0\r\nfile.txt\r\nuser@host:~$ ").await.unwrap();

    // Client sends completed command record (new architecture)
    mgr.receive_command("sess1", CommandRecord {
        command_id: "sess1:0".to_string(),
        session_id: "sess1".to_string(),
        command_line: Some("ls -la".to_string()),
        cwd: Some("/home/user".to_string()),
        started_at: 1000,
        ended_at: Some(1002),
        output_summary: "total 0\nfile.txt".to_string(),
        stream_offset: 0,
        stream_length: 0,
    }).await.unwrap();

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

    mgr.register("sess1", None, HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("cwd".to_string(), "/tmp".to_string()),
    ])).await.unwrap();

    mgr.write_io("sess1", 1000, 1, b"$ ").await.unwrap();
    mgr.write_io("sess1", 1001, 0, b"echo hi\r\n").await.unwrap();
    mgr.write_io("sess1", 1002, 1, b"hi\r\n$ ").await.unwrap();

    // Client sends completed command
    mgr.receive_command("sess1", CommandRecord {
        command_id: "sess1:0".to_string(),
        session_id: "sess1".to_string(),
        command_line: Some("echo hi".to_string()),
        cwd: Some("/tmp".to_string()),
        started_at: 1000,
        ended_at: Some(1002),
        output_summary: "hi".to_string(),
        stream_offset: 0,
        stream_length: 0,
    }).await.unwrap();

    mgr.end_session("sess1").await.unwrap();

    // After session ends, commands.json should exist on disk
    let mut session_dirs: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(session_dirs.len(), 1);

    let session_dir = session_dirs.remove(0).path();
    let commands = CommandRecord::load_all(&session_dir).unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].command_line.as_deref(), Some("echo hi"));
}

#[tokio::test]
async fn test_multi_command_session_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("e2e", None, HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("cwd".to_string(), "/home/user/project".to_string()),
    ])).await.unwrap();

    // Write IO stream data
    mgr.write_io("e2e", 1000, 1, b"user@host:~/project$ ").await.unwrap();
    mgr.write_io("e2e", 1001, 0, b"ls\r\n").await.unwrap();
    mgr.write_io("e2e", 1002, 1, b"Cargo.toml\r\nsrc/\r\nuser@host:~/project$ ").await.unwrap();
    mgr.write_io("e2e", 1003, 0, b"cargo build\r\n").await.unwrap();
    mgr.write_io("e2e", 1004, 1, b"   Compiling omnish v0.1.0\r\n    Finished dev\r\nuser@host:~/project$ ").await.unwrap();
    mgr.write_io("e2e", 1005, 0, b"cargo test\r\n").await.unwrap();
    mgr.write_io("e2e", 1006, 1, b"running 5 tests\r\n").await.unwrap();

    // Client sends 2 completed commands (command 3 is still running)
    mgr.receive_command("e2e", CommandRecord {
        command_id: "e2e:0".to_string(),
        session_id: "e2e".to_string(),
        command_line: Some("ls".to_string()),
        cwd: Some("/home/user/project".to_string()),
        started_at: 1000,
        ended_at: Some(1002),
        output_summary: "Cargo.toml\nsrc/".to_string(),
        stream_offset: 0,
        stream_length: 0,
    }).await.unwrap();

    mgr.receive_command("e2e", CommandRecord {
        command_id: "e2e:1".to_string(),
        session_id: "e2e".to_string(),
        command_line: Some("cargo build".to_string()),
        cwd: Some("/home/user/project".to_string()),
        started_at: 1003,
        ended_at: Some(1004),
        output_summary: "   Compiling omnish v0.1.0\n    Finished dev".to_string(),
        stream_offset: 0,
        stream_length: 0,
    }).await.unwrap();

    let commands = mgr.get_commands("e2e").await.unwrap();

    // Only 2 completed commands (command 3 is still running)
    assert_eq!(commands.len(), 2);

    assert_eq!(commands[0].command_id, "e2e:0");
    assert_eq!(commands[0].command_line.as_deref(), Some("ls"));

    assert_eq!(commands[1].command_id, "e2e:1");
    assert_eq!(commands[1].command_line.as_deref(), Some("cargo build"));
    assert!(commands[1].output_summary.contains("Compiling"));

    // End session — should persist
    mgr.end_session("e2e").await.unwrap();
}

#[tokio::test]
async fn test_session_register_with_parent() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());
    mgr.register("child1", Some("parent1".to_string()), HashMap::new()).await.unwrap();
    let active = mgr.list_active().await;
    assert!(active.contains(&"child1".to_string()));
}

#[tokio::test]
async fn test_nested_session_parent_child_relationship() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    // Register parent session (no parent)
    mgr.register("parent1", None, HashMap::new()).await.unwrap();

    // Register child session with parent
    mgr.register("child1", Some("parent1".to_string()), HashMap::new()).await.unwrap();

    // Both should be active
    let active = mgr.list_active().await;
    assert!(active.contains(&"parent1".to_string()));
    assert!(active.contains(&"child1".to_string()));

    // End both sessions
    mgr.end_session("child1").await.unwrap();
    mgr.end_session("parent1").await.unwrap();

    // Verify parent_session_id persisted in meta.json
    let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
    assert_eq!(entries.len(), 2, "should have 2 session dirs");

    for entry in &entries {
        let meta = SessionMeta::load(&entry.path()).unwrap();
        if meta.session_id == "child1" {
            assert_eq!(meta.parent_session_id, Some("parent1".to_string()),
                "child session should have parent_session_id");
        } else if meta.session_id == "parent1" {
            assert_eq!(meta.parent_session_id, None,
                "parent session should have no parent");
        } else {
            panic!("unexpected session: {}", meta.session_id);
        }
    }
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn test_debug_context_request() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    // Register a session
    mgr.register("dbg1", None, HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("cwd".to_string(), "/tmp".to_string()),
    ])).await.unwrap();

    // Write some IO data (direction=1 for output)
    mgr.write_io("dbg1", 1000, 1, b"$ ").await.unwrap();
    mgr.write_io("dbg1", 1001, 0, b"echo hello\r\n").await.unwrap();
    mgr.write_io("dbg1", 1002, 1, b"hello\r\n$ ").await.unwrap();

    // Client sends completed command
    mgr.receive_command("dbg1", CommandRecord {
        command_id: "dbg1:0".to_string(),
        session_id: "dbg1".to_string(),
        command_line: Some("echo hello".to_string()),
        cwd: Some("/tmp".to_string()),
        started_at: 1000,
        ended_at: Some(1002),
        output_summary: "hello".to_string(),
        stream_offset: 0,
        stream_length: 0,
    }).await.unwrap();

    // Verify get_session_context returns the output data
    let ctx = mgr.get_session_context("dbg1").await.unwrap();
    assert!(!ctx.is_empty(), "context should not be empty");
    assert!(ctx.contains("hello"), "context should contain output data");
}
