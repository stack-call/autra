use autra_core::{HistoryItem, ModelCallSpec};
use autra_harness::ModelProvider;
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
