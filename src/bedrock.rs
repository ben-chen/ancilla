use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, bail};
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::{
    Client,
    types::{ContentBlock, ConversationRole, InferenceConfiguration, Message, SystemContentBlock},
};
use aws_types::region::Region;
use uuid::Uuid;

use crate::{
    config::AppConfig,
    model::{ConversationTurn, MemoryRecord},
};

const DEFAULT_SYSTEM_PROMPT: &str = "You are Ancilla, a personal memory assistant. Use injected personal context when it is present, do not invent private facts, and answer directly.";

#[derive(Clone, Debug, PartialEq)]
pub struct ChatCompletionRequest {
    pub message: String,
    pub recent_turns: Vec<ConversationTurn>,
    pub recent_context: Option<String>,
    pub injected_context: Option<String>,
    pub selected_memories: Vec<MemoryRecord>,
    pub trace_id: Uuid,
}

#[async_trait]
pub trait ChatCompletionBackend: Send + Sync {
    async fn complete(&self, request: &ChatCompletionRequest) -> anyhow::Result<String>;
}

pub async fn build_chat_backend(
    config: &AppConfig,
) -> anyhow::Result<Arc<dyn ChatCompletionBackend>> {
    if let Some(model_id) = config.bedrock_chat_model_id.clone() {
        let settings = BedrockChatSettings {
            region: config.aws_region.clone(),
            profile: config.aws_profile.clone(),
            model_id,
            max_tokens: config.bedrock_chat_max_tokens,
            temperature: config.bedrock_chat_temperature,
        };
        Ok(Arc::new(BedrockChatBackend::new(settings).await?))
    } else {
        Ok(Arc::new(SyntheticChatBackend))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BedrockChatSettings {
    pub region: String,
    pub profile: Option<String>,
    pub model_id: String,
    pub max_tokens: i32,
    pub temperature: f32,
}

#[derive(Clone, Debug)]
pub struct BedrockChatBackend {
    client: Client,
    settings: BedrockChatSettings,
}

impl BedrockChatBackend {
    pub async fn new(settings: BedrockChatSettings) -> anyhow::Result<Self> {
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(settings.region.clone()));
        if let Some(profile) = settings.profile.clone() {
            loader = loader.profile_name(profile);
        }

        let sdk_config = loader.load().await;
        let client = Client::new(&sdk_config);
        Ok(Self { client, settings })
    }
}

#[async_trait]
impl ChatCompletionBackend for BedrockChatBackend {
    async fn complete(&self, request: &ChatCompletionRequest) -> anyhow::Result<String> {
        let system_prompt = compose_system_prompt(
            DEFAULT_SYSTEM_PROMPT,
            request.injected_context.as_deref(),
            request.recent_context.as_deref(),
            request.trace_id,
        );
        let messages = build_bedrock_messages(&request.recent_turns, &request.message);

        let response = self
            .client
            .converse()
            .model_id(&self.settings.model_id)
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
            )])))
            .send()
            .await
            .with_context(|| "bedrock converse request failed")?;

        extract_text_response(&response)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SyntheticChatBackend;

#[async_trait]
impl ChatCompletionBackend for SyntheticChatBackend {
    async fn complete(&self, request: &ChatCompletionRequest) -> anyhow::Result<String> {
        Ok(synthesize_answer(
            &request.message,
            &request.selected_memories,
        ))
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
        bail!("bedrock converse response had no text content")
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{ConversationRole, MemoryKind, MemoryState, MemorySubtype, now_utc};

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
            recent_turns: Vec::new(),
            recent_context: None,
            injected_context: None,
            selected_memories: vec![sample_memory()],
            trace_id: Uuid::new_v4(),
        };

        let answer = SyntheticChatBackend.complete(&request).await.unwrap();
        assert!(answer.contains("You prefer Rust."));
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
}
