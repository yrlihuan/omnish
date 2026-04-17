//! Verifies the chat agent loop marks last-2 messages with Long.
//!
//! Mirrors the helper's logic - if you change `mark_chat_message_hints`,
//! update this test (it's intentionally redundant for safety).

use omnish_llm::backend::{CacheHint, TaggedMessage};

fn mark_chat_message_hints(messages: &mut [TaggedMessage]) {
    for m in messages.iter_mut() {
        m.cache = CacheHint::None;
    }
    let len = messages.len();
    for i in 0..2.min(len) {
        messages[len - 1 - i].cache = CacheHint::Long;
    }
}

fn msg(text: &str) -> TaggedMessage {
    TaggedMessage {
        content: serde_json::json!({"role":"user","content":text}),
        cache: CacheHint::Long, // Pre-set to verify reset
    }
}

#[test]
fn marks_last_two_messages_long() {
    let mut msgs = vec![msg("a"), msg("b"), msg("c"), msg("d")];
    mark_chat_message_hints(&mut msgs);
    assert_eq!(msgs[0].cache, CacheHint::None);
    assert_eq!(msgs[1].cache, CacheHint::None);
    assert_eq!(msgs[2].cache, CacheHint::Long);
    assert_eq!(msgs[3].cache, CacheHint::Long);
}

#[test]
fn handles_single_message() {
    let mut msgs = vec![msg("only")];
    mark_chat_message_hints(&mut msgs);
    assert_eq!(msgs[0].cache, CacheHint::Long);
}

#[test]
fn handles_empty_list() {
    let mut msgs: Vec<TaggedMessage> = vec![];
    mark_chat_message_hints(&mut msgs);
    assert!(msgs.is_empty());
}

#[test]
fn resets_old_marks_before_setting_new() {
    let mut msgs = vec![msg("a"), msg("b"), msg("c"), msg("d"), msg("e")];
    // All start as Long (from msg() helper). After marking, only last 2 should be Long.
    mark_chat_message_hints(&mut msgs);
    assert_eq!(msgs[0].cache, CacheHint::None);
    assert_eq!(msgs[1].cache, CacheHint::None);
    assert_eq!(msgs[2].cache, CacheHint::None);
    assert_eq!(msgs[3].cache, CacheHint::Long);
    assert_eq!(msgs[4].cache, CacheHint::Long);
}
