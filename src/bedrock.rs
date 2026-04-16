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
    types::{
        AutoToolChoice, ContentBlock, ConversationRole, InferenceConfiguration, Message,
        SystemContentBlock, TokenUsage, Tool, ToolChoice, ToolConfiguration, ToolInputSchema,
        ToolResultBlock, ToolResultContentBlock, ToolSpecification,
    },
};
use aws_smithy_types::{Document, Number};
use aws_types::region::Region;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    memory_markdown::{MemoryDocument, parse_memory_list_json},
    model::{
        ChatModelOption, ChatModelsResponse, ChatThinkingMode, ConversationTurn, GateDecision,
        LlmCallMetrics, LlmCostBreakdown, LlmTokenUsage, MemoryRecord, ScoredMemory,
    },
    server_config::ServerConfig,
};

const DEFAULT_SYSTEM_PROMPT: &str = "You are Ancilla, a personal AI assistant. You are speaking with a specific user over a harness that may provide relevant memories about that user. Treat those retrieved memories as things you recall about the user when they are relevant, not as something you should quote as an external source. When relevant, answer the user naturally as if you remember: for example, \"I remember that...\", \"You told me...\", or simply answer directly without mentioning where the knowledge came from. Do not say things like \"based on the provided context\" or otherwise call attention to the retrieval mechanism unless the user explicitly asks how you know. Use retrieved memories when they are relevant, ignore them when they are not, and never invent personalized facts that were not provided. Answer directly, naturally, and helpfully.\n\nYou may have access to these tools:\n- search_memories: search the user's explicit memory bank when you need more user-specific context.\n- remember_current_conversation: store durable, important facts from the current conversation when they are important enough that a thoughtful friend would remember them later.\n\nUse search_memories when the question depends on the user's preferences, projects, identity, plans, or past details and the injected memories are insufficient. If the user explicitly asks you to search or check their memories, call search_memories rather than pretending to know. Use remember_current_conversation sparingly. It is completely fine to not store anything. If the user explicitly asks you to remember or save something durable about them, call remember_current_conversation. Do not store generic chit-chat, weak guesses, or low-value transient details.";
const DEFAULT_GATE_SYSTEM_PROMPT: &str = "You are Ancilla's memory gate. Your job is to decide whether candidate stored memories are actually relevant to the user's latest query and recent conversation context. Prefer the smallest useful subset, and prefer no memories when the candidates are weak, redundant, or off-topic. Only select memories that would materially help the assistant answer better. Return strict JSON only with keys decision, confidence, reason, and selected_ids.";
const MEMORY_CREATION_SYSTEM_PROMPT: &str = include_str!("../prompts/memory_creation.md");

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

#[derive(Clone, Debug, PartialEq)]
pub struct ChatCompletionResult {
    pub answer: String,
    pub model_id: Option<String>,
    pub metrics: Option<LlmCallMetrics>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatToolUseRequest {
    pub tool_use_id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatToolUseResult {
    pub tool_use_id: String,
    pub output: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryCreationRequest {
    pub context_text: String,
    pub model_id: Option<String>,
    pub trace_id: Uuid,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MemoryCreationResult {
    pub memories: Vec<MemoryDocument>,
    pub model_id: Option<String>,
    pub metrics: Option<LlmCallMetrics>,
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
    async fn extract_memories(
        &self,
        request: &MemoryCreationRequest,
    ) -> anyhow::Result<MemoryCreationResult>;
    fn models(&self) -> ChatModelsResponse;
    async fn complete_with_tools(
        &self,
        request: &ChatCompletionRequest,
        tools: &dyn ChatToolExecutor,
    ) -> anyhow::Result<ChatCompletionResult> {
        let _ = tools;
        self.complete(request).await
    }
}

#[async_trait]
pub trait ChatToolExecutor: Send + Sync {
    async fn search_memories(&self, query: String, limit: usize) -> anyhow::Result<Value>;
    async fn remember_current_conversation(&self, reason: Option<String>) -> anyhow::Result<Value>;
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
    pub metrics: Option<LlmCallMetrics>,
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

    async fn converse_with_optional_tools(
        &self,
        model: &ChatModelOption,
        system_prompt: String,
        messages: Vec<Message>,
        tool_config: Option<ToolConfiguration>,
        trace_id: Uuid,
    ) -> anyhow::Result<aws_sdk_bedrockruntime::operation::converse::ConverseOutput> {
        let mut converse = self
            .client
            .converse()
            .model_id(&model.model_id)
            .set_system(Some(vec![SystemContentBlock::Text(system_prompt)]))
            .set_messages(Some(messages))
            .inference_config(build_chat_inference_config(
                model,
                self.settings.max_tokens,
                self.settings.temperature,
            ))
            .set_request_metadata(Some(HashMap::from([(
                "trace_id".to_string(),
                trace_id.to_string(),
            )])))
            .set_tool_config(tool_config);
        if let Some(additional_fields) = build_additional_model_request_fields(model)? {
            converse = converse.additional_model_request_fields(additional_fields);
        }
        converse
            .send()
            .await
            .map_err(|error| bedrock_request_error("chat", &model.model_id, &error))
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
        let response = self
            .converse_with_optional_tools(model, system_prompt, messages, None, request.trace_id)
            .await?;

        Ok(ChatCompletionResult {
            answer: extract_text_response(&response)?,
            model_id: Some(model.model_id.clone()),
            metrics: usage_metrics_for_model(model, response.usage()),
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
            .inference_config(build_chat_inference_config(
                model,
                self.settings.max_tokens,
                self.settings.temperature,
            ))
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

    async fn extract_memories(
        &self,
        request: &MemoryCreationRequest,
    ) -> anyhow::Result<MemoryCreationResult> {
        let model = self.resolve_model(request.model_id.as_deref())?;
        let model_id = model.model_id.clone();
        let response = self
            .client
            .converse()
            .model_id(&model_id)
            .set_system(Some(vec![SystemContentBlock::Text(
                MEMORY_CREATION_SYSTEM_PROMPT.to_string(),
            )]))
            .set_messages(Some(vec![
                Message::builder()
                    .role(ConversationRole::User)
                    .content(ContentBlock::Text(request.context_text.clone()))
                    .build()
                    .expect("memory creation message build should not fail"),
            ]))
            .inference_config(
                InferenceConfiguration::builder()
                    .max_tokens(self.settings.max_tokens.max(1600))
                    .temperature(0.0)
                    .build(),
            )
            .set_request_metadata(Some(HashMap::from([(
                "trace_id".to_string(),
                request.trace_id.to_string(),
            )])))
            .send()
            .await
            .map_err(|error| bedrock_request_error("memory creation", &model_id, &error))?;

        let raw = extract_text_response(&response)?;
        let memories = parse_memory_list_json(&raw)?;
        Ok(MemoryCreationResult {
            memories,
            model_id: Some(model_id),
            metrics: usage_metrics_for_model(model, response.usage()),
        })
    }

    fn models(&self) -> ChatModelsResponse {
        self.catalog.clone()
    }

    async fn complete_with_tools(
        &self,
        request: &ChatCompletionRequest,
        tools: &dyn ChatToolExecutor,
    ) -> anyhow::Result<ChatCompletionResult> {
        let model = self.resolve_model(request.model_id.as_deref())?;
        let system_prompt = compose_system_prompt(
            DEFAULT_SYSTEM_PROMPT,
            request.injected_context.as_deref(),
            request.recent_context.as_deref(),
            request.trace_id,
        );
        let mut messages = build_bedrock_messages(&request.recent_turns, &request.message);
        let tool_config = build_chat_tool_config()?;
        let mut metrics = None;

        for _ in 0..6 {
            let response = self
                .converse_with_optional_tools(
                    model,
                    system_prompt.clone(),
                    messages.clone(),
                    Some(tool_config.clone()),
                    request.trace_id,
                )
                .await?;
            metrics =
                LlmCallMetrics::merged(metrics, usage_metrics_for_model(model, response.usage()));

            let Some(output) = response.output() else {
                bail!("bedrock converse response had no output")
            };
            let Ok(message) = output.as_message() else {
                bail!("bedrock converse response did not contain a message output")
            };
            messages.push(message.clone());

            if response.stop_reason().as_str() == "tool_use" {
                let tool_requests = parse_tool_use_requests(message)?;
                if tool_requests.is_empty() {
                    bail!("bedrock returned stop_reason=tool_use without any toolUse blocks");
                }
                let tool_results = execute_tool_requests(&tool_requests, tools).await;
                messages.push(build_tool_result_message(&tool_results)?);
                continue;
            }

            return Ok(ChatCompletionResult {
                answer: extract_text_response(&response)?,
                model_id: Some(model.model_id.clone()),
                metrics,
            });
        }

        bail!("bedrock chat exceeded tool iteration limit")
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
        let mut result = parse_gate_response(&raw, request, &model_id)?;
        result.metrics = usage_metrics_for_model_id(&model_id, None, response.usage());
        Ok(result)
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
            metrics: None,
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

    async fn extract_memories(
        &self,
        request: &MemoryCreationRequest,
    ) -> anyhow::Result<MemoryCreationResult> {
        let text = request.context_text.trim();
        let memories = if text.is_empty() {
            Vec::new()
        } else {
            vec![MemoryDocument {
                title: crate::memory_markdown::infer_title(text),
                tags: Vec::new(),
                body_markdown: text.to_string(),
            }]
        };
        Ok(MemoryCreationResult {
            memories,
            model_id: None,
            metrics: None,
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
            .map(MemoryRecord::context_line)
            .collect::<Vec<_>>()
            .join(" ");
        format!("Respond to: {message}. Use these memories: {facts}")
    }
}

fn build_chat_tool_config() -> anyhow::Result<ToolConfiguration> {
    let search_schema = json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Focused search query for the user's explicit memory bank."
            },
            "limit": {
                "type": "integer",
                "description": "Maximum number of memories to return.",
                "minimum": 1,
                "maximum": 10
            }
        },
        "required": ["query"]
    });
    let remember_schema = json!({
        "type": "object",
        "properties": {
            "reason": {
                "type": "string",
                "description": "Short reason why the current conversation is worth remembering."
            }
        }
    });

    Ok(ToolConfiguration::builder()
        .tools(Tool::ToolSpec(
            ToolSpecification::builder()
                .name("search_memories")
                .description(
                    "Search the user's explicit memory bank for relevant durable facts, preferences, projects, and plans.",
                )
                .input_schema(ToolInputSchema::Json(json_to_document(search_schema)?))
                .build()
                .expect("search_memories tool specification should build"),
        ))
        .tools(Tool::ToolSpec(
            ToolSpecification::builder()
                .name("remember_current_conversation")
                .description(
                    "Store durable, important facts from the current conversation in the user's memory bank. Use sparingly.",
                )
                .input_schema(ToolInputSchema::Json(json_to_document(remember_schema)?))
                .build()
                .expect("remember_current_conversation tool specification should build"),
        ))
        .tool_choice(ToolChoice::Auto(AutoToolChoice::builder().build()))
        .build()
        .expect("chat tool configuration should build"))
}

fn parse_tool_use_requests(message: &Message) -> anyhow::Result<Vec<ChatToolUseRequest>> {
    let mut requests = Vec::new();
    for block in &message.content {
        if let ContentBlock::ToolUse(tool_use) = block {
            requests.push(ChatToolUseRequest {
                tool_use_id: tool_use.tool_use_id().to_string(),
                name: tool_use.name().to_string(),
                input: document_to_json(tool_use.input())?,
            });
        }
    }
    Ok(requests)
}

async fn execute_tool_requests(
    tool_requests: &[ChatToolUseRequest],
    tools: &dyn ChatToolExecutor,
) -> Vec<ChatToolUseResult> {
    let mut results = Vec::new();
    for request in tool_requests {
        let output = match request.name.as_str() {
            "search_memories" => {
                let query = request
                    .input
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let limit = request
                    .input
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|value| value.clamp(1, 10) as usize)
                    .unwrap_or(5);
                match tools.search_memories(query, limit).await {
                    Ok(value) => json!({ "ok": true, "result": value }),
                    Err(error) => json!({ "ok": false, "error": error.to_string() }),
                }
            }
            "remember_current_conversation" => {
                let reason = request
                    .input
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned);
                match tools.remember_current_conversation(reason).await {
                    Ok(value) => json!({ "ok": true, "result": value }),
                    Err(error) => json!({ "ok": false, "error": error.to_string() }),
                }
            }
            other => json!({
                "ok": false,
                "error": format!("unknown tool `{other}`"),
            }),
        };

        results.push(ChatToolUseResult {
            tool_use_id: request.tool_use_id.clone(),
            output,
        });
    }
    results
}

fn build_tool_result_message(results: &[ChatToolUseResult]) -> anyhow::Result<Message> {
    let mut builder = Message::builder().role(ConversationRole::User);
    for result in results {
        let tool_result = ToolResultBlock::builder()
            .tool_use_id(result.tool_use_id.clone())
            .content(ToolResultContentBlock::Json(json_to_document(
                result.output.clone(),
            )?))
            .build()
            .expect("tool result block should build");
        builder = builder.content(ContentBlock::ToolResult(tool_result));
    }
    builder
        .build()
        .with_context(|| "failed to build Bedrock toolResult message")
}

fn json_to_document(value: Value) -> anyhow::Result<Document> {
    Ok(match value {
        Value::Null => Document::Null,
        Value::Bool(boolean) => Document::Bool(boolean),
        Value::String(string) => Document::String(string),
        Value::Array(items) => Document::Array(
            items
                .into_iter()
                .map(json_to_document)
                .collect::<anyhow::Result<Vec<_>>>()?,
        ),
        Value::Object(map) => Document::Object(
            map.into_iter()
                .map(|(key, value)| Ok((key, json_to_document(value)?)))
                .collect::<anyhow::Result<HashMap<_, _>>>()?,
        ),
        Value::Number(number) => {
            if let Some(value) = number.as_u64() {
                Document::Number(Number::PosInt(value))
            } else if let Some(value) = number.as_i64() {
                if value >= 0 {
                    Document::Number(Number::PosInt(value as u64))
                } else {
                    Document::Number(Number::NegInt(value))
                }
            } else if let Some(value) = number.as_f64() {
                Document::Number(Number::Float(value))
            } else {
                bail!("unsupported JSON number for Bedrock tool document")
            }
        }
    })
}

fn document_to_json(document: &Document) -> anyhow::Result<Value> {
    Ok(match document {
        Document::Null => Value::Null,
        Document::Bool(boolean) => Value::Bool(*boolean),
        Document::String(string) => Value::String(string.clone()),
        Document::Array(items) => Value::Array(
            items
                .iter()
                .map(document_to_json)
                .collect::<anyhow::Result<Vec<_>>>()?,
        ),
        Document::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| Ok((key.clone(), document_to_json(value)?)))
                .collect::<anyhow::Result<serde_json::Map<_, _>>>()?,
        ),
        Document::Number(number) => match *number {
            Number::PosInt(value) => Value::Number(serde_json::Number::from(value)),
            Number::NegInt(value) => Value::Number(serde_json::Number::from(value)),
            Number::Float(value) => Value::Number(
                serde_json::Number::from_f64(value)
                    .with_context(|| "non-finite float in Bedrock tool document")?,
            ),
        },
    })
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
        prompt.push_str("\n\nMemories you recall about the user:\n");
        prompt.push_str(injected_context.trim());
    } else {
        prompt.push_str(
            "\n\nMemories you recall about the user:\nNone. Do not claim personalized memory you were not given.",
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
                "title": candidate.memory.title,
                "tags": candidate.memory.tags,
                "content_markdown_preview": truncate_for_gate(&candidate.memory.content_markdown, 280),
                "search_text_preview": truncate_for_gate(&candidate.memory.search_text, 280),
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

fn build_chat_inference_config(
    model: &ChatModelOption,
    max_tokens: i32,
    configured_temperature: f32,
) -> InferenceConfiguration {
    // Anthropic thinking mode on Bedrock currently requires temperature=1.
    let temperature = if model.thinking_mode.is_some() {
        1.0
    } else {
        configured_temperature
    };

    InferenceConfiguration::builder()
        .max_tokens(max_tokens)
        .temperature(temperature)
        .build()
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

fn usage_metrics_for_model(
    model: &ChatModelOption,
    usage: Option<&TokenUsage>,
) -> Option<LlmCallMetrics> {
    usage_metrics_for_model_id(&model.model_id, model.pricing, usage)
}

fn usage_metrics_for_model_id(
    model_id: &str,
    pricing: Option<crate::model::ChatModelPricing>,
    usage: Option<&TokenUsage>,
) -> Option<LlmCallMetrics> {
    let usage = usage?;
    let usage = LlmTokenUsage {
        input_tokens: usage.input_tokens().max(0) as u32,
        output_tokens: usage.output_tokens().max(0) as u32,
        total_tokens: usage.total_tokens().max(0) as u32,
        cache_read_input_tokens: usage
            .cache_read_input_tokens()
            .map(|value| value.max(0) as u32),
        cache_write_input_tokens: usage
            .cache_write_input_tokens()
            .map(|value| value.max(0) as u32),
    };
    let pricing = pricing.or_else(|| {
        let canonical = crate::server_config::canonicalize_chat_model_id(model_id);
        crate::server_config::pricing_for_chat_model_id(&canonical)
    })?;

    let input_usd =
        (usage.input_tokens as f64 / 1_000_000.0) * pricing.input_usd_per_million_tokens;
    let output_usd =
        (usage.output_tokens as f64 / 1_000_000.0) * pricing.output_usd_per_million_tokens;
    let cache_read_input_usd = usage.cache_read_input_tokens.map(|tokens| {
        (tokens as f64 / 1_000_000.0)
            * pricing
                .cache_read_input_usd_per_million_tokens
                .unwrap_or_default()
    });
    let total_usd = input_usd + output_usd + cache_read_input_usd.unwrap_or_default();

    Some(LlmCallMetrics {
        model_id: Some(model_id.to_string()),
        usage,
        cost: LlmCostBreakdown {
            input_usd,
            output_usd,
            cache_read_input_usd,
            cache_write_input_usd: None,
            total_usd,
        },
    })
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
        metrics: None,
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

    use crate::{
        memory_markdown::markdown_from_plain_text,
        model::{ChatThinkingEffort, ConversationRole, MemoryKind, MemoryState, now_utc},
    };

    use super::*;

    fn sample_memory() -> MemoryRecord {
        let now = now_utc();
        MemoryRecord {
            id: Uuid::new_v4(),
            lineage_id: Uuid::new_v4(),
            kind: MemoryKind::Semantic,
            title: "You prefer Rust.".to_string(),
            tags: vec!["preference".to_string()],
            content_markdown: markdown_from_plain_text(
                "You prefer Rust.",
                &["preference".to_string()],
            ),
            search_text: "You prefer Rust.\npreference".to_string(),
            attrs: serde_json::json!({}),
            observed_at: Some(now),
            valid_from: now,
            valid_to: None,
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
        assert!(prompt.contains("Memories you recall about the user"));
        assert!(prompt.contains("Recent conversation context"));
        assert!(prompt.contains("I remember that"));
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
            region: "us-east-1".to_string(),
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
                pricing: None,
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
            pricing: None,
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
            pricing: None,
        })
        .unwrap_err();
        assert!(error.to_string().contains("thinking_budget_tokens"));
    }

    #[test]
    fn thinking_models_force_temperature_to_one() {
        let config = build_chat_inference_config(
            &ChatModelOption {
                label: "Claude Sonnet 4.6".to_string(),
                model_id: "us.anthropic.claude-sonnet-4-6".to_string(),
                description: None,
                thinking_mode: Some(ChatThinkingMode::Adaptive),
                thinking_effort: None,
                thinking_budget_tokens: None,
                pricing: None,
            },
            800,
            0.2,
        );

        assert_eq!(config.temperature(), Some(1.0));
    }

    #[test]
    fn non_thinking_models_keep_configured_temperature() {
        let config = build_chat_inference_config(
            &ChatModelOption {
                label: "Kimi K2.5".to_string(),
                model_id: "moonshotai.kimi-k2.5".to_string(),
                description: None,
                thinking_mode: None,
                thinking_effort: None,
                thinking_budget_tokens: None,
                pricing: None,
            },
            800,
            0.2,
        );

        assert_eq!(config.temperature(), Some(0.2));
    }
}
