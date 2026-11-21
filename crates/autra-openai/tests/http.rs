use autra_core::{HistoryItem, ModelCallSpec};
use autra_harness::{MODEL_PROTOCOL_VERSION, ModelProvider};
use autra_openai::{OpenAiCompatibleModel, OpenAiConfig};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn request() -> ModelCallSpec {
    ModelCallSpec {
        system: None,
        messages: vec![HistoryItem::User("hello".to_owned())],
        tools: Vec::new(),
    }
}

#[tokio::test]
async fn sends_bearer_token_and_parses_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "hello from model" } }]
        })))
        .mount(&server)
        .await;
    let model =
        OpenAiCompatibleModel::new(OpenAiConfig::new(server.uri(), "test-key", "test-model"))
            .unwrap();

    let output = model.complete(&request()).await.unwrap();

    assert_eq!(output.text.as_deref(), Some("hello from model"));
    assert!(output.tool_calls.is_empty());
    assert!(model.capabilities().tool_calling);
    assert_eq!(model.protocol_version(), MODEL_PROTOCOL_VERSION);
}

#[tokio::test]
async fn non_success_response_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .mount(&server)
        .await;
    let model =
        OpenAiCompatibleModel::new(OpenAiConfig::new(server.uri(), "test-key", "test-model"))
            .unwrap();

    let error = model.complete(&request()).await.unwrap_err();

    assert!(error.to_string().contains("401"));
    assert!(error.to_string().contains("bad key"));
}

#[tokio::test]
async fn malformed_json_error_includes_the_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    let model =
        OpenAiCompatibleModel::new(OpenAiConfig::new(server.uri(), "test-key", "test-model"))
            .unwrap();

    let error = model.complete(&request()).await.unwrap_err();

    assert!(error.to_string().contains("invalid model response JSON"));
    assert!(error.to_string().contains("not json"));
}

#[tokio::test]
async fn streams_content_deltas() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"he\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"llo\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("accept", "text/event-stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let model =
        OpenAiCompatibleModel::new(OpenAiConfig::new(server.uri(), "test-key", "test-model"))
            .unwrap();
    let mut deltas = Vec::new();

    let output = model
        .complete_streaming(&request(), &mut |delta| deltas.push(delta))
        .await
        .unwrap();

    assert_eq!(deltas, ["he", "llo"]);
    assert_eq!(output.text.as_deref(), Some("hello"));
    assert!(output.tool_calls.is_empty());
    assert!(model.capabilities().streaming);
}

#[tokio::test]
async fn non_event_stream_response_is_parsed_as_complete() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": "fallback" } }]
        })))
        .mount(&server)
        .await;
    let model =
        OpenAiCompatibleModel::new(OpenAiConfig::new(server.uri(), "test-key", "test-model"))
            .unwrap();
    let mut deltas = Vec::new();

    let output = model
        .complete_streaming(&request(), &mut |delta| deltas.push(delta))
        .await
        .unwrap();

    assert_eq!(deltas, ["fallback"]);
    assert_eq!(output.text.as_deref(), Some("fallback"));
}

#[tokio::test]
async fn streams_tool_call_fragments() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"text\\\":\\\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"hi\\\"}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("accept", "text/event-stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let model =
        OpenAiCompatibleModel::new(OpenAiConfig::new(server.uri(), "test-key", "test-model"))
            .unwrap();

    let output = model
        .complete_streaming(&request(), &mut |_| {})
        .await
        .unwrap();

    assert_eq!(output.tool_calls.len(), 1);
    assert_eq!(output.tool_calls[0].call_id, "call_1");
    assert_eq!(output.tool_calls[0].name, "echo");
    assert_eq!(output.tool_calls[0].arguments, r#"{"text":"hi"}"#);
}
