use omnish_llm::backend::{LlmRequest, TriggerType};
use omnish_llm::context::ContextBuilder;

#[test]
fn test_context_builder_strips_escape_sequences() {
    let raw = b"\x1b[31mERROR\x1b[0m: file not found\n";
    let builder = ContextBuilder::new();
    let cleaned = builder.strip_escapes(raw);
    assert_eq!(cleaned, "ERROR: file not found\n");
}

#[test]
fn test_context_builder_truncates_to_max_chars() {
    let builder = ContextBuilder::new().max_chars(20);
    let long_text = "a".repeat(100);
    let truncated = builder.truncate(&long_text);
    assert_eq!(truncated.len(), 20);
}

#[test]
fn test_llm_request_build() {
    let req = LlmRequest {
        context: "$ ls\nfile.txt\n$ cat file.txt\nhello".to_string(),
        query: Some("what is in file.txt?".to_string()),
        trigger: TriggerType::Manual,
        session_ids: vec!["abc".to_string()],
    };
    assert_eq!(req.session_ids.len(), 1);
    assert!(req.query.is_some());
}
