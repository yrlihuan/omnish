use omnish_llm::backend::{LlmRequest, TriggerType, UseCase};

#[test]
fn test_llm_request_build() {
    let req = LlmRequest {
        context: "$ ls\nfile.txt\n$ cat file.txt\nhello".to_string(),
        query: Some("what is in file.txt?".to_string()),
        trigger: TriggerType::Manual,
        session_ids: vec!["abc".to_string()],
        use_case: UseCase::Analysis,
        max_content_chars: None,
    };
    assert_eq!(req.session_ids.len(), 1);
    assert!(req.query.is_some());
}
