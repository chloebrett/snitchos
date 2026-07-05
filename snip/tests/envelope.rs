use snip::{extract_result_text, extract_usage};

#[test]
fn extracts_token_usage_from_the_envelope() {
    let env = r#"{"type":"result","is_error":false,"result":"{}","usage":{"input_tokens":1200,"output_tokens":45}}"#;
    let u = extract_usage(env);
    assert_eq!(u.input_tokens, 1200);
    assert_eq!(u.output_tokens, 45);
    assert_eq!(u.total(), 1245);
}

#[test]
fn missing_usage_is_zero() {
    let env = r#"{"type":"result","is_error":false,"result":"{}"}"#;
    assert_eq!(extract_usage(env).total(), 0);
}

#[test]
fn counts_cache_tokens_as_input() {
    let env = r#"{"usage":{"input_tokens":10,"cache_read_input_tokens":900,"cache_creation_input_tokens":100,"output_tokens":5}}"#;
    let u = extract_usage(env);
    assert_eq!(u.input_tokens, 1010, "cache read + creation count as input");
    assert_eq!(u.output_tokens, 5);
}

#[test]
fn pulls_result_field_from_claude_json_envelope() {
    let envelope = r#"{
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": "{\"include\": [], \"exclude\": [], \"overall\": \"high\"}",
        "session_id": "abc"
    }"#;

    let text = extract_result_text(envelope).expect("result extracted");
    assert!(text.contains("\"include\""));
}

#[test]
fn an_error_envelope_is_reported() {
    let envelope = r#"{"type": "result", "subtype": "error_during_execution", "is_error": true, "result": "boom"}"#;
    assert!(extract_result_text(envelope).is_err());
}
