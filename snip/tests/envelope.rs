use snip::extract_result_text;

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
