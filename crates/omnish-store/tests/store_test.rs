use omnish_store::command::CommandRecord;
use omnish_store::session::SessionMeta;
use omnish_store::stream::{read_range, StreamWriter};
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

#[test]
fn test_command_record_save_and_load() {
    let dir = tempdir().unwrap();
    let records = vec![
        CommandRecord {
            command_id: "sess1:0".into(),
            session_id: "sess1".into(),
            command_line: Some("cargo build".into()),
            cwd: Some("/home/user/project".into()),
            started_at: 1000,
            ended_at: Some(2000),
            output_summary: "Compiling omnish v0.1.0\nFinished dev".into(),
            stream_offset: 0,
            stream_length: 512,
        },
        CommandRecord {
            command_id: "sess1:1".into(),
            session_id: "sess1".into(),
            command_line: Some("cargo test".into()),
            cwd: Some("/home/user/project".into()),
            started_at: 2000,
            ended_at: Some(3000),
            output_summary: "running 5 tests\ntest result: ok".into(),
            stream_offset: 512,
            stream_length: 1024,
        },
    ];

    CommandRecord::save_all(&records, dir.path()).unwrap();
    let loaded = CommandRecord::load_all(dir.path()).unwrap();

    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].command_id, "sess1:0");
    assert_eq!(loaded[0].command_line.as_deref(), Some("cargo build"));
    assert_eq!(loaded[0].stream_offset, 0);
    assert_eq!(loaded[1].command_id, "sess1:1");
    assert_eq!(loaded[1].ended_at, Some(3000));
}

#[test]
fn test_stream_writer_position_tracking() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("stream.bin");

    let mut writer = StreamWriter::create(&path).unwrap();

    let pos0 = writer.position();
    assert_eq!(pos0, 0);

    writer.write_entry(1000, 0, b"ls\n").unwrap(); // 8+1+4+3 = 16 bytes
    let pos1 = writer.position();
    assert_eq!(pos1, 16);

    writer.write_entry(1001, 1, b"file.txt\n").unwrap(); // 8+1+4+9 = 22 bytes
    let pos2 = writer.position();
    assert_eq!(pos2, 38); // 16 + 22
}

#[test]
fn test_read_range_from_stream() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("stream.bin");

    let mut writer = StreamWriter::create(&path).unwrap();
    let pos0 = writer.position();
    writer.write_entry(1000, 0, b"ls\n").unwrap();
    let pos1 = writer.position();
    writer.write_entry(1001, 1, b"file.txt\n").unwrap();
    let pos2 = writer.position();

    // Read only the second entry's range
    let entries = read_range(&path, pos1, pos2 - pos1).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].data, b"file.txt\n");
    assert_eq!(entries[0].direction, 1);

    // Read both entries
    let all = read_range(&path, pos0, pos2 - pos0).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn test_command_record_load_empty() {
    let dir = tempdir().unwrap();
    let loaded = CommandRecord::load_all(dir.path()).unwrap();
    assert!(loaded.is_empty());
}
