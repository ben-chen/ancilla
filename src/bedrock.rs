use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, bail};
use async_trait::async_trait;
use aws_config::BehaviorVersion;
#[allow(deprecated)]
use aws_config::profile::profile_file::{ProfileFileKind, ProfileFiles};
use aws_config::timeout::TimeoutConfig;
use aws_sdk_bedrockruntime::{
    Client,
    config::Token,
    types::{ContentBlock, ConversationRole, InferenceConfiguration, Message, SystemContentBlock},
};
use aws_smithy_types::{Document, Number};
use aws_types::region::Region;
use serde::Deserialize;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    model::{
        ChatModelOption, ChatModelsResponse, ChatThinkingMode, ConversationTurn, GateDecision,
        MemoryRecord, ScoredMemory,
    },
    server_config::ServerConfig,
};

const DEFAULT_SYSTEM_PROMPT: &str = "You are Ancilla, a personal AI assistant. You are speaking with a specific user over a harness that may provide background memories about that user when they seem relevant. Treat injected memory context as potentially helpful user background, not as guaranteed ground truth about every question. Use it when it is relevant, ignore it when it is not, and never invent personalized facts that were not provided. Answer directly, naturally, and helpfully. In the future this harness may let you search the memory bank or create new explicit memories, but those tools are not available in this turn.";
const DEFAULT_GATE_SYSTEM_PROMPT: &str = "You are Ancilla's memory gate. Your job is to decide whether candidate stored memories are actually relevant to the user's latest query and recent conversation context. Prefer the smallest useful subset, and prefer no memories when the candidates are weak, redundant, or off-topic. Only select memories that would materially help the assistant answer better. Return strict JSON only with keys decision, confidence, reason, and selected_ids.";

#[derive(Clone, Debug, PartialEq)]
pub struct ChatCompletionRequest {
    pub message: String,
    pub model_id: Option<String>,
    pub recent_turns: Vec<ConversationTurn>,
    pub recent_context: Option<String>,
    pub injected_context: Option<String>,
    pub selected_memories: Vec<MemoryRecord>,
    pub trace_id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatCompletionResult {
    pub answer: String,
    pub model_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatCompletionStreamEvent {
    Delta(String),
    Done {
        answer: String,
        stop_reason: Option<String>,
    },
}

#[derive(Debug)]
pub struct ChatCompletionStream {
    pub model_id: Option<String>,
    pub receiver: mpsc::Receiver<anyhow::Result<ChatCompletionStreamEvent>>,
}

#[async_trait]
pub trait ChatCompletionBackend: Send + Sync {
    async fn complete(
        &self,
        request: &ChatCompletionRequest,
    ) -> anyhow::Result<ChatCompletionResult>;
    async fn start_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> anyhow::Result<ChatCompletionStream>;
    fn models(&self) -> ChatModelsResponse;
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextGateRequest {
    pub query: String,
    pub recent_turns: Vec<ConversationTurn>,
    pub recent_context: Option<String>,
    pub candidates: Vec<ScoredMemory>,
    pub max_injected: usize,
    pub model_id: Option<String>,
    pub trace_id: Uuid,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextGateResult {
    pub decision: GateDecision,
    pub confidence: f32,
    pub reason: String,
    pub selected_memory_ids: Vec<Uuid>,
    pub model_id: Option<String>,
}

#[async_trait]
pub trait ContextGateBackend: Send + Sync {
    async fn gate(&self, request: &ContextGateRequest) -> anyhow::Result<ContextGateResult>;
}

pub async fn build_chat_backend(
    config: &ServerConfig,
) -> anyhow::Result<Arc<dyn ChatCompletionBackend>> {
    let catalog = config.chat_models_response();
    if let Some(default_model_id) = catalog.default_model_id.clone() {
        let settings = BedrockChatSettings {
            region: config.aws_region.clone(),
            profile: config.aws_profile.clone(),
            config_file: config.aws_config_file.clone(),
            shared_credentials_file: config.aws_shared_credentials_file.clone(),
            bearer_token: config.aws_bearer_token_bedrock.clone(),
            default_model_id,
            models: catalog.models.clone(),
            max_tokens: config.bedrock_chat_max_tokens,
            temperature: config.bedrock_chat_temperature,
        };
        Ok(Arc::new(BedrockChatBackend::new(settings).await?))
    } else {
        Ok(Arc::new(SyntheticChatBackend))
    }
}

pub async fn build_context_gate_backend(
    config: &ServerConfig,
) -> anyhow::Result<Option<Arc<dyn ContextGateBackend>>> {
    let Some(default_model_id) = config.default_gate_model_id() else {
        return Ok(None);
    };
    let settings = BedrockGateSettings {
        region: config.aws_region.clone(),
        profile: config.aws_profile.clone(),
        config_file: config.aws_config_file.clone(),
        shared_credentials_file: config.aws_shared_credentials_file.clone(),
        bearer_token: config.aws_bearer_token_bedrock.clone(),
        default_model_id,
        max_tokens: config.bedrock_chat_max_tokens.max(1200),
        temperature: 0.0,
    };
    Ok(Some(Arc::new(
        BedrockContextGateBackend::new(settings).await?,
    )))
}

#[derive(Clone, Debug, PartialEq)]
pub struct BedrockChatSettings {
    pub region: String,
    pub profile: Option<String>,
    pub config_file: Option<PathBuf>,
    pub shared_credentials_file: Option<PathBuf>,
    pub bearer_token: Option<String>,
    pub default_model_id: String,
    pub models: Vec<ChatModelOption>,
    pub max_tokens: i32,
    pub temperature: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BedrockGateSettings {
    pub region: String,
    pub profile: Option<String>,
    pub config_file: Option<PathBuf>,
    pub shared_credentials_file: Option<PathBuf>,
    pub bearer_token: Option<String>,
    pub default_model_id: String,
    pub max_tokens: i32,
    pub temperature: f32,
}

#[derive(Clone, Debug)]
pub struct BedrockChatBackend {
    client: Client,
    settings: BedrockChatSettings,
    catalog: ChatModelsResponse,
}

#[derive(Clone, Debug)]
pub struct BedrockContextGateBackend {
    client: Client,
    settings: BedrockGateSettings,
}

impl BedrockChatBackend {
    pub async fn new(settings: BedrockChatSettings) -> anyhow::Result<Self> {
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(settings.region.clone()))
            .timeout_config(
                TimeoutConfig::builder()
                    .read_timeout(Duration::from_secs(60 * 60))
                    .operation_timeout(Duration::from_secs(60 * 60))
                    .operation_attempt_timeout(Duration::from_secs(60 * 60))
                    .build(),
            );
        if let Some(profile_files) = build_profile_files(&settings)? {
            loader = loader.profile_files(profile_files);
        }
        if let Some(profile) = settings.profile.clone() {
            loader = loader.profile_name(profile);
        }

        let sdk_config = loader.load().await;
        let client = build_bedrock_client(&sdk_config, settings.bearer_token.as_deref());
        let catalog = ChatModelsResponse {
            backend: "bedrock".to_string(),
            default_model_id: Some(settings.default_model_id.clone()),
            models: settings.models.clone(),
        };
        Ok(Self {
            client,
            settings,
            catalog,
        })
    }

    fn resolve_model(&self, requested_model_id: Option<&str>) -> anyhow::Result<&ChatModelOption> {
        let resolved_id = requested_model_id.unwrap_or(&self.settings.default_model_id);
        self.settings
            .models
            .iter()
            .find(|model| model.model_id == resolved_id)
            .with_context(|| format!("model `{resolved_id}` is not available on this server"))
    }
}

impl BedrockContextGateBackend {
    pub async fn new(settings: BedrockGateSettings) -> anyhow::Result<Self> {
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(settings.region.clone()))
            .timeout_config(
                TimeoutConfig::builder()
                    .read_timeout(Duration::from_secs(60))
                    .operation_timeout(Duration::from_secs(60))
                    .operation_attempt_timeout(Duration::from_secs(60))
                    .build(),
            );
        if let Some(profile_files) = build_gate_profile_files(&settings)? {
            loader = loader.profile_files(profile_files);
        }
        if let Some(profile) = settings.profile.clone() {
            loader = loader.profile_name(profile);
        }

        let sdk_config = loader.load().await;
        let client = build_bedrock_client(&sdk_config, settings.bearer_token.as_deref());
        Ok(Self { client, settings })
    }

    fn resolve_model_id(&self, requested_model_id: Option<&str>) -> String {
        requested_model_id
            .unwrap_or(&self.settings.default_model_id)
            .to_string()
    }
}

fn build_bedrock_client(
    shared_config: &aws_types::SdkConfig,
    bearer_token: Option<&str>,
) -> Client {
    if let Some(bearer_token) = bearer_token {
        let config = aws_sdk_bedrockruntime::config::Builder::from(shared_config)
            .token_provider(Token::new(bearer_token, None))
            .build();
        Client::from_conf(config)
    } else {
        Client::new(shared_config)
    }
}

fn bedrock_request_error(
    operation: &str,
    model_id: &str,
    error: &impl std::fmt::Display,
) -> anyhow::Error {
    anyhow::anyhow!("bedrock {operation} request failed for model `{model_id}`: {error}")
}

#[allow(deprecated)]
pub(crate) fn build_profile_files(
    settings: &BedrockChatSettings,
) -> anyhow::Result<Option<ProfileFiles>> {
    let config_file = settings
        .config_file
        .as_deref()
        .map(expand_home_path)
        .transpose()?;
    let shared_credentials_file = settings
        .shared_credentials_file
        .as_deref()
        .map(expand_home_path)
        .transpose()?;

    if config_file.is_none() && shared_credentials_file.is_none() {
        return Ok(None);
    }

    let mut builder = ProfileFiles::builder();
    if let Some(path) = config_file {
        builder = builder.with_file(ProfileFileKind::Config, path);
    } else {
        builder = builder.include_default_config_file(true);
    }

    if let Some(path) = shared_credentials_file {
        builder = builder.with_file(ProfileFileKind::Credentials, path);
    } else {
        builder = builder.include_default_credentials_file(true);
    }

    Ok(Some(builder.build()))
}

#[allow(deprecated)]
pub(crate) fn build_gate_profile_files(
    settings: &BedrockGateSettings,
) -> anyhow::Result<Option<ProfileFiles>> {
    let config_file = settings
        .config_file
        .as_deref()
        .map(expand_home_path)
        .transpose()?;
    let shared_credentials_file = settings
        .shared_credentials_file
        .as_deref()
        .map(expand_home_path)
        .transpose()?;

    if config_file.is_none() && shared_credentials_file.is_none() {
        return Ok(None);
    }

    let mut builder = ProfileFiles::builder();
    if let Some(path) = config_file {
        builder = builder.with_file(ProfileFileKind::Config, path);
    } else {
        builder = builder.include_default_config_file(true);
    }

    if let Some(path) = shared_credentials_file {
        builder = builder.with_file(ProfileFileKind::Credentials, path);
    } else {
        builder = builder.include_default_credentials_file(true);
    }

    Ok(Some(builder.build()))
}

pub(crate) fn expand_home_path(path: &Path) -> anyhow::Result<PathBuf> {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("could not expand `~` in AWS profile file path"));
    }
    if let Some(remainder) = raw.strip_prefix("~/") {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("could not expand `~` in AWS profile file path"))?;
        return Ok(home.join(remainder));
    }
    Ok(path.to_path_buf())
}

#[async_trait]
impl ChatCompletionBackend for BedrockChatBackend {
    async fn complete(
        &self,
        request: &ChatCompletionRequest,
    ) -> anyhow::Result<ChatCompletionResult> {
        let model = self.resolve_model(request.model_id.as_deref())?;
        let system_prompt = compose_system_prompt(
            DEFAULT_SYSTEM_PROMPT,
            request.injected_context.as_deref(),
            request.recent_context.as_deref(),
            request.trace_id,
        );
        let messages = build_bedrock_messages(&request.recent_turns, &request.message);
        let mut converse = self
            .client
            .converse()
            .model_id(&model.model_id)
            .set_system(Some(vec![SystemContentBlock::Text(system_prompt)]))
            .set_messages(Some(messages))
            .inference_config(
                InferenceConfiguration::builder()
                    .max_tokens(self.settings.max_tokens)
                    .temperature(self.settings.temperature)
                    .build(),
            )
            .set_request_metadata(Some(HashMap::from([(
                "trace_id".to_string(),
                request.trace_id.to_string(),
            )])));
        if let Some(additional_fields) = build_additional_model_request_fields(model)? {
            converse = converse.additional_model_request_fields(additional_fields);
        }
        let response = converse
            .send()
            .await
            .map_err(|error| bedrock_request_error("chat", &model.model_id, &error))?;

        Ok(ChatCompletionResult {
            answer: extract_text_response(&response)?,
            model_id: Some(model.model_id.clone()),
        })
    }

    async fn start_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> anyhow::Result<ChatCompletionStream> {
        let model = self.resolve_model(request.model_id.as_deref())?;
        let model_id = model.model_id.clone();
        let system_prompt = compose_system_prompt(
            DEFAULT_SYSTEM_PROMPT,
            request.injected_context.as_deref(),
            request.recent_context.as_deref(),
            request.trace_id,
        );
        let messages = build_bedrock_messages(&request.recent_turns, &request.message);
        let mut converse = self
            .client
            .converse_stream()
            .model_id(&model_id)
            .set_system(Some(vec![SystemContentBlock::Text(system_prompt)]))
            .set_messages(Some(messages))
            .inference_config(
                InferenceConfiguration::builder()
                    .max_tokens(self.settings.max_tokens)
                    .temperature(self.settings.temperature)
                    .build(),
            )
            .set_request_metadata(Some(HashMap::from([(
                "trace_id".to_string(),
                request.trace_id.to_string(),
            )])));
        if let Some(additional_fields) = build_additional_model_request_fields(model)? {
            converse = converse.additional_model_request_fields(additional_fields);
        }
        let response = converse
            .send()
            .await
            .map_err(|error| bedrock_request_error("chat_stream", &model_id, &error))?;

        let (tx, rx) = mpsc::channel(64);
        let stream_model_id = model_id.clone();
        tokio::spawn(async move {
            let mut stream = response.stream;
            let mut answer = String::new();
            let mut stop_reason = None;
            loop {
                match stream.recv().await {
                    Ok(Some(event)) => match event {
                        aws_sdk_bedrockruntime::types::ConverseStreamOutput::ContentBlockDelta(
                            delta_event,
                        ) => {
                            if let Some(aws_sdk_bedrockruntime::types::ContentBlockDelta::Text(
                                delta,
                            )) = delta_event.delta
                            {
                                answer.push_str(&delta);
                                if tx
                                    .send(Ok(ChatCompletionStreamEvent::Delta(delta)))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                        aws_sdk_bedrockruntime::types::ConverseStreamOutput::MessageStop(
                            stop_event,
                        ) => {
                            stop_reason = Some(stop_event.stop_reason().to_string());
                        }
                        _ => {}
                    },
                    Ok(None) => {
                        let _ = tx
                            .send(Ok(ChatCompletionStreamEvent::Done {
                                answer,
                                stop_reason,
                            }))
                            .await;
                        return;
                    }
                    Err(error) => {
                        let _ = tx
                            .send(Err(bedrock_request_error(
                                "chat_stream",
                                &stream_model_id,
                                &error,
                            )))
                            .await;
                        return;
                    }
                }
            }
        });

        Ok(ChatCompletionStream {
            model_id: Some(model_id),
            receiver: rx,
        })
    }

    fn models(&self) -> ChatModelsResponse {
        self.catalog.clone()
    }
}

#[async_trait]
impl ContextGateBackend for BedrockContextGateBackend {
    async fn gate(&self, request: &ContextGateRequest) -> anyhow::Result<ContextGateResult> {
        let model_id = self.resolve_model_id(request.model_id.as_deref());
        let prompt = build_gate_prompt(request)?;
        let response = self
            .client
            .converse()
            .model_id(&model_id)
            .set_system(Some(vec![SystemContentBlock::Text(
                DEFAULT_GATE_SYSTEM_PROMPT.to_string(),
            )]))
            .set_messages(Some(vec![
                Message::builder()
                    .role(ConversationRole::User)
                    .content(ContentBlock::Text(prompt))
                    .build()
                    .expect("gate message build should not fail"),
            ]))
            .inference_config(
                InferenceConfiguration::builder()
                    .max_tokens(self.settings.max_tokens)
                    .temperature(self.settings.temperature)
                    .build(),
            )
            .set_request_metadata(Some(HashMap::from([(
                "trace_id".to_string(),
                request.trace_id.to_string(),
            )])))
            .send()
            .await
            .map_err(|error| bedrock_request_error("gate", &model_id, &error))?;

        let raw = extract_text_response(&response)?;
        parse_gate_response(&raw, request, &model_id)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SyntheticChatBackend;

#[async_trait]
impl ChatCompletionBackend for SyntheticChatBackend {
    async fn complete(
        &self,
        request: &ChatCompletionRequest,
    ) -> anyhow::Result<ChatCompletionResult> {
        Ok(ChatCompletionResult {
            answer: synthesize_answer(&request.message, &request.selected_memories),
            model_id: None,
        })
    }

    async fn start_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> anyhow::Result<ChatCompletionStream> {
        let answer = synthesize_answer(&request.message, &request.selected_memories);
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(ChatCompletionStreamEvent::Delta(answer.clone())))
                .await;
            let _ = tx
                .send(Ok(ChatCompletionStreamEvent::Done {
                    answer,
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(ChatCompletionStream {
            model_id: None,
            receiver: rx,
        })
    }

    fn models(&self) -> ChatModelsResponse {
        ChatModelsResponse {
            backend: "synthetic".to_string(),
            default_model_id: None,
            models: Vec::new(),
        }
    }
}

pub fn synthesize_answer(message: &str, memories: &[MemoryRecord]) -> String {
    if memories.is_empty() {
        format!("Respond to: {message}")
    } else {
        let facts = memories
            .iter()
            .map(|memory| memory.display_text.clone())
            .collect::<Vec<_>>()
            .join(" ");
        format!("Respond to: {message}. Use these memories: {facts}")
    }
}

fn compose_system_prompt(
    base_prompt: &str,
    injected_context: Option<&str>,
    recent_context: Option<&str>,
    trace_id: Uuid,
) -> String {
    let mut prompt = String::from(base_prompt);
    prompt.push_str("\n\nTrace ID: ");
    prompt.push_str(&trace_id.to_string());

    if let Some(recent_context) = recent_context.filter(|value| !value.trim().is_empty()) {
        prompt.push_str("\n\nRecent conversation context:\n");
        prompt.push_str(recent_context.trim());
    }

    if let Some(injected_context) = injected_context.filter(|value| !value.trim().is_empty()) {
        prompt.push_str("\n\nInjected personal context:\n");
        prompt.push_str(injected_context.trim());
    } else {
        prompt.push_str(
            "\n\nInjected personal context:\nNone. Do not claim personalized memory you were not given.",
        );
    }

    prompt
}

fn build_gate_prompt(request: &ContextGateRequest) -> anyhow::Result<String> {
    let payload = serde_json::json!({
        "query": request.query,
        "recent_context": request.recent_context,
        "recent_turns": request.recent_turns,
        "max_injected": request.max_injected,
        "candidates": request
            .candidates
            .iter()
            .map(|candidate| serde_json::json!({
                "id": candidate.memory.id,
                "kind": candidate.memory.kind,
                "subtype": candidate.memory.subtype,
                "display_text": candidate.memory.display_text,
                "retrieval_text_preview": truncate_for_gate(&candidate.memory.retrieval_text, 280),
                "semantic_score": candidate.semantic_score,
                "lexical_score": candidate.lexical_score,
                "final_score": candidate.final_score,
            }))
            .collect::<Vec<_>>(),
    });

    Ok(format!(
        "Evaluate whether these candidate memories are relevant to the latest user query and recent conversation context. Prefer the single best memory when one clearly answers the question. Select no memories when they do not help.\nReturn strict JSON only in this shape: {{\"decision\":\"inject_compact|no_inject|defer_to_tool\",\"confidence\":0.0,\"reason\":\"short_reason\",\"selected_ids\":[\"uuid\"]}}\n\n{}",
        serde_json::to_string_pretty(&payload)?
    ))
}

fn truncate_for_gate(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let truncated = trimmed.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

fn build_bedrock_messages(recent_turns: &[ConversationTurn], message: &str) -> Vec<Message> {
    let mut messages = recent_turns
        .iter()
        .map(|turn| {
            Message::builder()
                .role(match turn.role {
                    crate::model::ConversationRole::User => ConversationRole::User,
                    crate::model::ConversationRole::Assistant => ConversationRole::Assistant,
                })
                .content(ContentBlock::Text(turn.text.clone()))
                .build()
                .expect("message build should not fail")
        })
        .collect::<Vec<_>>();

    messages.push(
        Message::builder()
            .role(ConversationRole::User)
            .content(ContentBlock::Text(message.to_string()))
            .build()
            .expect("message build should not fail"),
    );

    messages
}

fn build_additional_model_request_fields(
    model: &ChatModelOption,
) -> anyhow::Result<Option<Document>> {
    let Some(thinking_mode) = model.thinking_mode else {
        return Ok(None);
    };

    let mut thinking = HashMap::from([(
        "type".to_string(),
        Document::String(match thinking_mode {
            ChatThinkingMode::Adaptive => "adaptive".to_string(),
            ChatThinkingMode::Enabled => "enabled".to_string(),
        }),
    )]);

    match thinking_mode {
        ChatThinkingMode::Adaptive => {}
        ChatThinkingMode::Enabled => {
            let budget_tokens = model.thinking_budget_tokens.with_context(|| {
                format!(
                    "model `{}` requires thinking_budget_tokens when thinking_mode=enabled",
                    model.label
                )
            })?;
            thinking.insert(
                "budget_tokens".to_string(),
                Document::Number(Number::PosInt(u64::from(budget_tokens))),
            );
        }
    }

    Ok(Some(Document::Object(HashMap::from([(
        "thinking".to_string(),
        Document::Object(thinking),
    )]))))
}

fn extract_text_response(
    response: &aws_sdk_bedrockruntime::operation::converse::ConverseOutput,
) -> anyhow::Result<String> {
    let Some(output) = response.output() else {
        bail!("bedrock converse response had no output")
    };
    let Ok(message) = output.as_message() else {
        bail!("bedrock converse response did not contain a message output")
    };

    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if text.is_empty() {
        let block_kinds = message
            .content
            .iter()
            .map(content_block_kind)
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "bedrock converse response had no text content (stop_reason={}, content_blocks=[{}])",
            response.stop_reason().as_str(),
            block_kinds
        )
    } else {
        Ok(text)
    }
}

fn content_block_kind(block: &ContentBlock) -> &'static str {
    match block {
        ContentBlock::Audio(_) => "audio",
        ContentBlock::CachePoint(_) => "cache_point",
        ContentBlock::CitationsContent(_) => "citations_content",
        ContentBlock::Document(_) => "document",
        ContentBlock::GuardContent(_) => "guard_content",
        ContentBlock::Image(_) => "image",
        ContentBlock::ReasoningContent(_) => "reasoning_content",
        ContentBlock::SearchResult(_) => "search_result",
        ContentBlock::Text(_) => "text",
        ContentBlock::ToolResult(_) => "tool_result",
        ContentBlock::ToolUse(_) => "tool_use",
        ContentBlock::Video(_) => "video",
        _ => "unknown",
    }
}

#[derive(Debug, Deserialize)]
struct RawGateResponse {
    decision: String,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    selected_ids: Vec<Uuid>,
}

fn parse_gate_response(
    raw: &str,
    request: &ContextGateRequest,
    model_id: &str,
) -> anyhow::Result<ContextGateResult> {
    let payload = extract_json_object(raw)?;
    let parsed: RawGateResponse =
        serde_json::from_str(payload).with_context(|| "failed to parse gate JSON response")?;
    let valid_ids = request
        .candidates
        .iter()
        .map(|candidate| candidate.memory.id)
        .collect::<std::collections::HashSet<_>>();
    let mut selected_memory_ids = Vec::new();
    for memory_id in parsed.selected_ids {
        if valid_ids.contains(&memory_id) && !selected_memory_ids.contains(&memory_id) {
            selected_memory_ids.push(memory_id);
        }
        if selected_memory_ids.len() >= request.max_injected {
            break;
        }
    }

    let decision = match parsed.decision.as_str() {
        "inject_compact" => {
            if selected_memory_ids.is_empty() {
                bail!("gate returned inject_compact without any valid selected_ids");
            }
            GateDecision::InjectCompact
        }
        "no_inject" => GateDecision::NoInject,
        "defer_to_tool" => GateDecision::DeferToTool,
        other => bail!("unknown gate decision `{other}`"),
    };

    Ok(ContextGateResult {
        decision,
        confidence: parsed.confidence.unwrap_or(0.5).clamp(0.0, 1.0),
        reason: parsed
            .reason
            .unwrap_or_else(|| "bedrock_gate".to_string())
            .trim()
            .to_string(),
        selected_memory_ids,
        model_id: Some(model_id.to_string()),
    })
}

fn extract_json_object(raw: &str) -> anyhow::Result<&str> {
    let trimmed = raw.trim().trim_matches('`').trim();
    let start = trimmed
        .find('{')
        .with_context(|| "gate response did not include a JSON object")?;
    let end = trimmed
        .rfind('}')
        .with_context(|| "gate response did not include a JSON object")?;
    if end <= start {
        bail!("gate response did not include a valid JSON object");
    }
    Ok(&trimmed[start..=end])
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        path::{Path, PathBuf},
    };

    use crate::model::{
        ChatThinkingEffort, ConversationRole, MemoryKind, MemoryState, MemorySubtype, now_utc,
    };

    use super::*;

    fn sample_memory() -> MemoryRecord {
        let now = now_utc();
        MemoryRecord {
            id: Uuid::new_v4(),
            lineage_id: Uuid::new_v4(),
            kind: MemoryKind::Semantic,
            subtype: MemorySubtype::Preference,
            display_text: "You prefer Rust.".to_string(),
            retrieval_text: "preference rust".to_string(),
            attrs: serde_json::json!({}),
            observed_at: Some(now),
            valid_from: now,
            valid_to: None,
            confidence: 0.9,
            salience: 0.8,
            state: MemoryState::Accepted,
            embedding: None,
            source_artifact_ids: Vec::new(),
            thread_id: None,
            parent_id: None,
            path: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn synthetic_backend_uses_selected_memories() {
        let request = ChatCompletionRequest {
            message: "What should I build?".to_string(),
            model_id: None,
            recent_turns: Vec::new(),
            recent_context: None,
            injected_context: None,
            selected_memories: vec![sample_memory()],
            trace_id: Uuid::new_v4(),
        };

        let response = SyntheticChatBackend.complete(&request).await.unwrap();
        assert!(response.answer.contains("You prefer Rust."));
        assert_eq!(response.model_id, None);
    }

    #[test]
    fn system_prompt_includes_trace_and_context_sections() {
        let trace_id = Uuid::new_v4();
        let prompt = compose_system_prompt(
            DEFAULT_SYSTEM_PROMPT,
            Some("Relevant personal context:\n- You prefer Rust."),
            Some("The user asked about backend choices."),
            trace_id,
        );
        assert!(prompt.contains(&trace_id.to_string()));
        assert!(prompt.contains("Injected personal context"));
        assert!(prompt.contains("Recent conversation context"));
        assert!(prompt.contains("harness"));
        assert!(prompt.contains("memory bank"));
    }

    #[test]
    fn gate_prompt_mentions_relevance_role() {
        let request = ContextGateRequest {
            query: "What am I building?".to_string(),
            recent_turns: Vec::new(),
            recent_context: None,
            candidates: vec![ScoredMemory {
                memory: sample_memory(),
                semantic_score: 0.8,
                lexical_score: 0.2,
                fusion_score: 0.4,
                temporal_bonus: 0.0,
                thread_bonus: 0.0,
                salience_bonus: 0.0,
                confidence_bonus: 0.0,
                reinjection_penalty: 0.0,
                stale_penalty: 0.0,
                final_score: 0.4,
                prior_injected: false,
                candidate_rank: 0,
            }],
            max_injected: 3,
            model_id: None,
            trace_id: Uuid::new_v4(),
        };

        let prompt = build_gate_prompt(&request).unwrap();
        assert!(prompt.contains("relevant to the latest user query"));
        assert!(prompt.contains("\"selected_ids\""));
    }

    #[test]
    fn bedrock_messages_preserve_turn_roles() {
        let messages = build_bedrock_messages(
            &[ConversationTurn {
                role: ConversationRole::Assistant,
                text: "Previously discussed memory.".to_string(),
            }],
            "What next?",
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].role(),
            &aws_sdk_bedrockruntime::types::ConversationRole::Assistant
        );
    }

    #[test]
    fn custom_profile_files_use_explicit_paths_and_default_counterpart() {
        let settings = BedrockChatSettings {
            region: "us-west-2".to_string(),
            profile: Some("ancilla-dev".to_string()),
            config_file: Some(PathBuf::from("/tmp/project/.aws/config")),
            shared_credentials_file: None,
            bearer_token: None,
            default_model_id: "model".to_string(),
            models: vec![ChatModelOption {
                label: "Model".to_string(),
                model_id: "model".to_string(),
                description: None,
                thinking_mode: None,
                thinking_effort: None,
                thinking_budget_tokens: None,
            }],
            max_tokens: 800,
            temperature: 0.2,
        };

        let profile_files = build_profile_files(&settings).unwrap().unwrap();
        let debug = format!("{profile_files:?}");

        assert!(debug.contains("/tmp/project/.aws/config"));
        assert!(debug.contains("Default(Credentials)"));
    }

    #[test]
    fn expand_home_path_expands_tilde_prefix() {
        let home = env::var_os("HOME").expect("HOME should be set for test");
        let expanded = expand_home_path(Path::new("~/workspace/ancilla/.aws/config")).unwrap();
        assert_eq!(
            expanded,
            PathBuf::from(home).join("workspace/ancilla/.aws/config")
        );
    }

    #[test]
    fn adaptive_thinking_fields_only_set_type() {
        let fields = build_additional_model_request_fields(&ChatModelOption {
            label: "Claude Opus 4.6".to_string(),
            model_id: "anthropic.claude-opus-4-6-v1".to_string(),
            description: None,
            thinking_mode: Some(ChatThinkingMode::Adaptive),
            thinking_effort: Some(ChatThinkingEffort::High),
            thinking_budget_tokens: None,
        })
        .unwrap()
        .unwrap();

        let root = fields.as_object().unwrap();
        let thinking = root.get("thinking").and_then(Document::as_object).unwrap();
        assert_eq!(
            thinking.get("type").and_then(Document::as_string),
            Some("adaptive")
        );
        assert_eq!(thinking.get("effort"), None);
    }

    #[test]
    fn enabled_thinking_requires_budget_tokens() {
        let error = build_additional_model_request_fields(&ChatModelOption {
            label: "Claude Sonnet".to_string(),
            model_id: "anthropic.claude-sonnet".to_string(),
            description: None,
            thinking_mode: Some(ChatThinkingMode::Enabled),
            thinking_effort: None,
            thinking_budget_tokens: None,
        })
        .unwrap_err();
        assert!(error.to_string().contains("thinking_budget_tokens"));
    }
}
