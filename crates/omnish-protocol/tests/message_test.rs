use omnish_protocol::message::*;

#[test]
fn test_session_start_roundtrip() {
    let msg = Message::SessionStart(SessionStart {
        session_id: "abc123".to_string(),
        shell: "/bin/bash".to_string(),
        pid: 1234,
        tty: "/dev/pts/0".to_string(),
        timestamp_ms: 1707600000000,
    });
    let bytes = msg.to_bytes().unwrap();
    let decoded = Message::from_bytes(&bytes).unwrap();
    match decoded {
        Message::SessionStart(s) => {
            assert_eq!(s.session_id, "abc123");
            assert_eq!(s.pid, 1234);
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
fn test_frame_magic_validation() {
    let bad_bytes = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    assert!(Message::from_bytes(&bad_bytes).is_err());
}
