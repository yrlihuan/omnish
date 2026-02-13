use omnish_store::session::SessionMeta;
use omnish_store::stream::StreamWriter;
use std::collections::HashMap;
use tempfile::tempdir;

#[test]
fn test_write_and_read_session_meta() {
    let dir = tempdir().unwrap();
    let meta = SessionMeta {
        session_id: "abc123".to_string(),
        started_at: "2026-02-11T16:30:00Z".to_string(),
        ended_at: None,
        attrs: HashMap::from([
            ("shell".to_string(), "/bin/bash".to_string()),
            ("pid".to_string(), "1234".to_string()),
            ("tty".to_string(), "/dev/pts/0".to_string()),
            ("cwd".to_string(), "/home/user".to_string()),
        ]),
    };
    meta.save(dir.path()).unwrap();
    let loaded = SessionMeta::load(dir.path()).unwrap();
    assert_eq!(loaded.session_id, "abc123");
    assert_eq!(loaded.attrs.get("pid").unwrap(), "1234");
}

#[test]
fn test_stream_writer_and_reader() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("stream.bin");

    {
        let mut writer = StreamWriter::create(&path).unwrap();
        writer.write_entry(1000, 0, b"ls -la\n").unwrap(); // 0 = input
        writer.write_entry(1001, 1, b"total 0\n").unwrap(); // 1 = output
    }

    let entries = omnish_store::stream::read_entries(&path).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].timestamp_ms, 1000);
    assert_eq!(entries[0].direction, 0);
    assert_eq!(entries[0].data, b"ls -la\n");
    assert_eq!(entries[1].direction, 1);
}
