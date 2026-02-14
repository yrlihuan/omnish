use omnish_protocol::message::*;
use std::collections::HashMap;

#[test]
fn test_session_start_roundtrip() {
    let msg = Message::SessionStart(SessionStart {
        session_id: "abc123".to_string(),
        parent_session_id: None,
        timestamp_ms: 1707600000000,
        attrs: HashMap::from([
            ("shell".to_string(), "/bin/bash".to_string()),
            ("pid".to_string(), "1234".to_string()),
            ("tty".to_string(), "/dev/pts/0".to_string()),
            ("cwd".to_string(), "/home/user".to_string()),
        ]),
    });
    let bytes = msg.to_bytes().unwrap();
    let decoded = Message::from_bytes(&bytes).unwrap();
    match decoded {
        Message::SessionStart(s) => {
            assert_eq!(s.session_id, "abc123");
            assert_eq!(s.attrs.get("pid").unwrap(), "1234");
        }
        _ => panic!("wrong message type"),
    }
}

#[test]
fn test_io_data_roundtrip() {
    let msg = Message::IoData(IoData {
        session_id: "abc123".to_string(),
        direction: IoDirection::Output,
        timestamp_ms: 1707600000000,
        data: b"hello world\n".to_vec(),
    });
    let bytes = msg.to_bytes().unwrap();
    let decoded = Message::from_bytes(&bytes).unwrap();
    match decoded {
        Message::IoData(io) => {
            assert_eq!(io.data, b"hello world\n");
            assert_eq!(io.direction, IoDirection::Output);
        }
        _ => panic!("wrong message type"),
    }
}

#[test]
fn test_session_start_with_parent() {
    let msg = Message::SessionStart(SessionStart {
        session_id: "child1".to_string(),
        parent_session_id: Some("parent1".to_string()),
        timestamp_ms: 1707600000000,
        attrs: HashMap::new(),
    });
    let bytes = msg.to_bytes().unwrap();
    let decoded = Message::from_bytes(&bytes).unwrap();
    match decoded {
        Message::SessionStart(s) => {
            assert_eq!(s.session_id, "child1");
            assert_eq!(s.parent_session_id, Some("parent1".to_string()));
        }
        _ => panic!("wrong message type"),
    }
}

#[test]
fn test_session_start_without_parent() {
    let msg = Message::SessionStart(SessionStart {
        session_id: "root1".to_string(),
        parent_session_id: None,
        timestamp_ms: 1707600000000,
        attrs: HashMap::new(),
    });
    let bytes = msg.to_bytes().unwrap();
    let decoded = Message::from_bytes(&bytes).unwrap();
    match decoded {
        Message::SessionStart(s) => {
            assert_eq!(s.session_id, "root1");
            assert_eq!(s.parent_session_id, None);
        }
        _ => panic!("wrong message type"),
    }
}

#[test]
fn test_frame_magic_validation() {
    let bad_bytes = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    assert!(Message::from_bytes(&bad_bytes).is_err());
}
