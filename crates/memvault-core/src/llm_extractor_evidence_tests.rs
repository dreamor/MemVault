use super::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct MockServer {
    base: String,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockServer {
    async fn new(status: u16, body: String, extra_length: usize) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request).unwrap());
                let response = format!(
                    "HTTP/1.1 {} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len() + extra_length,
                    body,
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        Self {
            base,
            requests,
            task,
        }
    }

    fn extractor(&self) -> OpenAiChatExtractor {
        let mut extractor = OpenAiChatExtractor::new(LlmExtractionConfig {
            provider: "openai-compatible".to_string(),
            api_base: self.base.clone(),
            api_key: None,
            model: "requested-model".to_string(),
        });
        // Tests are strictly loopback-only, regardless of a host's proxy env.
        extractor.client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        extractor
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

fn envelope(text: &str) -> Value {
    json!({
        "id": "reply-id",
        "object": "chat.completion",
        "model": "returned-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 0,
            "total_tokens": 12,
            "completion_tokens_details": {"reasoning_tokens": 0},
        },
        "provider_extension": {"cache_hit": true, "unknown": [1, {"x": "y"}]},
    })
}

struct LegacyExtractor {
    text: Option<&'static str>,
    error: bool,
    calls: AtomicUsize,
}

#[async_trait]
impl LlmExtractor for LegacyExtractor {
    async fn extract(&self, _context: &str) -> Result<Vec<ExtractedMemory>> {
        Ok(Vec::new())
    }

    async fn json_chat(&self, system: &str, user: &str) -> Result<Option<String>> {
        assert_eq!((system, user), ("system", "user"));
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.error {
            Err(MemVaultError::LlmExtraction(
                "Authorization: Bearer legacy-secret".to_string(),
            ))
        } else {
            Ok(self.text.map(str::to_string))
        }
    }
}

#[tokio::test]
async fn legacy_default_calls_once_and_keeps_metadata_unknown() {
    for (text, error, status) in [
        (
            Some("{\"status\":\"success\"}"),
            false,
            ChatEvidenceStatus::Response,
        ),
        (Some(""), false, ChatEvidenceStatus::Response),
        (None, false, ChatEvidenceStatus::Unsupported),
        (None, true, ChatEvidenceStatus::Error),
    ] {
        let legacy = LegacyExtractor {
            text,
            error,
            calls: AtomicUsize::new(0),
        };
        let extractor: &dyn LlmExtractor = &legacy;
        let evidence = extractor.json_chat_evidence("system", "user").await;
        assert_eq!(legacy.calls.load(Ordering::SeqCst), 1);
        assert_eq!(evidence.status, status);
        assert_eq!(evidence.text_for_validation(), text);
        assert!(!evidence.is_complete());
        assert!(evidence.requested_model.is_none());
        assert!(evidence.response_model.is_none());
        assert!(evidence.finish_reason.is_none());
        assert!(evidence.usage.is_none());
        assert!(evidence.prompt_tokens.is_none());
        assert!(evidence.completion_tokens.is_none());
        assert!(evidence.total_tokens.is_none());
        assert!(evidence.request.is_none());
        assert!(evidence.response.is_none());
        assert!(evidence.raw_response.is_none());
        assert!(evidence.http_status.is_none());
        assert!(evidence.endpoint.is_none());
        assert!(evidence.elapsed_ms.is_some());
        if error {
            assert_eq!(
                evidence.error.as_ref().unwrap().stage,
                ChatErrorStage::Legacy
            );
            assert!(
                !serde_json::to_string(&evidence)
                    .unwrap()
                    .contains("legacy-secret")
            );
        }
    }
}

#[tokio::test]
async fn unimplemented_legacy_chat_is_unsupported() {
    struct NoChat;
    #[async_trait]
    impl LlmExtractor for NoChat {
        async fn extract(&self, _context: &str) -> Result<Vec<ExtractedMemory>> {
            Ok(Vec::new())
        }
    }
    let extractor: &dyn LlmExtractor = &NoChat;
    let evidence = extractor.json_chat_evidence("system", "user").await;
    assert_eq!(evidence.status, ChatEvidenceStatus::Unsupported);
    assert!(evidence.reason.is_some());
    assert!(!evidence.is_complete());
}

#[tokio::test]
async fn not_attempted_and_empty_user_have_no_fabricated_attempt() {
    let evidence = ChatEvidence::not_attempted("disabled: Bearer private-secret");
    assert_eq!(evidence.status, ChatEvidenceStatus::NotAttempted);
    assert!(!evidence.is_complete());
    assert!(evidence.text_for_validation().is_none());
    assert!(evidence.elapsed_ms.is_none());
    assert!(
        !serde_json::to_string(&evidence)
            .unwrap()
            .contains("private-secret")
    );
    let server = MockServer::new(200, envelope("{}").to_string(), 0).await;
    let extractor = server.extractor();
    let evidence = extractor.json_chat_evidence("system", " \n ").await;
    assert_eq!(evidence.status, ChatEvidenceStatus::NotAttempted);
    assert!(evidence.request.is_none());
    assert!(evidence.http_status.is_none());
    assert!(extractor.json_chat("system", " ").await.unwrap().is_none());
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn native_evidence_preserves_envelope_usage_request_and_one_attempt() {
    let text = "{\"status\":\"foo\"}";
    let response = envelope(text);
    let raw = format!(" \n{}\n", response);
    let server = MockServer::new(200, raw.clone(), 0).await;
    let extractor: Arc<dyn LlmExtractor> = Arc::new(server.extractor());
    let evidence = extractor.json_chat_evidence("system", "user").await;
    assert_eq!(evidence.status, ChatEvidenceStatus::Response);
    assert!(evidence.is_complete());
    // Transport completeness does not silently turn an unknown task verdict
    // into success. Schema validation belongs to the caller, on original text.
    assert!(parse_judged_outcome(text).unwrap().is_none());
    assert_eq!(evidence.text_for_validation(), Some(text));
    assert_eq!(evidence.text.as_deref(), Some(text));
    assert_eq!(evidence.requested_model.as_deref(), Some("requested-model"));
    assert_eq!(evidence.response_model.as_deref(), Some("returned-model"));
    assert_eq!(evidence.finish_reason.as_deref(), Some("stop"));
    assert_eq!(evidence.usage.as_ref(), response.get("usage"));
    assert_eq!(evidence.prompt_tokens, Some(12));
    assert_eq!(evidence.completion_tokens, Some(0));
    assert_eq!(evidence.total_tokens, Some(12));
    assert_eq!(evidence.response, Some(response));
    assert_eq!(evidence.raw_response.as_deref(), Some(raw.as_str()));
    assert_eq!(evidence.http_status, Some(200));
    assert!(evidence.error.is_none());
    assert!(evidence.elapsed_ms.is_some());
    assert_eq!(
        evidence.endpoint,
        Some(format!("{}/chat/completions", server.base))
    );
    assert_eq!(server.request_count(), 1);
    let requests = server.requests.lock().unwrap();
    let request = &requests[0];
    assert!(request.starts_with("POST /chat/completions HTTP/1.1\r\n"));
    let (_, body) = request.split_once("\r\n\r\n").unwrap();
    let actual: Value = serde_json::from_str(body).unwrap();
    assert_eq!(evidence.request.as_ref(), Some(&actual));
    assert_eq!(
        actual,
        json!({
            "model": "requested-model",
            "messages": [{"role": "system", "content": "system"}, {"role": "user", "content": "user"}],
            "temperature": 0.0,
            "response_format": {"type": "json_object"},
        })
    );
}

#[tokio::test]
async fn non_stop_or_empty_text_is_incomplete_but_old_chat_stays_compatible() {
    for (finish, text) in [
        (Some("length"), "{\"status\":\"success\"}"),
        (Some("content_filter"), "{}"),
        (Some("foo"), "{}"),
        (Some("stop "), "{}"),
        (Some("STOP"), "{}"),
        (None, "{}"),
        (Some("stop"), ""),
        (Some("stop"), " \n "),
    ] {
        let mut response = envelope(text);
        match finish {
            Some(finish) => response["choices"][0]["finish_reason"] = json!(finish),
            None => {
                response["choices"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("finish_reason");
            }
        }
        let server = MockServer::new(200, response.to_string(), 0).await;
        let extractor = server.extractor();
        let evidence = extractor.json_chat_evidence("system", "user").await;
        assert_eq!(evidence.status, ChatEvidenceStatus::Response);
        assert!(evidence.error.is_none());
        assert!(!evidence.is_complete(), "finish={finish:?}, text={text:?}");
        assert_eq!(evidence.text_for_validation(), Some(text));
        assert_eq!(
            extractor
                .json_chat("system", "user")
                .await
                .unwrap()
                .as_deref(),
            Some(text)
        );
        assert_eq!(server.request_count(), 2);
    }
}

#[tokio::test]
async fn native_text_only_response_has_no_inferred_model_finish_or_usage() {
    let text = "```json\n{\"lesson\":\"check credentials\"}\n```";
    let response = json!({"choices": [{"message": {"content": text}}]});
    let server = MockServer::new(200, response.to_string(), 0).await;
    let extractor = server.extractor();
    let evidence = extractor.json_chat_evidence("system", "user").await;
    assert_eq!(evidence.status, ChatEvidenceStatus::Response);
    assert!(!evidence.is_complete());
    assert!(evidence.response_model.is_none());
    assert!(evidence.finish_reason.is_none());
    assert!(evidence.usage.is_none());
    assert!(evidence.prompt_tokens.is_none());
    assert!(evidence.completion_tokens.is_none());
    assert!(evidence.total_tokens.is_none());
    assert_eq!(
        extractor
            .json_chat("system", "user")
            .await
            .unwrap()
            .as_deref(),
        Some(text)
    );
    assert_eq!(
        extractor.reflect_lesson("user").await.unwrap().as_deref(),
        Some("check credentials")
    );
    assert_eq!(server.request_count(), 3);
}

#[tokio::test]
async fn malformed_metadata_types_stay_unknown_and_original_envelope_is_kept() {
    for field in [
        "model",
        "finish_reason",
        "usage",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
    ] {
        for invalid in [
            Value::Null,
            json!(true),
            json!("12"),
            json!(-1),
            json!(1.0),
            json!(1.5),
            json!([]),
            json!({}),
        ] {
            if field == "model" || field == "finish_reason" {
                if invalid.is_string() {
                    continue;
                }
            } else if field == "usage" && invalid.is_object() {
                continue;
            }
            let mut response = envelope("{}");
            match field {
                "model" | "usage" => response[field] = invalid.clone(),
                "finish_reason" => response["choices"][0][field] = invalid.clone(),
                _ => response["usage"][field] = invalid.clone(),
            }
            let server = MockServer::new(200, response.to_string(), 0).await;
            let evidence = server
                .extractor()
                .json_chat_evidence("system", "user")
                .await;
            assert_eq!(
                evidence.status,
                ChatEvidenceStatus::Response,
                "{field}={invalid}"
            );
            assert_eq!(evidence.response.as_ref(), Some(&response));
            match field {
                "model" => assert!(evidence.response_model.is_none()),
                "finish_reason" => {
                    assert!(evidence.finish_reason.is_none());
                    assert!(!evidence.is_complete());
                }
                "usage" => {
                    assert!(evidence.usage.is_none());
                    assert!(evidence.prompt_tokens.is_none());
                    assert!(evidence.completion_tokens.is_none());
                    assert!(evidence.total_tokens.is_none());
                }
                "prompt_tokens" => assert!(evidence.prompt_tokens.is_none()),
                "completion_tokens" => assert!(evidence.completion_tokens.is_none()),
                "total_tokens" => assert!(evidence.total_tokens.is_none()),
                _ => unreachable!(),
            }
            assert_eq!(server.request_count(), 1);
        }
    }
    let raw = envelope("{}").to_string().replace(
        "\"prompt_tokens\":12",
        "\"prompt_tokens\":18446744073709551616",
    );
    let server = MockServer::new(200, raw, 0).await;
    let evidence = server
        .extractor()
        .json_chat_evidence("system", "user")
        .await;
    assert!(evidence.prompt_tokens.is_none());
    assert!(evidence.response.is_some());
}

#[tokio::test]
async fn malformed_envelopes_preserve_raw_and_json_without_becoming_complete() {
    for response in [
        Value::Null,
        json!(true),
        json!(1),
        json!("text"),
        json!([]),
        json!({}),
        json!({"choices": null}),
        json!({"choices": {}}),
        json!({"choices": "no"}),
        json!({"choices": []}),
        json!({"choices": [null]}),
        json!({"choices": [1]}),
        json!({"choices": [{}]}),
        json!({"choices": [{"message": null}]}),
        json!({"choices": [{"message": []}]}),
        json!({"choices": [{"message": "no"}]}),
        json!({"choices": [{"message": {}}]}),
        json!({"choices": [{"message": {"content": null}, "finish_reason": "stop"}]}),
        json!({"choices": [{"message": {"content": 1}, "finish_reason": "stop"}]}),
        json!({"choices": [{"message": {"content": []}, "finish_reason": "stop"}]}),
        json!({"choices": [{"message": {"content": {}}, "finish_reason": "stop"}]}),
        json!({"choices": [{"message": {"content": true}, "finish_reason": "stop"}]}),
        json!({"choices": [{"message": {"content": "{}"}, "finish_reason": "stop"}, null]}),
    ] {
        let raw = response.to_string();
        let server = MockServer::new(200, raw.clone(), 0).await;
        let extractor = server.extractor();
        let evidence = extractor.json_chat_evidence("system", "user").await;
        assert_eq!(evidence.status, ChatEvidenceStatus::Error, "{response}");
        assert_eq!(
            evidence.error.as_ref().unwrap().stage,
            ChatErrorStage::ResponseShape
        );
        assert!(!evidence.is_complete());
        assert_eq!(evidence.response, Some(response));
        assert_eq!(evidence.raw_response, Some(raw));
        assert_eq!(evidence.http_status, Some(200));
        assert!(extractor.json_chat("system", "user").await.is_err());
        assert_eq!(server.request_count(), 2);
    }
}

#[tokio::test]
async fn invalid_json_keeps_raw_body_and_parse_error() {
    for raw in ["not JSON", "{\"choices\":", "\u{feff}{}"] {
        let server = MockServer::new(200, raw.to_string(), 0).await;
        let extractor = server.extractor();
        let evidence = extractor.json_chat_evidence("system", "user").await;
        assert_eq!(
            evidence.error.as_ref().unwrap().stage,
            ChatErrorStage::ResponseParse
        );
        assert!(!evidence.is_complete());
        assert_eq!(evidence.raw_response.as_deref(), Some(raw));
        assert!(evidence.response.is_none());
        assert_eq!(evidence.http_status, Some(200));
        assert!(
            extractor
                .json_chat("system", "user")
                .await
                .unwrap_err()
                .to_string()
                .contains("response parse error:")
        );
        assert_eq!(server.request_count(), 2);
    }
}

#[tokio::test]
async fn http_errors_preserve_status_body_and_even_success_looking_metadata() {
    for (status, body) in [
        (401, envelope("{}").to_string()),
        (500, "upstream failed".to_string()),
    ] {
        let server = MockServer::new(status, body.clone(), 0).await;
        let extractor = server.extractor();
        let evidence = extractor.json_chat_evidence("system", "user").await;
        assert_eq!(evidence.status, ChatEvidenceStatus::Error);
        assert_eq!(evidence.error.as_ref().unwrap().stage, ChatErrorStage::Http);
        assert_eq!(evidence.http_status, Some(status));
        assert_eq!(evidence.raw_response, Some(body));
        assert!(!evidence.is_complete());
        if status == 401 {
            assert_eq!(evidence.response_model.as_deref(), Some("returned-model"));
            assert_eq!(evidence.finish_reason.as_deref(), Some("stop"));
            assert!(evidence.response.is_some());
        } else {
            assert!(evidence.response.is_none());
        }
        assert!(
            extractor
                .json_chat("system", "user")
                .await
                .unwrap_err()
                .to_string()
                .contains(&format!("API {status}"))
        );
        assert_eq!(server.request_count(), 2);
    }
}

#[tokio::test]
async fn body_read_failure_has_status_and_no_retry_or_fabricated_body() {
    for status in [200, 503] {
        let server = MockServer::new(status, "partial body".to_string(), 100).await;
        let extractor = server.extractor();
        let evidence = extractor.json_chat_evidence("system", "user").await;
        assert_eq!(evidence.status, ChatEvidenceStatus::Error);
        assert_eq!(
            evidence.error.as_ref().unwrap().stage,
            ChatErrorStage::BodyRead
        );
        assert_eq!(evidence.http_status, Some(status));
        assert!(evidence.raw_response.is_none());
        assert!(evidence.response.is_none());
        assert!(evidence.finish_reason.is_none());
        assert!(evidence.elapsed_ms.is_some());
        assert!(!evidence.is_complete());
        assert_eq!(server.request_count(), 1);
        let error = extractor
            .json_chat("system", "user")
            .await
            .unwrap_err()
            .to_string();
        if status == 200 {
            assert!(error.contains("response parse error:"));
        } else {
            assert!(error.contains("API 503 Service Unavailable : "));
        }
        assert_eq!(server.request_count(), 2);
    }
}

#[tokio::test]
async fn request_build_error_records_attempt_without_network_or_secret() {
    let key = "sk-request-error-secret";
    let extractor = OpenAiChatExtractor::new(LlmExtractionConfig {
        provider: "openai".to_string(),
        api_base: format!("http://[invalid/{key}"),
        api_key: Some(key.to_string()),
        model: "requested-model".to_string(),
    });
    let evidence = extractor.json_chat_evidence("system", "user").await;
    assert_eq!(evidence.status, ChatEvidenceStatus::Error);
    assert_eq!(
        evidence.error.as_ref().unwrap().stage,
        ChatErrorStage::Request
    );
    assert!(evidence.http_status.is_none());
    assert!(evidence.response.is_none());
    assert!(evidence.raw_response.is_none());
    assert!(evidence.request.is_some());
    assert!(evidence.elapsed_ms.is_some());
    assert!(!evidence.is_complete());
    assert!(!serde_json::to_string(&evidence).unwrap().contains(key));
    let error = extractor
        .json_chat("system", "user")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("request error:"));
    assert!(!error.contains(key));
}

#[tokio::test]
async fn serialized_and_debug_evidence_redact_credentials_not_requests_or_validation() {
    let key = "sk-loopback-quote\"secret\\piece";
    let username = "loop-user-902";
    let password = "loop-pass-902";
    let text =
        json!({"status": "foo", "echo": key, "user": username, "pass": password}).to_string();
    let mut response = envelope(&text);
    response["model"] = json!(format!("model-{key}"));
    response["provider_extension"]["api_key"] = json!("unknown provider key with spaces");
    response["provider_extension"]["Authorization"] = json!("Bearer other-private-token");
    response["provider_extension"]["userinfo"] = json!(format!("{username}:{password}"));
    response["provider_extension"]["password"] = json!("unknown password with spaces");
    let server = MockServer::new(200, response.to_string(), 0).await;
    let mut extractor = server.extractor();
    extractor.config.api_key = Some(key.to_string());
    extractor.config.api_base =
        server
            .base
            .replacen("http://", &format!("http://{username}:{password}@"), 1);
    let prompt = format!("unlabelled credential {key} for {username} and {password}");
    let evidence = extractor.json_chat_evidence(&prompt, &prompt).await;
    assert!(evidence.is_complete());
    assert_eq!(evidence.text_for_validation(), Some(text.as_str()));
    let original: Value = serde_json::from_str(evidence.text_for_validation().unwrap()).unwrap();
    assert_eq!(original["echo"], key);
    for record in [
        serde_json::to_string(&evidence).unwrap(),
        format!("{evidence:?}"),
    ] {
        for secret in [
            key,
            username,
            password,
            "other-private-token",
            "unknown provider key with spaces",
            "unknown password with spaces",
        ] {
            assert!(
                !record.contains(secret),
                "credential redaction check failed"
            );
        }
        let escaped = serde_json::to_string(key).unwrap();
        assert!(!record.contains(&escaped[1..escaped.len() - 1]));
        assert!(!record.contains("validation_text"));
        assert!(!record.contains("legacy_error"));
    }
    assert!(
        evidence
            .endpoint
            .as_ref()
            .unwrap()
            .contains("[REDACTED]@127.0.0.1")
    );
    {
        let requests = server.requests.lock().unwrap();
        let request = &requests[0];
        assert!(request.contains(&format!("Bearer {key}")));
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        let actual: Value = serde_json::from_str(body).unwrap();
        assert_eq!(actual["messages"][0]["content"], prompt);
        assert_eq!(actual["messages"][1]["content"], prompt);
    }
    // The old API still returns original, not record-redacted, model text.
    assert_eq!(
        extractor
            .json_chat(&prompt, &prompt)
            .await
            .unwrap()
            .as_deref(),
        Some(text.as_str())
    );
    assert_eq!(server.request_count(), 2);
    let restored: ChatEvidence =
        serde_json::from_str(&serde_json::to_string(&evidence).unwrap()).unwrap();
    assert!(restored.validation_text.is_none());
    assert!(restored.legacy_error.is_none());
    assert_ne!(
        restored.text_for_validation(),
        evidence.text_for_validation()
    );
}

#[tokio::test]
async fn derived_record_copies_are_safe_without_changing_original_completeness() {
    // Even a configured key colliding with a finish literal must not change
    // operational completeness when that literal is redacted in the record.
    let text = "{\"answer\":\"stop\"}";
    let server = MockServer::new(200, envelope(text).to_string(), 0).await;
    let mut extractor = server.extractor();
    extractor.config.api_key = Some("stop".to_string());
    let evidence = extractor.json_chat_evidence("system", "user").await;
    assert!(evidence.is_complete());
    assert_eq!(evidence.text_for_validation(), Some(text));
    assert_eq!(evidence.finish_reason.as_deref(), Some("[REDACTED]"));
    let parsed: Value = serde_json::from_str(evidence.text_for_validation().unwrap()).unwrap();
    let mut record_copy = parsed.clone();
    evidence.redact_record_value(&mut record_copy);
    assert_eq!(parsed["answer"], "stop");
    assert_eq!(record_copy["answer"], "[REDACTED]");
    let mut reason = json!("parse reason contains stop and Authorization: Bearer unknown-secret");
    evidence.redact_record_value(&mut reason);
    assert!(!reason.as_str().unwrap().contains("stop"));
    assert!(!reason.as_str().unwrap().contains("unknown-secret"));
    assert!(
        !serde_json::to_string(&evidence)
            .unwrap()
            .contains("record_secrets")
    );
    assert_eq!(server.request_count(), 1);
}

#[test]
fn nested_json_escaping_of_known_credentials_is_redacted_at_every_depth() {
    let key = "sk-deep-quote\"secret\\piece";
    let mut nested = key.to_string();
    for _ in 0..8 {
        nested = serde_json::to_string(&nested).unwrap();
    }
    let mut evidence = ChatEvidence::not_attempted("reason");
    evidence.raw_response = Some(nested);
    evidence.redact(&[key.to_string()]);
    assert!(!evidence.raw_response.as_ref().unwrap().contains("secret"));
}

#[tokio::test]
async fn raw_error_body_and_error_message_redact_unlabelled_known_credentials() {
    let key = "sk-http-error-quote\"secret\\piece";
    for status in [200, 401] {
        let raw = format!(
            "upstream echo {key}; {}",
            serde_json::to_string(key).unwrap()
        );
        let server = MockServer::new(status, raw, 0).await;
        let mut extractor = server.extractor();
        extractor.config.api_key = Some(key.to_string());
        let evidence = extractor.json_chat_evidence("system", "user").await;
        assert!(!evidence.is_complete());
        let serialized = serde_json::to_string(&evidence).unwrap();
        assert!(!serialized.contains(key));
        assert!(!serialized.contains("secret"));
        assert!(!evidence.raw_response.as_ref().unwrap().contains(key));
        assert!(!evidence.error.as_ref().unwrap().message.contains(key));
        assert!(
            !extractor
                .json_chat("system", "user")
                .await
                .unwrap_err()
                .to_string()
                .contains(key)
        );
    }
}

#[test]
fn record_redaction_handles_nested_fields_auth_userinfo_and_encoded_credentials() {
    let mut evidence = ChatEvidence::not_attempted("reason");
    evidence.response = Some(json!({
        "nested": [{"API_KEY": "opaque value with spaces", "Proxy-Authorization": "Basic dXNlcjpwYXNz", "auth": "unknown-auth-value"}],
        "url": "https://encoded-user:encoded-pass@example.test/v1",
        "token": 123,
    }));
    evidence.raw_response = Some(
        "{\"API KEY\":\"opaque value with spaces\",\"Authorization\":\"Bearer auth-token\",\"auth\":\"unknown-auth-value\"}"
            .to_string(),
    );
    evidence.redact(&[]);
    let record = serde_json::to_string(&evidence).unwrap();
    for secret in [
        "opaque value with spaces",
        "dXNlcjpwYXNz",
        "encoded-user",
        "encoded-pass",
        "auth-token",
        "unknown-auth-value",
    ] {
        assert!(
            !record.contains(secret),
            "credential redaction check failed"
        );
    }
    let extractor = OpenAiChatExtractor::new(LlmExtractionConfig {
        provider: "openai".to_string(),
        api_base: "https://user%40mail:pass%2Bword@example.test/v1?api_key=query-secret"
            .to_string(),
        api_key: None,
        model: "model".to_string(),
    });
    let mut evidence = ChatEvidence::not_attempted("reason");
    evidence.raw_response =
        Some("user@mail pass+word user%40mail pass%2Bword query-secret".to_string());
    evidence.endpoint = Some(extractor.config.api_base.clone());
    let evidence = extractor.record_chat_evidence(evidence, Instant::now());
    let record = serde_json::to_string(&evidence).unwrap();
    for secret in [
        "user@mail",
        "pass+word",
        "user%40mail",
        "pass%2Bword",
        "query-secret",
    ] {
        assert!(!record.contains(secret));
    }
}

fn record_echoes(
    extractor: &OpenAiChatExtractor,
    echo: &str,
    raw: &str,
    error: bool,
) -> ChatEvidence {
    let text = json!({"echo": echo}).to_string();
    let mut evidence = ChatEvidence::not_attempted(format!("reason echoes {echo}"));
    evidence.status = ChatEvidenceStatus::Response;
    evidence.endpoint = Some(format!("{}/chat/completions", extractor.config.api_base));
    evidence.requested_model = Some(format!("requested {echo}"));
    evidence.response_model = Some(format!("returned {echo}"));
    evidence.finish_reason = Some("stop".to_string());
    evidence.text = Some(text.clone());
    evidence.validation_text = Some(text);
    evidence.request = Some(json!({"messages": [{"content": echo}], "extension": {echo: echo}}));
    evidence.response = Some(serde_json::from_str(raw).unwrap());
    evidence.usage = Some(json!({"provider_extension": echo}));
    evidence.raw_response = Some(raw.to_string());
    if error {
        evidence.fail(
            ChatErrorStage::Http,
            format!("API error echoes {echo}; raw={raw}"),
            Some(format!("API error echoes {echo}; raw={raw}")),
        );
    }
    extractor.record_chat_evidence(evidence, Instant::now())
}

fn assert_record_hides(evidence: &ChatEvidence, secrets: &[&str]) {
    let serialized = serde_json::to_string(evidence).unwrap();
    let debug = format!("{evidence:?}");
    let mut derived = json!({"reason": evidence.text_for_validation().unwrap()});
    evidence.redact_record_value(&mut derived);
    for text in [
        serialized.as_str(),
        debug.as_str(),
        evidence.raw_response.as_deref().unwrap(),
        evidence.legacy_error.as_deref().unwrap_or(""),
    ] {
        for secret in secrets {
            assert!(!text.contains(secret), "credential redaction check failed");
        }
    }
    for secret in secrets {
        assert!(!derived.to_string().contains(secret));
    }
}

#[test]
fn configured_query_credentials_redact_raw_decoded_and_reencoded_echoes() {
    let aliases = ["abc/def", "abc%2Fdef", "abc%2fdef", "%61%62%63%2Fdef"];
    let echo = aliases.join(" | ");
    for name in ["key", "KEY", "api%5Fkey", "%6B%65%79", "client%5Fsecret"] {
        for raw_value in ["abc%2Fdef", "%61bc%2fdef"] {
            let extractor = OpenAiChatExtractor::new(LlmExtractionConfig {
                provider: "openai".to_string(),
                api_base: format!("https://example.test/v1?public=ok&&{name}={raw_value}&tail=yes"),
                api_key: None,
                model: "model".to_string(),
            });
            let raw = json!({"echo": echo, "extension": {"unrelated": true}}).to_string();
            for error in [false, true] {
                let evidence = record_echoes(&extractor, &echo, &raw, error);
                assert_record_hides(&evidence, &aliases);
                assert!(!evidence.endpoint.as_ref().unwrap().contains(raw_value));
                assert!(evidence.endpoint.as_ref().unwrap().contains("public=ok"));
                assert!(evidence.endpoint.as_ref().unwrap().contains("tail=yes"));
                assert_eq!(
                    evidence.text_for_validation(),
                    Some(json!({"echo": echo}).to_string().as_str())
                );
                assert_eq!(evidence.is_complete(), !error);
                assert_eq!(
                    evidence.response.as_ref().unwrap()["extension"]["unrelated"],
                    true
                );
            }
        }
    }
}

#[test]
fn invalid_api_bases_still_hide_configured_query_credentials() {
    let aliases = ["abc/def", "abc%2Fdef", "abc%2fdef"];
    let echo = aliases.join(" | ");
    for base in [
        "https://[invalid?key=abc%2Fdef&public=yes",
        "not a URL?api%5Fkey=abc%2Fdef&public=yes",
    ] {
        assert!(reqwest::Url::parse(base).is_err());
        let extractor = OpenAiChatExtractor::new(LlmExtractionConfig {
            provider: "openai".to_string(),
            api_base: base.to_string(),
            api_key: None,
            model: "model".to_string(),
        });
        let raw = json!({"echo": echo}).to_string();
        let mut evidence = record_echoes(&extractor, &echo, &raw, true);
        evidence.error.as_mut().unwrap().stage = ChatErrorStage::Request;
        assert_record_hides(&evidence, &aliases);
        assert!(evidence.endpoint.as_ref().unwrap().contains("public=yes"));
        assert_eq!(
            evidence.text_for_validation(),
            Some(json!({"echo": echo}).to_string().as_str())
        );
        assert!(!evidence.is_complete());
    }
}

#[test]
fn userinfo_uri_decoding_preserves_plus_and_delimiters_and_redacts_encoded_aliases() {
    let aliases = [
        "user+tag&tail/more",
        "user+tag%26tail%2fmore",
        "user%2Btag%26tail%2Fmore",
        "pass+word&tail=part",
        "pass+word%26tail%3dpart",
        "pass%2bword%26tail%3Dpart",
    ];
    let echo = aliases.join(" | ");
    let extractor = OpenAiChatExtractor::new(LlmExtractionConfig {
        provider: "openai".to_string(),
        api_base: "https://user+tag%26tail%2fmore:pass+word%26tail%3dpart@example.test/v1"
            .to_string(),
        api_key: None,
        model: "model".to_string(),
    });
    let raw = json!({"echo": echo}).to_string();
    for error in [false, true] {
        let evidence = record_echoes(&extractor, &echo, &raw, error);
        assert_record_hides(&evidence, &aliases);
        assert!(
            evidence
                .endpoint
                .as_ref()
                .unwrap()
                .contains("[REDACTED]@example.test")
        );
        assert_eq!(evidence.is_complete(), !error);
        assert_eq!(
            evidence.text_for_validation(),
            Some(json!({"echo": echo}).to_string().as_str())
        );
    }
    assert_eq!(decode_chat_percent_credential("a+b%26c%3Dd"), "a+b&c=d");
    assert_eq!(decode_chat_percent_credential("bad%zz%2"), "bad%zz%2");
}

#[test]
fn labelled_secret_aliases_redact_raw_parsed_and_nested_container_echoes() {
    let aliases = [
        "refresh_token",
        "refresh-token",
        "refreshToken",
        "REFRESH_TOKEN",
        "credentials",
        "Credentials",
        "secret_key",
        "secret-key",
        "secretKey",
        "client_secret",
        "client-secret",
        "clientSecret",
    ];
    for label in aliases {
        for value in [
            json!("opaque-secret-value with spaces"),
            json!({"arbitrary_name": "opaque-secret-value"}),
            json!(["opaque-secret-value", {"arbitrary_name": "opaque-secret-value"}]),
            json!(12345),
        ] {
            let original = json!({label: value, "preserved": {"x": 1}});
            let raw = format!(" \n{}\n", original);
            let mut parsed = original.clone();
            redact_chat_json(&mut parsed, &[]);
            assert_eq!(parsed[label], "[REDACTED]");
            assert_eq!(parsed["preserved"], original["preserved"]);
            let safe = redact_chat_text(&raw, &[]);
            let safe_value: Value = serde_json::from_str(&safe).unwrap();
            assert_eq!(safe_value[label], "[REDACTED]");
            assert_eq!(safe_value["preserved"], original["preserved"]);
            assert!(safe.starts_with(" \n"));
            assert!(safe.ends_with('\n'));
            assert!(!safe.contains("opaque-secret-value"));
            let nested = json!({"content": raw}).to_string();
            let safe_nested = redact_chat_text(&nested, &[]);
            let nested_value: Value = serde_json::from_str(&safe_nested).unwrap();
            let inner: Value =
                serde_json::from_str(nested_value["content"].as_str().unwrap()).unwrap();
            assert_eq!(inner[label], "[REDACTED]");
            assert!(!safe_nested.contains("opaque-secret-value"));
        }
        for literal in [
            format!("{label}: opaque-secret-value"),
            format!("prefix {label}: \"opaque-secret-value with spaces\" suffix"),
            format!("prefix {label}='opaque-secret-value with spaces' suffix"),
        ] {
            assert!(!redact_chat_text(&literal, &[]).contains("opaque-secret-value"));
            assert!(!redact_chat_text(&literal, &[]).contains("with spaces"));
        }
    }
}

#[test]
fn alternate_json_unicode_and_slash_escapes_cannot_hide_known_credentials() {
    let extractor = OpenAiChatExtractor::new(LlmExtractionConfig {
        provider: "openai".to_string(),
        api_base: "https://example.test/v1?key=abc%2Fdef".to_string(),
        api_key: Some("sk-test".to_string()),
        model: "model".to_string(),
    });
    let unicode_t = concat!("\\", "u0074");
    let unicode_percent = concat!("\\", "u0025");
    let unicode_a = concat!("\\", "u0061");
    let escaped_slash = concat!("\\", "/");
    let raw = format!(
        " \n{{ \"echo\" : \"sk-{unicode_t}est\", \"sk-{unicode_t}est\" : \"abc{unicode_percent}2fdef\", \"slash\" : \"abc{escaped_slash}def\", \"unicode\" : \"{unicode_a}bc/def\", \"preserved\" : 1 }}\n"
    );
    let evidence = record_echoes(&extractor, "sk-test abc%2Fdef abc/def", &raw, true);
    assert_record_hides(&evidence, &["sk-test", "abc/def", "abc%2Fdef", "abc%2fdef"]);
    let safe_raw = evidence.raw_response.as_ref().unwrap();
    let parsed: Value = serde_json::from_str(safe_raw).unwrap();
    assert_eq!(parsed["echo"], "[REDACTED]");
    assert_eq!(parsed["[REDACTED]"], "[REDACTED]");
    assert_eq!(parsed["slash"], "[REDACTED]");
    assert_eq!(parsed["unicode"], "[REDACTED]");
    assert_eq!(parsed["preserved"], 1);
    assert!(safe_raw.starts_with(" \n{ "));
    assert!(safe_raw.ends_with(" }\n"));
    assert!(safe_raw.contains("\"preserved\" : 1"));
    assert!(!safe_raw.contains(unicode_t));
    assert!(!safe_raw.contains(unicode_percent));
    let nested = json!({"content": raw}).to_string();
    let safe = redact_chat_text(&nested, &evidence.record_secrets);
    let nested_value: Value = serde_json::from_str(&safe).unwrap();
    let inner: Value = serde_json::from_str(nested_value["content"].as_str().unwrap()).unwrap();
    assert_eq!(inner["echo"], "[REDACTED]");
}

#[test]
fn percent_secret_matching_preserves_unrelated_unicode_and_invalid_escapes() {
    let text = "💬 bad%zz%2 %e5%af%86%E9%92%A5 %61bc%2fdef trailing";
    let safe = redact_chat_text(
        text,
        &["abc".to_string(), "abc/def".to_string(), "密钥".to_string()],
    );
    assert_eq!(safe, "💬 bad%zz%2 [REDACTED] [REDACTED] trailing");
    let malformed = r#"prefix {"credentials":{"arbitrary_name":"opaque-secret-value""#;
    let safe = redact_chat_text(malformed, &[]);
    assert!(!safe.contains("opaque-secret-value"));
    assert!(safe.starts_with("prefix {\"credentials\":"));
}

#[test]
fn malformed_json_strings_fail_closed_without_changing_operational_text() {
    let extractor = OpenAiChatExtractor::new(LlmExtractionConfig {
        provider: "openai".to_string(),
        api_base: "https://example.test/v1".to_string(),
        api_key: Some("sk-test".to_string()),
        model: "model".to_string(),
    });
    let unicode_t = concat!("\\", "u0074");
    let complete = format!(r#"{{"echo":"sk-{unicode_t}est"}}"#);
    let invalid_escape = concat!("\\", "q");
    for raw in [
        complete[..complete.len() - 2].to_string(),
        format!(r#"{{"echo":"sk-{unicode_t}est{invalid_escape}"}}"#),
        format!("{{\"echo\":\"sk-{unicode_t}est\\\n\"}}"),
    ] {
        // Malformed outer envelopes retain no fabricated model text, but
        // their raw/error/request records must still be credential safe.
        let mut source = ChatEvidence::not_attempted("response parse error");
        source.raw_response = Some(raw.clone());
        source.request = Some(json!({"messages": [{"content": raw}]}));
        let error = format!("response parse error: {raw}");
        source.fail(ChatErrorStage::ResponseParse, error.clone(), Some(error));
        let evidence = extractor.record_chat_evidence(source, Instant::now());
        assert!(evidence.text_for_validation().is_none());
        assert!(!evidence.is_complete());
        assert!(evidence.response.is_none());
        for text in [
            serde_json::to_string(&evidence).unwrap(),
            format!("{evidence:?}"),
            evidence.raw_response.clone().unwrap(),
            evidence.error.as_ref().unwrap().message.clone(),
            evidence.legacy_error.clone().unwrap(),
        ] {
            assert!(!text.contains("sk-test"));
            assert!(
                !text.contains("u0074"),
                "encoded credential survived: {text}"
            );
        }
        // A valid envelope can carry the same malformed model text. Record
        // redaction must not change original text/finish or transport gating.
        let mut source = ChatEvidence::not_attempted("model text");
        source.status = ChatEvidenceStatus::Response;
        source.reason = None;
        source.finish_reason = Some("stop".to_string());
        source.text = Some(raw.clone());
        source.validation_text = Some(raw.clone());
        source.response = Some(envelope(&raw));
        source.raw_response = Some(envelope(&raw).to_string());
        let evidence = extractor.record_chat_evidence(source, Instant::now());
        assert_eq!(evidence.text_for_validation(), Some(raw.as_str()));
        assert!(evidence.is_complete());
        assert!(!serde_json::to_string(&evidence).unwrap().contains("u0074"));
    }
}
