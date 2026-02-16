use omnish_llm::backend::{LlmRequest, TriggerType};

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
