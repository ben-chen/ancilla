use std::{convert::Infallible, sync::Arc};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, patch, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use thiserror::Error;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tower_http::{
    cors::{Any, CorsLayer},
    services::ServeDir,
};
use uuid::Uuid;

use crate::{
    model::{
        ApiErrorBody, AssembleContextRequest, ChatRespondRequest, ChatStreamEvent,
        CreateAudioEntryRequest, CreateMemoryRequest, CreateTextEntryRequest,
        GenerateMemoriesRequest, ImportMemoriesRequest, PatchMemoryRequest, SearchMemoriesRequest,
        SpeakRequest,
    },
    service::AppService,
};

#[derive(Clone)]
pub struct ApiState {
    pub service: Arc<AppService>,
    pub basic_auth: Option<BasicAuthConfig>,
}

#[derive(Clone, Debug)]
pub struct BasicAuthConfig {
    pub username: String,
    pub password: String,
}

impl BasicAuthConfig {
    fn is_authorized(&self, header_value: Option<&HeaderValue>) -> bool {
        let Some(header_value) = header_value else {
            return false;
        };
        let Ok(header_value) = header_value.to_str() else {
            return false;
        };
        let Some(encoded) = header_value.strip_prefix("Basic ") else {
            return false;
        };
        let Ok(decoded) = BASE64.decode(encoded) else {
            return false;
        };
        let Ok(decoded) = String::from_utf8(decoded) else {
            return false;
        };
        decoded == format!("{}:{}", self.username, self.password)
    }
}

pub fn router(service: AppService, basic_auth: Option<BasicAuthConfig>) -> Router {
    let state = ApiState {
        service: Arc::new(service),
        basic_auth,
    };
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let protected = Router::new()
        .route("/", get(frontend_index))
        .nest_service("/assets", ServeDir::new("web/dist/assets"))
        .route("/v1/entries/text", post(create_text_entry))
        .route("/v1/entries/audio", post(create_audio_entry))
        .route("/v1/memories", get(get_memories).post(create_memory))
        .route("/v1/memories/export", get(export_memories))
        .route("/v1/memories/import", post(import_memories))
        .route("/v1/memories/generate", post(generate_memories))
        .route("/v1/timeline", get(get_timeline))
        .route("/v1/chat/models", get(get_chat_models))
        .route("/v1/context/assemble", post(assemble_context))
        .route("/v1/memories/search", post(search_memories))
        .route(
            "/v1/memories/{id}",
            patch(patch_memory).delete(delete_memory),
        )
        .route("/v1/profile/blocks", get(profile_blocks))
        .route("/v1/chat/respond", post(chat_respond))
        .route("/v1/chat/respond/stream", post(chat_respond_stream))
        .route("/v1/speak", post(speak))
        .route("/v1/retrieval-traces/{id}", get(get_trace))
        .with_state(state.clone());

    let protected = if state.basic_auth.is_some() {
        protected.route_layer(middleware::from_fn_with_state(
            state.clone(),
            basic_auth_middleware,
        ))
    } else {
        protected
    };

    Router::new()
        .route("/healthz", get(health))
        .merge(protected)
        .layer(cors)
        .with_state(state)
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn frontend_index() -> Html<String> {
    let html = tokio::fs::read_to_string("web/dist/index.html")
        .await
        .unwrap_or_else(|_| {
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>Ancilla</title></head><body><main style=\"font-family:sans-serif;padding:2rem\"><h1>Ancilla</h1><p>The frontend has not been built yet.</p></main></body></html>".to_string()
        });
    Html(html)
}

async fn basic_auth_middleware(
    State(state): State<ApiState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let authorized = state
        .basic_auth
        .as_ref()
        .is_some_and(|auth| auth.is_authorized(request.headers().get(header::AUTHORIZATION)));
    if authorized {
        return next.run(request).await;
    }

    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(ApiErrorBody {
            error: "basic authentication required".to_string(),
        }),
    )
        .into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static(r#"Basic realm="ancilla""#),
    );
    response
}

async fn create_text_entry(
    State(state): State<ApiState>,
    Json(request): Json<CreateTextEntryRequest>,
) -> Result<Json<crate::model::CaptureEntryResponse>, ApiError> {
    Ok(Json(
        state
            .service
            .create_text_entry(request)
            .await?
            .without_embeddings(),
    ))
}

async fn create_audio_entry(
    State(state): State<ApiState>,
    Json(request): Json<CreateAudioEntryRequest>,
) -> Result<Json<crate::model::CaptureEntryResponse>, ApiError> {
    Ok(Json(
        state
            .service
            .create_audio_entry(request)
            .await?
            .without_embeddings(),
    ))
}

async fn create_memory(
    State(state): State<ApiState>,
    Json(request): Json<CreateMemoryRequest>,
) -> Result<Json<crate::model::CaptureEntryResponse>, ApiError> {
    Ok(Json(
        state
            .service
            .create_memory(request)
            .await?
            .without_embeddings(),
    ))
}

async fn generate_memories(
    State(state): State<ApiState>,
    Json(request): Json<GenerateMemoriesRequest>,
) -> Result<Json<crate::model::CaptureEntryResponse>, ApiError> {
    Ok(Json(
        state
            .service
            .generate_memories(request)
            .await?
            .without_embeddings(),
    ))
}

async fn export_memories(
    State(state): State<ApiState>,
) -> Json<crate::model::ExportMemoriesResponse> {
    Json(state.service.export_memories().await)
}

async fn import_memories(
    State(state): State<ApiState>,
    Json(request): Json<ImportMemoriesRequest>,
) -> Result<Json<crate::model::ImportMemoriesResponse>, ApiError> {
    let response = state.service.import_memories(request).await?;
    Ok(Json(crate::model::ImportMemoriesResponse {
        imported_count: response.imported_count,
        memories: response
            .memories
            .into_iter()
            .map(crate::model::MemoryRecord::without_embedding)
            .collect(),
    }))
}

async fn get_timeline(State(state): State<ApiState>) -> Json<Vec<crate::model::Entry>> {
    Json(state.service.list_timeline().await)
}

async fn get_memories(State(state): State<ApiState>) -> Json<Vec<crate::model::MemoryRecord>> {
    Json(
        state
            .service
            .review_memories()
            .await
            .into_iter()
            .filter(|memory| memory.state != crate::model::MemoryState::Deleted)
            .map(crate::model::MemoryRecord::without_embedding)
            .collect(),
    )
}

async fn assemble_context(
    State(state): State<ApiState>,
    Json(request): Json<AssembleContextRequest>,
) -> Result<Json<crate::model::AssembleContextResponse>, ApiError> {
    Ok(Json(
        state
            .service
            .assemble_context(request)
            .await?
            .without_embeddings(),
    ))
}

async fn search_memories(
    State(state): State<ApiState>,
    Json(request): Json<SearchMemoriesRequest>,
) -> Result<Json<Vec<crate::model::ScoredMemory>>, ApiError> {
    Ok(Json(
        state
            .service
            .search_memories(request)
            .await?
            .into_iter()
            .map(crate::model::ScoredMemory::without_embedding)
            .collect(),
    ))
}

async fn patch_memory(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(request): Json<PatchMemoryRequest>,
) -> Result<Json<crate::model::MemoryRecord>, ApiError> {
    Ok(Json(
        state
            .service
            .patch_memory(id, request)
            .await?
            .without_embedding(),
    ))
}

async fn delete_memory(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::model::MemoryRecord>, ApiError> {
    Ok(Json(
        state.service.delete_memory(id).await?.without_embedding(),
    ))
}

async fn profile_blocks(State(state): State<ApiState>) -> Json<Vec<crate::model::ProfileBlock>> {
    Json(state.service.profile_blocks().await)
}

async fn get_chat_models(State(state): State<ApiState>) -> Json<crate::model::ChatModelsResponse> {
    Json(state.service.chat_models())
}

async fn chat_respond(
    State(state): State<ApiState>,
    Json(request): Json<ChatRespondRequest>,
) -> Result<Json<crate::model::ChatResponse>, ApiError> {
    Ok(Json(
        state
            .service
            .chat_respond(request)
            .await?
            .without_embeddings(),
    ))
}

async fn chat_respond_stream(
    State(state): State<ApiState>,
    Json(request): Json<ChatRespondRequest>,
) -> Result<Response, ApiError> {
    let stream = state.service.chat_respond_stream(request).await?;
    let crate::service::ChatResponseStream {
        trace_id,
        injected_context,
        selected_memories,
        model_id,
        gate_metrics,
        chat_metrics,
        remember_current_conversation_used,
        remembered_memories_count,
        receiver,
    } = stream;
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(64);

    tokio::spawn(async move {
        let start = ChatStreamEvent::Start {
            trace_id,
            model_id: model_id.clone(),
            gate_metrics,
            injected_context,
            selected_memories,
            remember_current_conversation_used,
            remembered_memories_count,
        }
        .without_embeddings();
        if send_stream_event(&tx, &start).await.is_err() {
            return;
        }

        let mut receiver = receiver;
        while let Some(event) = receiver.recv().await {
            match event {
                Ok(crate::bedrock::ChatCompletionStreamEvent::Delta(delta)) => {
                    if send_stream_event(&tx, &ChatStreamEvent::Delta { delta })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(crate::bedrock::ChatCompletionStreamEvent::Done {
                    answer,
                    stop_reason,
                }) => {
                    let done = ChatStreamEvent::Done {
                        answer,
                        trace_id,
                        model_id: model_id.clone(),
                        stop_reason,
                        chat_metrics,
                    };
                    let _ = send_stream_event(&tx, &done).await;
                    return;
                }
                Err(error) => {
                    let error = ChatStreamEvent::Error {
                        error: error.to_string(),
                        trace_id,
                        model_id: model_id.clone(),
                    };
                    let _ = send_stream_event(&tx, &error).await;
                    return;
                }
            }
        }
    });

    let body = Body::from_stream(ReceiverStream::new(rx).map(Ok::<Bytes, Infallible>));
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/x-ndjson"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response())
}

async fn speak(
    State(state): State<ApiState>,
    Json(request): Json<SpeakRequest>,
) -> Result<Response, ApiError> {
    let output = state
        .service
        .synthesize_speech(&request.text, request.voice_id.as_deref())
        .await?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_str(&output.content_type)
                    .unwrap_or_else(|_| HeaderValue::from_static("audio/mpeg")),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        output.audio,
    )
        .into_response())
}

async fn get_trace(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::model::RetrievalTrace>, ApiError> {
    Ok(Json(state.service.retrieval_trace(id).await?))
}

async fn send_stream_event(
    tx: &tokio::sync::mpsc::Sender<Bytes>,
    event: &ChatStreamEvent,
) -> Result<(), tokio::sync::mpsc::error::SendError<Bytes>> {
    let mut line = serde_json::to_vec(event).expect("stream event should serialize");
    line.push(b'\n');
    tx.send(Bytes::from(line)).await
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Upstream(String),
    #[error("{0}")]
    Internal(String),
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        eprintln!("{error:#}");
        let message = format_error_chain(&error);
        if message.contains("model `") && message.contains("is not available on this server") {
            Self::BadRequest(message)
        } else if message.contains("not found") {
            Self::BadRequest(message)
        } else if message.contains("bedrock ")
            || message.contains("failed to embed retrieval query")
            || message.contains("embedder")
        {
            Self::Upstream(message)
        } else {
            Self::Internal(message)
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = Json(ApiErrorBody {
            error: self.to_string(),
        });
        (status, body).into_response()
    }
}

fn format_error_chain(error: &anyhow::Error) -> String {
    let mut parts = Vec::new();
    for cause in error.chain() {
        let message = cause.to_string();
        if parts.last().is_some_and(|previous| previous == &message) {
            continue;
        }
        parts.push(message);
    }
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::memory_markdown::markdown_from_plain_text;
    use crate::service::AppService;

    use super::*;

    #[tokio::test]
    async fn api_capture_and_context_routes_work() {
        let app = router(AppService::new_in_memory(), None);

        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/entries/text")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "raw_text": "I am building Ancilla, a personal memory system.",
                            "timezone": "UTC"
                            ,
                            "prepared_memories": [{
                                "content_markdown": "# Building Ancilla\n\nTags: project\n\nYou are building Ancilla, a personal memory system.",
                                "kind": "semantic",
                                "embedding": {
                                    "values": [0.1, 0.2, 0.3],
                                    "model": "test-model",
                                    "device": "cpu",
                                    "source": "test"
                                }
                            }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert!(value["memories"][0].get("embedding").is_none());

        let response = app
            .oneshot(
                Request::post("/v1/context/assemble")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "query": "Ancilla personal memory system"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["decision"], "inject_compact");
        assert!(value["context"].as_str().unwrap().contains("Ancilla"));
        assert!(value["selected_memories"][0].get("embedding").is_none());
        assert!(value["candidates"][0]["memory"].get("embedding").is_none());
    }

    #[tokio::test]
    async fn api_memory_export_and_import_routes_work() {
        let app = router(AppService::new_in_memory(), None);

        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/memories/import")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "memories": [{
                                "kind": "semantic",
                                "content_markdown": "# Portable API Test\n\nTags: api, import\n\nImported through the portable API.",
                                "attrs": { "source": "api-test" },
                                "observed_at": "2026-04-15T19:00:00Z",
                                "valid_from": "2026-04-15T19:00:00Z",
                                "thread_title": "Portable API"
                            }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["imported_count"].as_u64(), Some(1));
        assert!(value["memories"][0].get("embedding").is_none());

        let response = app
            .oneshot(
                Request::get("/v1/memories/export")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["memories"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            value["memories"][0]["thread_title"].as_str(),
            Some("Portable API")
        );
        assert_eq!(
            value["memories"][0]["attrs"]["source"].as_str(),
            Some("api-test")
        );
    }

    #[tokio::test]
    async fn api_patch_and_delete_routes_work() {
        let service = AppService::new_in_memory();
        let created = service
            .create_memory(CreateMemoryRequest {
                content_markdown: markdown_from_plain_text(
                    "You prefer Rust.",
                    &["preference".to_string()],
                ),
                kind: crate::model::MemoryKind::Semantic,
                captured_at: None,
                timezone: Some("UTC".to_string()),
                source_app: None,
                attrs: json!({}),
                observed_at: None,
                valid_from: None,
                valid_to: None,
                thread_title: None,
                metadata: json!({}),
            })
            .await
            .unwrap();
        let memory_id = created.memories[0].id;
        let app = router(service, None);

        let response = app
            .clone()
            .oneshot(
                Request::patch(format!("/v1/memories/{memory_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "content_markdown": "# You prefer Rust and SQLx.\n\nTags: preference\n\nYou prefer Rust and SQLx."
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::delete(format!("/v1/memories/{memory_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_generate_memories_route_creates_markdown_memories() {
        let app = router(AppService::new_in_memory(), None);

        let response = app
            .oneshot(
                Request::post("/v1/memories/generate")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "context_text": "I am building Ancilla, a personal memory system.",
                            "kind": "semantic",
                            "timezone": "UTC"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let memory = &value["memories"][0];
        assert_eq!(
            memory["title"],
            "I am building Ancilla, a personal memory system."
        );
        assert!(
            memory["content_markdown"]
                .as_str()
                .unwrap()
                .starts_with("# I am building Ancilla, a personal memory system.")
        );
    }

    #[tokio::test]
    async fn api_lists_memories_without_embeddings() {
        let service = AppService::new_in_memory();
        service
            .create_memory(CreateMemoryRequest {
                content_markdown: markdown_from_plain_text(
                    "You prefer sparkling water.",
                    &["preference".to_string()],
                ),
                kind: crate::model::MemoryKind::Semantic,
                captured_at: None,
                timezone: Some("UTC".to_string()),
                source_app: None,
                attrs: json!({}),
                observed_at: None,
                valid_from: None,
                valid_to: None,
                thread_title: None,
                metadata: json!({}),
            })
            .await
            .unwrap();
        let app = router(service, None);

        let response = app
            .oneshot(Request::get("/v1/memories").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value.as_array().unwrap().len(), 1);
        assert!(value[0].get("embedding").is_none());
        assert_eq!(value[0]["title"], "You prefer sparkling water.");
    }

    #[tokio::test]
    async fn api_hides_deleted_memories_from_memory_list() {
        let service = AppService::new_in_memory();
        let created = service
            .create_memory(CreateMemoryRequest {
                content_markdown: markdown_from_plain_text(
                    "Smoke test memory.",
                    &["test".to_string()],
                ),
                kind: crate::model::MemoryKind::Semantic,
                captured_at: None,
                timezone: Some("UTC".to_string()),
                source_app: None,
                attrs: json!({}),
                observed_at: None,
                valid_from: None,
                valid_to: None,
                thread_title: None,
                metadata: json!({}),
            })
            .await
            .unwrap();
        service.delete_memory(created.memories[0].id).await.unwrap();
        let app = router(service, None);

        let response = app
            .oneshot(Request::get("/v1/memories").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert!(value.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn api_chat_stream_route_returns_ndjson_events() {
        let service = AppService::new_in_memory();
        service
            .create_memory(CreateMemoryRequest {
                content_markdown: markdown_from_plain_text(
                    "You are building Ancilla.",
                    &["project".to_string()],
                ),
                kind: crate::model::MemoryKind::Semantic,
                captured_at: None,
                timezone: Some("UTC".to_string()),
                source_app: None,
                attrs: json!({}),
                observed_at: None,
                valid_from: None,
                valid_to: None,
                thread_title: None,
                metadata: json!({}),
            })
            .await
            .unwrap();
        let app = router(service, None);

        let response = app
            .oneshot(
                Request::post("/v1/chat/respond/stream")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "message": "What am I building?"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/x-ndjson"
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events = std::str::from_utf8(&body)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<ChatStreamEvent>(line).unwrap())
            .collect::<Vec<_>>();

        assert!(matches!(
            events.first(),
            Some(ChatStreamEvent::Start { .. })
        ));
        assert!(matches!(events.last(), Some(ChatStreamEvent::Done { .. })));
        assert!(events.iter().any(|event| matches!(
            event,
            ChatStreamEvent::Delta { delta } if delta.contains("Respond to:")
        )));
        match &events[0] {
            ChatStreamEvent::Start {
                selected_memories, ..
            } => {
                assert!(!selected_memories.is_empty());
                assert!(selected_memories[0].embedding.is_none());
            }
            _ => panic!("expected start event"),
        }
    }

    #[test]
    fn api_error_formats_full_chain_and_maps_bedrock_to_bad_gateway() {
        let error = anyhow::anyhow!("dispatch failure")
            .context("bedrock chat request failed for model `moonshotai.kimi-k2.5`");

        let api_error = ApiError::from(error);
        let response = api_error.into_response();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn api_basic_auth_protects_routes_but_not_healthz() {
        let app = router(
            AppService::new_in_memory(),
            Some(BasicAuthConfig {
                username: "ancilla".to_string(),
                password: "secret".to_string(),
            }),
        );

        let unauthorized = app
            .clone()
            .oneshot(Request::get("/v1/timeline").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            unauthorized
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .unwrap(),
            r#"Basic realm="ancilla""#
        );

        let health = app
            .clone()
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let token = BASE64.encode("ancilla:secret");
        let authorized = app
            .oneshot(
                Request::get("/v1/timeline")
                    .header(header::AUTHORIZATION, format!("Basic {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);
    }
}
