use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    model::{
        ApiErrorBody, AssembleContextRequest, ChatRespondRequest, CreateAudioEntryRequest,
        CreateTextEntryRequest, PatchMemoryRequest, SearchMemoriesRequest,
    },
    service::AppService,
};

#[derive(Clone)]
pub struct ApiState {
    pub service: Arc<AppService>,
}

pub fn router(service: AppService) -> Router {
    let state = ApiState {
        service: Arc::new(service),
    };

    Router::new()
        .route("/healthz", get(health))
        .route("/v1/entries/text", post(create_text_entry))
        .route("/v1/entries/audio", post(create_audio_entry))
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
        .with_state(state)
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn create_text_entry(
    State(state): State<ApiState>,
    Json(request): Json<CreateTextEntryRequest>,
) -> Result<Json<crate::model::CaptureEntryResponse>, ApiError> {
    Ok(Json(state.service.create_text_entry(request).await?))
}

async fn create_audio_entry(
    State(state): State<ApiState>,
    Json(request): Json<CreateAudioEntryRequest>,
) -> Result<Json<crate::model::CaptureEntryResponse>, ApiError> {
    Ok(Json(state.service.create_audio_entry(request).await?))
}

async fn get_timeline(State(state): State<ApiState>) -> Json<Vec<crate::model::Entry>> {
    Json(state.service.list_timeline().await)
}

async fn assemble_context(
    State(state): State<ApiState>,
    Json(request): Json<AssembleContextRequest>,
) -> Result<Json<crate::model::AssembleContextResponse>, ApiError> {
    Ok(Json(state.service.assemble_context(request).await?))
}

async fn search_memories(
    State(state): State<ApiState>,
    Json(request): Json<SearchMemoriesRequest>,
) -> Result<Json<Vec<crate::model::ScoredMemory>>, ApiError> {
    Ok(Json(state.service.search_memories(request).await?))
}

async fn patch_memory(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(request): Json<PatchMemoryRequest>,
) -> Result<Json<crate::model::MemoryRecord>, ApiError> {
    Ok(Json(state.service.patch_memory(id, request).await?))
}

async fn delete_memory(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::model::MemoryRecord>, ApiError> {
    Ok(Json(state.service.delete_memory(id).await?))
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
    Ok(Json(state.service.chat_respond(request).await?))
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
    Internal(String),
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        eprintln!("{error:#}");
        let message = error.to_string();
        if message.contains("not found") {
            Self::BadRequest(message)
        } else {
            Self::Internal(message)
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = Json(ApiErrorBody {
            error: self.to_string(),
        });
        (status, body).into_response()
    }
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
        let app = router(AppService::new_in_memory());

        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/entries/text")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "raw_text": "I prefer Rust. I'm building a personal memory system.",
                            "timezone": "UTC"
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
                Request::post("/v1/context/assemble")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "query": "What am I building?"
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
        assert!(
            value["context"]
                .as_str()
                .unwrap()
                .contains("personal memory system")
        );
    }

    #[tokio::test]
    async fn api_patch_and_delete_routes_work() {
        let service = AppService::new_in_memory();
        let created = service
            .create_text_entry(CreateTextEntryRequest {
                raw_text: "I prefer Rust.".to_string(),
                captured_at: None,
                timezone: Some("UTC".to_string()),
                source_app: None,
                prepared_artifacts: Vec::new(),
                prepared_memories: Vec::new(),
                metadata: json!({}),
            })
            .await
            .unwrap();
        let memory_id = created.memories[0].id;
        let app = router(service);

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
}
