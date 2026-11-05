use std::time::Duration;

use serde_json::json;

use super::*;
use crate::config::Ini;
use crate::testutil::{MockResponse, MockServer};

fn section(text: &str) -> Section {
    Ini::parse(&format!("[tiku]\n{text}"))
        .section("tiku")
        .cloned()
        .unwrap()
}

fn fast(mut cfg: LlmConfig) -> LlmConfig {
    cfg.timeout = Duration::from_secs(5);
    cfg.min_interval = 0.0;
    cfg
}

#[test]
fn endpoints_are_resolved_per_protocol() {
    let cases = [
        (
            ApiKind::OpenAi,
            "https://api.x.com/v1",
            "https://api.x.com/v1/chat/completions",
        ),
        (
            ApiKind::OpenAi,
            "https://api.x.com/v1/",
            "https://api.x.com/v1/chat/completions",
        ),
        (
            ApiKind::OpenAi,
            "https://api.siliconflow.cn/v1/chat/completions",
            "https://api.siliconflow.cn/v1/chat/completions",
        ),
        (
            ApiKind::Anthropic,
            "https://api.anthropic.com",
            "https://api.anthropic.com/v1/messages",
        ),
        (
            ApiKind::Anthropic,
            "https://relay.example/v1",
            "https://relay.example/v1/messages",
        ),
        (
            ApiKind::Anthropic,
            "https://relay.example/v1/messages",
            "https://relay.example/v1/messages",
        ),
    ];
    for (api, base, expected) in cases {
        assert_eq!(resolve_endpoint(api, base), expected);
    }
}

#[test]
fn presets_fill_endpoint_and_protocol() {
    let cfg = LlmConfig::from_section(&section(
        "llm_preset=claude\nkey=sk-ant\nmodel=claude-sonnet-5-5",
    ))
    .unwrap();
    assert_eq!(cfg.api, ApiKind::Anthropic);
    assert_eq!(cfg.endpoint, "https://api.anthropic.com/v1/messages");

    let cfg = LlmConfig::from_section(&section(
        "llm_preset=Kimi\nendpoint=https://proxy.example/v1\nkey=k\nmodel=kimi-k2\nmin_interval_seconds=0.5\nllm_vision=true",
    ))
    .unwrap();
    assert_eq!(cfg.api, ApiKind::OpenAi);
    assert_eq!(cfg.endpoint, "https://proxy.example/v1/chat/completions");
    assert_eq!(cfg.min_interval, 0.5);
    assert!(cfg.vision);

    assert!(find_preset("deepseek").is_some());
    assert!(LlmConfig::from_section(&section("llm_preset=unknown\nmodel=x")).is_err());
    assert!(
        LlmConfig::from_section(&section("endpoint=https://x/v1\nkey=k")).is_err(),
        "model is required"
    );
    assert!(
        LlmConfig::from_section(&section("model=x\nkey=k")).is_err(),
        "endpoint is required"
    );
    assert!(
        LlmConfig::from_section(&section("endpoint=https://x\nmodel=m\nllm_extra_body=[1]"))
            .is_err()
    );
}

#[test]
fn siliconflow_keeps_original_defaults() {
    let cfg =
        LlmConfig::siliconflow_from_section(&section("siliconflow_key=sk\nllm_temperature=0.2"))
            .unwrap();
    assert_eq!(
        cfg.endpoint,
        "https://api.siliconflow.cn/v1/chat/completions"
    );
    assert_eq!(cfg.model, "deepseek-ai/DeepSeek-V3");
    assert_eq!(cfg.temperature, Some(0.2));
    assert_eq!(cfg.top_p, Some(0.7));
    assert_eq!(cfg.max_tokens, Some(4096));
    assert!(LlmConfig::siliconflow_from_section(&section("siliconflow_model=x")).is_err());
}

fn sample_request() -> ChatRequest {
    ChatRequest {
        system: "系统提示".into(),
        messages: vec![Message {
            role: Role::User,
            parts: vec![
                Part::Text("题目".into()),
                Part::Image {
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                },
            ],
        }],
        max_tokens: None,
    }
}

#[test]
fn openai_body_format() {
    let mut cfg = LlmConfig::new(ApiKind::OpenAi, "https://api.x.com/v1", "sk", "gpt-x");
    cfg.temperature = Some(0.1);
    cfg.json_mode = true;
    cfg.extra_body
        .insert("enable_thinking".into(), json!(false));
    let client = LlmClient::new(cfg).unwrap();
    let body = client.build_body(&sample_request());
    assert_eq!(
        body,
        json!({
            "model": "gpt-x",
            "stream": false,
            "messages": [
                {"role": "system", "content": "系统提示"},
                {"role": "user", "content": [
                    {"type": "text", "text": "题目"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
                ]}
            ],
            "response_format": {"type": "json_object"},
            "temperature": 0.1,
            "enable_thinking": false
        })
    );
    // 纯文本消息使用字符串 content，且未配置时不发送 max_tokens
    let text_only = ChatRequest {
        system: String::new(),
        messages: vec![Message::user_text("hi")],
        max_tokens: None,
    };
    let body = client.build_body(&text_only);
    assert_eq!(body["messages"], json!([{"role": "user", "content": "hi"}]));
    assert!(body.get("max_tokens").is_none());
}

#[test]
fn deepseek_v4_disables_thinking_by_default() {
    let cfg = LlmConfig::new(
        ApiKind::OpenAi,
        "https://api.deepseek.com/v1",
        "sk",
        "deepseek-v4-flash",
    );
    let body = LlmClient::new(cfg.clone())
        .unwrap()
        .build_body(&sample_request());
    assert_eq!(body["thinking"], json!({"type": "disabled"}));

    let mut custom = cfg;
    custom
        .extra_body
        .insert("thinking".into(), json!({"type": "enabled"}));
    let body = LlmClient::new(custom)
        .unwrap()
        .build_body(&sample_request());
    assert_eq!(body["thinking"], json!({"type": "enabled"}));
}

#[test]
fn anthropic_body_format() {
    let client = LlmClient::new(LlmConfig::new(
        ApiKind::Anthropic,
        "https://api.anthropic.com",
        "sk",
        "claude-haiku-5-5",
    ))
    .unwrap();
    let body = client.build_body(&sample_request());
    assert_eq!(
        body,
        json!({
            "model": "claude-haiku-5-5",
            "max_tokens": 2048,
            "system": "系统提示",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "题目"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}
            ]}]
        })
    );
}

#[test]
fn responses_are_parsed() {
    let openai = parse_openai(&json!({
        "choices": [{"message": {"content": null, "reasoning_content": "思考"}, "finish_reason": "length"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    }))
    .unwrap();
    assert_eq!(openai.text, "");
    assert_eq!(openai.reasoning, "思考");
    assert_eq!(openai.finish_reason.as_deref(), Some("length"));
    assert_eq!(
        (openai.input_tokens, openai.output_tokens),
        (Some(10), Some(5))
    );

    let parts = parse_openai(&json!({"choices": [{"message": {"content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}}]})).unwrap();
    assert_eq!(parts.text, "ab");
    assert!(parse_openai(&json!({"id": "x"})).is_err());

    let claude = parse_anthropic(&json!({
        "content": [{"type": "thinking", "thinking": "嗯"}, {"type": "text", "text": "{\"answer\":[\"A\"]}"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 3, "output_tokens": 4}
    }))
    .unwrap();
    assert_eq!(claude.text, "{\"answer\":[\"A\"]}");
    assert_eq!(claude.reasoning, "嗯");
    assert_eq!(claude.output_tokens, Some(4));
}

#[test]
fn error_messages_are_readable() {
    assert_eq!(
        error_message(r#"{"error": {"message": "Invalid API key"}}"#),
        "Invalid API key"
    );
    assert_eq!(
        error_message(
            r#"{"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}"#
        ),
        "Overloaded"
    );
    assert_eq!(
        error_message(r#"{"code": 20015, "msg": "参数错误"}"#),
        "参数错误"
    );
    assert_eq!(error_message("Bad Gateway"), "Bad Gateway");
}

#[tokio::test]
async fn openai_request_retries_on_rate_limit() {
    let server = MockServer::start(vec![
        MockResponse::json(429, json!({"error": {"message": "rate limited"}}))
            .with_header("Retry-After", "0"),
        MockResponse::json(
            200,
            json!({"choices": [{"message": {"content": "{\"answer\": [\"B\"]}"}}]}),
        ),
    ])
    .await;
    let client = LlmClient::new(fast(LlmConfig::new(
        ApiKind::OpenAi,
        &format!("{}/v1", server.url),
        "sk-test",
        "m",
    )))
    .unwrap();
    let resp = client.chat(&sample_request()).await.unwrap();
    assert_eq!(resp.text, "{\"answer\": [\"B\"]}");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/v1/chat/completions");
    assert_eq!(requests[0].header("authorization"), Some("Bearer sk-test"));
    assert_eq!(requests[1].json()["model"], "m");
}

#[tokio::test]
async fn anthropic_request_uses_native_headers() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({"content": [{"type": "text", "text": "2"}], "stop_reason": "end_turn"}),
    )])
    .await;
    let mut cfg = fast(LlmConfig::new(
        ApiKind::Anthropic,
        &server.url,
        "sk-ant-test",
        "claude-haiku-5-5",
    ));
    cfg.extra_headers
        .push(("anthropic-beta".into(), "test-beta".into()));
    let client = LlmClient::new(cfg).unwrap();
    let resp = client.check_connection().await.unwrap();
    assert_eq!(resp.text, "2");

    let req = &server.requests()[0];
    assert_eq!(req.path, "/v1/messages");
    assert_eq!(req.header("x-api-key"), Some("sk-ant-test"));
    assert_eq!(req.header("anthropic-version"), Some(ANTHROPIC_VERSION));
    assert_eq!(req.header("anthropic-beta"), Some("test-beta"));
    assert!(req.header("authorization").is_none());
    assert_eq!(req.json()["max_tokens"], 256);
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let server = MockServer::start(vec![MockResponse::json(
        401,
        json!({"error": {"message": "Invalid API key"}}),
    )])
    .await;
    let client = LlmClient::new(fast(LlmConfig::new(
        ApiKind::OpenAi,
        &server.url,
        "bad",
        "m",
    )))
    .unwrap();
    let err = client.chat(&sample_request()).await.unwrap_err();
    assert!(err.to_string().contains("HTTP 401"), "{err}");
    assert!(err.to_string().contains("Invalid API key"), "{err}");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn connection_check_rejects_empty_reply() {
    let server = MockServer::start(vec![MockResponse::json(
        200,
        json!({"choices": [{"message": {"content": ""}}]}),
    )])
    .await;
    let client =
        LlmClient::new(fast(LlmConfig::new(ApiKind::OpenAi, &server.url, "k", "m"))).unwrap();
    assert!(client.check_connection().await.is_err());
}

#[test]
fn local_endpoints_are_recognized_for_direct_connections() {
    for endpoint in [
        "http://127.0.0.1:11434/v1/chat/completions",
        "http://localhost:11434/v1",
        "http://[::1]:1234/v1",
        "http://192.168.1.10:1234/v1",
    ] {
        assert!(super::is_local_endpoint(endpoint), "{endpoint}");
    }
    for endpoint in [
        "https://api.openai.com/v1/chat/completions",
        "https://api.deepseek.com/v1",
    ] {
        assert!(!super::is_local_endpoint(endpoint), "{endpoint}");
    }
}
