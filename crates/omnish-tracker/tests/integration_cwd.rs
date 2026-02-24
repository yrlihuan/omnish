// crates/omnish-tracker/tests/integration_cwd.rs
use omnish_tracker::command_tracker::CommandTracker;
use omnish_tracker::osc133_detector::{Osc133Detector, Osc133Event, Osc133EventKind};

#[test]
fn test_end_to_end_cwd_tracking() {
    // Simulate session starting in /home/user
    let mut tracker = CommandTracker::new("sess1".into(), Some("/home/user".into()));

    // First command in /home/user
    let mut detector = Osc133Detector::new();
    let events = detector.feed(b"\x1b]133;A\x07");
    for event in events {
        tracker.feed_osc133(event, 1000, 0);
    }

    let events = detector.feed(b"\x1b]133;B;ls -la;cwd:/home/user\x07");
    for event in events {
        tracker.feed_osc133(event, 1001, 50);
    }

    tracker.feed_input(b"\r", 1001);

    let events = detector.feed(b"\x1b]133;D;0\x07");
    let mut cmds1 = Vec::new();
    for event in events {
        cmds1.extend(tracker.feed_osc133(event, 1003, 100));
    }

    assert_eq!(cmds1.len(), 1);
    assert_eq!(cmds1[0].cwd.as_deref(), Some("/home/user"));

    // User changes directory
    let events = detector.feed(b"\x1b]133;A\x07");
    for event in events {
        tracker.feed_osc133(event, 2000, 100);
    }

    // Second command in /home/user/project
    let events = detector.feed(b"\x1b]133;B;make;cwd:/home/user/project\x07");
    for event in events {
        tracker.feed_osc133(event, 2001, 150);
    }

    tracker.feed_input(b"\r", 2001);

    let events = detector.feed(b"\x1b]133;D;0\x07");
    let mut cmds2 = Vec::new();
    for event in events {
        cmds2.extend(tracker.feed_osc133(event, 2003, 200));
    }

    assert_eq!(cmds2.len(), 1);
    assert_eq!(cmds2[0].cwd.as_deref(), Some("/home/user/project"));
    assert_ne!(cmds1[0].cwd, cmds2[0].cwd, "CWD should change between commands");
}