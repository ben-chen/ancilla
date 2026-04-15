use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    model::{
        ApiErrorBody, AssembleContextRequest, ChatRespondRequest, CreateAudioEntryRequest,
        CreateMemoryRequest, CreateTextEntryRequest, PatchMemoryRequest, SearchMemoriesRequest,
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

    let protected = Router::new()
        .route("/v1/entries/text", post(create_text_entry))
        .route("/v1/entries/audio", post(create_audio_entry))
        .route("/v1/memories", post(create_memory))
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
        .with_state(state)
}

async fn health() -> StatusCode {
    StatusCode::OK
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

async fn get_timeline(State(state): State<ApiState>) -> Json<Vec<crate::model::Entry>> {
    Json(state.service.list_timeline().await)
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

async fn get_trace(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::model::RetrievalTrace>, ApiError> {
    Ok(Json(state.service.retrieval_trace(id).await?))
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
                                "display_text": "You are building Ancilla, a personal memory system.",
                                "retrieval_text": "project Ancilla personal memory system",
                                "kind": "semantic",
                                "subtype": "project",
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
    async fn api_patch_and_delete_routes_work() {
        let service = AppService::new_in_memory();
        let created = service
            .create_memory(CreateMemoryRequest {
                display_text: "You prefer Rust.".to_string(),
                retrieval_text: None,
                kind: crate::model::MemoryKind::Semantic,
                subtype: crate::model::MemorySubtype::Preference,
                captured_at: None,
                timezone: Some("UTC".to_string()),
                source_app: None,
                attrs: json!({}),
                observed_at: None,
                valid_from: None,
                valid_to: None,
                confidence: None,
                salience: None,
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
                            "display_text": "You prefer Rust and SQLx."
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

    #[test]
    fn api_error_formats_full_chain_and_maps_bedrock_to_bad_gateway() {
        let error = anyhow::anyhow!("dispatch failure")
            .context("bedrock chat request failed for model `moonshot.kimi-k2-thinking`");

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
