use omnish_llm::backend::{CacheHint, CachedText, TaggedMessage};
use omnish_llm::tool::ToolDef;

#[test]
fn cache_hint_default_is_none() {
    assert_eq!(CacheHint::default(), CacheHint::None);
}

#[test]
fn cache_hint_variants_distinct() {
    assert_ne!(CacheHint::Short, CacheHint::Long);
    assert_ne!(CacheHint::Short, CacheHint::None);
    assert_ne!(CacheHint::Long, CacheHint::None);
}

#[test]
fn cache_hint_is_copy() {
    let h = CacheHint::Long;
    let h2 = h;
    assert_eq!(h, h2);
}

#[test]
fn cached_text_constructs_with_hint() {
    let ct = CachedText { text: "hello".into(), cache: CacheHint::Long };
    assert_eq!(ct.text, "hello");
    assert_eq!(ct.cache, CacheHint::Long);
}

#[test]
fn tagged_message_default_hint_is_none() {
    let m = TaggedMessage {
        content: serde_json::json!({"role":"user","content":"hi"}),
        cache: CacheHint::default(),
        cache_pos: None,
    };
    assert_eq!(m.cache, CacheHint::None);
    assert_eq!(m.cache_pos, None);
}

#[test]
fn tool_def_cache_defaults_to_none_on_deserialize() {
    let json = serde_json::json!({
        "name": "my_tool",
        "description": "desc",
        "input_schema": {"type": "object"}
    });
    let td: ToolDef = serde_json::from_value(json).unwrap();
    assert_eq!(td.cache, omnish_llm::backend::CacheHint::None);
}

#[test]
fn tool_def_cache_serialized_roundtrip() {
    let td = ToolDef {
        name: "x".into(),
        description: "y".into(),
        input_schema: serde_json::json!({}),
        cache: omnish_llm::backend::CacheHint::Long,
    };
    let v = serde_json::to_value(&td).unwrap();
    let td2: ToolDef = serde_json::from_value(v).unwrap();
    assert_eq!(td2.cache, omnish_llm::backend::CacheHint::Long);
}
