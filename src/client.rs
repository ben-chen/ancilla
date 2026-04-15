use std::{
    io,
    time::{Duration, Instant},
};

use crate::{
    client_config::{ClientConfig, normalize_base_url},
    model::{
        ApiErrorBody, AssembleContextRequest, AssembleContextResponse, CaptureEntryResponse,
        ChatModelOption, ChatModelsResponse, ChatRespondRequest, ChatResponse, ConversationRole,
        ConversationTurn, CreateMemoryRequest, Entry, EntryKind, GateDecision, MemoryKind,
        MemoryRecord, MemorySubtype, empty_object,
    },
};
use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap},
};
use uuid::Uuid;

const COLOR_BG: Color = Color::Rgb(12, 16, 24);
const COLOR_PANEL: Color = Color::Rgb(20, 26, 38);
const COLOR_PANEL_ALT: Color = Color::Rgb(25, 33, 47);
const COLOR_BORDER: Color = Color::Rgb(58, 74, 102);
const COLOR_TEXT: Color = Color::Rgb(235, 239, 245);
const COLOR_MUTED: Color = Color::Rgb(151, 161, 179);
const COLOR_ACCENT: Color = Color::Rgb(96, 192, 255);
const COLOR_ACCENT_WARM: Color = Color::Rgb(255, 182, 94);
const COLOR_SUCCESS: Color = Color::Rgb(125, 208, 138);
const COLOR_ERROR: Color = Color::Rgb(255, 115, 115);

pub async fn run(base_url_override: Option<String>, config: &ClientConfig) -> anyhow::Result<()> {
    let base_url = resolve_base_url(base_url_override, config)?;
    let api = RemoteApi::new(base_url.clone(), config)?;
    let mut app = ClientApp::new(base_url);

    app.refresh_remote_state(&api).await?;

    let mut terminal = TerminalSession::enter()?;
    loop {
        terminal.draw(|frame| draw(frame, &mut app))?;

        if !event::poll(Duration::from_millis(125))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match app.handle_key(key) {
            ClientAction::None => {}
            ClientAction::Quit => break,
            ClientAction::Refresh => {
                app.set_info(format!("Refreshing timeline from {}", api.base_url));
                if let Err(error) = app.refresh_remote_state(&api).await {
                    app.set_request_error("Refresh failed.", "Refresh Error", error.to_string());
                }
            }
            ClientAction::SubmitAsk { message, model_id } => {
                app.set_info("Sending question to remote service...");
                let model_label = app
                    .selected_model_label()
                    .unwrap_or("server default")
                    .to_string();
                match api
                    .ask(
                        &message,
                        model_id.as_deref(),
                        app.conversation_id,
                        &app.recent_turns,
                    )
                    .await
                {
                    Ok(response) => {
                        app.set_chat_response(message, response);
                        app.set_success("Answer received.");
                    }
                    Err(error) => app.set_request_error(
                        format!("Chat request failed for {model_label}."),
                        format!("Chat Error [{model_label}]"),
                        format!("Q: {message}\n\n{error}"),
                    ),
                }
            }
            ClientAction::SubmitAssemble(message) => {
                app.set_info("Assembling retrieval context on remote service...");
                let model_label = app
                    .selected_model_label()
                    .unwrap_or("server default")
                    .to_string();
                match api
                    .assemble_context(&message, None, app.conversation_id, &app.recent_turns)
                    .await
                {
                    Ok(response) => {
                        app.set_context_preview(message, response);
                        app.set_success("Context preview received.");
                    }
                    Err(error) => app.set_request_error(
                        format!("Context request failed for {model_label}."),
                        format!("Context Error [{model_label}]"),
                        format!("Q: {message}\n\n{error}"),
                    ),
                }
            }
            ClientAction::SubmitCapture(text) => {
                app.set_info("Capturing memory on remote service...");
                match api.capture_text(&text).await {
                    Ok(response) => {
                        app.set_success(format!(
                            "Captured memory entry {} with {} memories.",
                            response.entry.id,
                            response.memories.len()
                        ));
                        if let Err(error) = app.refresh_timeline(&api).await {
                            app.set_request_error(
                                "Capture succeeded but refresh failed.",
                                "Refresh Error",
                                error.to_string(),
                            );
                        }
                    }
                    Err(error) => app.set_request_error(
                        "Capture failed.",
                        "Capture Error",
                        format!("Memory: {text}\n\n{error}"),
                    ),
                }
            }
        }
    }

    Ok(())
}

fn resolve_base_url(
    base_url_override: Option<String>,
    config: &ClientConfig,
) -> anyhow::Result<String> {
    match base_url_override {
        Some(base_url) => normalize_base_url(&base_url),
        None => Ok(config.base_url.clone()),
    }
}

struct RemoteApi {
    base_url: String,
    http: reqwest::Client,
}

impl RemoteApi {
    fn new(base_url: String, config: &ClientConfig) -> anyhow::Result<Self> {
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(20));

        if let (Some(username), Some(password)) = (
            config.basic_auth_username.as_deref(),
            config.basic_auth_password.as_deref(),
        ) {
            let token = BASE64.encode(format!("{username}:{password}"));
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Basic {token}"))
                    .context("invalid basic auth header value")?,
            );
            builder = builder.default_headers(headers);
        }

        let http = builder.build().context("failed to build HTTP client")?;
        Ok(Self { base_url, http })
    }

    async fn get_timeline(&self) -> anyhow::Result<Vec<Entry>> {
        self.get_json("/v1/timeline").await
    }

    async fn get_chat_models(&self) -> anyhow::Result<Option<ChatModelsResponse>> {
        let response = self
            .http
            .get(self.url("/v1/chat/models"))
            .send()
            .await
            .with_context(|| "request failed for GET /v1/chat/models")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(parse_json_response(response).await?))
    }

    async fn capture_text(&self, raw_text: &str) -> anyhow::Result<CaptureEntryResponse> {
        self.post_json(
            "/v1/memories",
            &CreateMemoryRequest {
                display_text: raw_text.to_string(),
                retrieval_text: None,
                kind: MemoryKind::Semantic,
                subtype: MemorySubtype::Fact,
                captured_at: None,
                timezone: Some("UTC".to_string()),
                source_app: Some("ratatui-client".to_string()),
                attrs: empty_object(),
                observed_at: None,
                valid_from: None,
                valid_to: None,
                confidence: None,
                salience: None,
                thread_title: None,
                metadata: empty_object(),
            },
        )
        .await
    }

    async fn ask(
        &self,
        message: &str,
        model_id: Option<&str>,
        conversation_id: Uuid,
        recent_turns: &[ConversationTurn],
    ) -> anyhow::Result<ChatResponse> {
        self.post_json(
            "/v1/chat/respond",
            &ChatRespondRequest {
                message: message.to_string(),
                model_id: model_id.map(ToOwned::to_owned),
                gate_model_id: None,
                recent_turns: recent_turns.to_vec(),
                recent_context: None,
                conversation_id: Some(conversation_id),
                active_thread_id: None,
                focus_from: None,
                focus_to: None,
                query_embedding: None,
            },
        )
        .await
    }

    async fn assemble_context(
        &self,
        message: &str,
        gate_model_id: Option<&str>,
        conversation_id: Uuid,
        recent_turns: &[ConversationTurn],
    ) -> anyhow::Result<AssembleContextResponse> {
        self.post_json(
            "/v1/context/assemble",
            &AssembleContextRequest {
                query: message.to_string(),
                recent_turns: recent_turns.to_vec(),
                recent_context: None,
                gate_model_id: gate_model_id.map(ToOwned::to_owned),
                conversation_id: Some(conversation_id),
                active_thread_id: None,
                focus_from: None,
                focus_to: None,
                query_embedding: None,
                max_candidates: Some(20),
                max_injected: Some(5),
            },
        )
        .await
    }

    async fn get_json<T>(&self, path: &str) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = self
            .http
            .get(self.url(path))
            .send()
            .await
            .with_context(|| format!("request failed for GET {path}"))?;
        parse_json_response(response).await
    }

    async fn post_json<T, B>(&self, path: &str, body: &B) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
        B: serde::Serialize,
    {
        let response = self
            .http
            .post(self.url(path))
            .json(body)
            .send()
            .await
            .with_context(|| format!("request failed for POST {path}"))?;
        parse_json_response(response).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

async fn parse_json_response<T>(response: reqwest::Response) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<ApiErrorBody>(&body)
            .map(|parsed| parsed.error)
            .unwrap_or_else(|_| {
                if body.trim().is_empty() {
                    format!("request failed with status {status}")
                } else {
                    body
                }
            });
        bail!("{status}: {message}")
    }

    response
        .json::<T>()
        .await
        .with_context(|| format!("failed to decode JSON response with status {status}"))
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalSession {
    fn enter() -> anyhow::Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).context("failed to create terminal")?;
        terminal.hide_cursor().context("failed to hide cursor")?;
        Ok(Self { terminal })
    }

    fn draw<F>(&mut self, render: F) -> anyhow::Result<()>
    where
        F: FnOnce(&mut ratatui::Frame<'_>),
    {
        self.terminal
            .draw(render)
            .context("failed to draw terminal")?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputMode {
    Normal,
    Ask,
    ContextPreview,
    Capture,
    ModelPicker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusKind {
    Info,
    Success,
    Error,
}

struct StatusLine {
    kind: StatusKind,
    message: String,
    updated_at: Instant,
}

struct ClientApp {
    base_url: String,
    conversation_id: Uuid,
    recent_turns: Vec<ConversationTurn>,
    mode: InputMode,
    timeline: Vec<Entry>,
    timeline_state: ListState,
    input: String,
    response_title: String,
    response_body: String,
    status: StatusLine,
    chat_backend: String,
    chat_models: Vec<ChatModelOption>,
    model_state: ListState,
    selected_model_id: Option<String>,
}

impl ClientApp {
    fn new(base_url: String) -> Self {
        let mut timeline_state = ListState::default();
        timeline_state.select(Some(0));
        Self {
            base_url,
            conversation_id: Uuid::new_v4(),
            recent_turns: Vec::new(),
            mode: InputMode::Normal,
            timeline: Vec::new(),
            timeline_state,
            input: String::new(),
            response_title: "Response".to_string(),
            response_body:
                "Press 's' to preview retrieval context, 'a' to ask the live service, or 'c' to capture a new memory."
                    .to_string(),
            status: StatusLine {
                kind: StatusKind::Info,
                message: "Ready.".to_string(),
                updated_at: Instant::now(),
            },
            chat_backend: "unknown".to_string(),
            chat_models: Vec::new(),
            model_state: ListState::default(),
            selected_model_id: None,
        }
    }

    async fn refresh_remote_state(&mut self, api: &RemoteApi) -> anyhow::Result<()> {
        self.refresh_models(api).await?;
        self.refresh_timeline(api).await
    }

    async fn refresh_timeline(&mut self, api: &RemoteApi) -> anyhow::Result<()> {
        let timeline = api.get_timeline().await?;
        self.timeline = timeline;
        let next_selected = match self.timeline_state.selected() {
            Some(index) if !self.timeline.is_empty() => Some(index.min(self.timeline.len() - 1)),
            _ if self.timeline.is_empty() => None,
            _ => Some(0),
        };
        self.timeline_state.select(next_selected);
        self.set_success(format!(
            "Loaded {} entries from {}.",
            self.timeline.len(),
            api.base_url
        ));
        Ok(())
    }

    async fn refresh_models(&mut self, api: &RemoteApi) -> anyhow::Result<()> {
        if let Some(response) = api.get_chat_models().await? {
            self.apply_chat_models(response);
        } else {
            self.chat_backend = "legacy".to_string();
            self.chat_models.clear();
            self.model_state.select(None);
            self.selected_model_id = None;
        }
        Ok(())
    }

    fn apply_chat_models(&mut self, response: ChatModelsResponse) {
        let current = self.selected_model_id.clone();
        self.chat_backend = response.backend;
        self.chat_models = response.models;
        self.selected_model_id = current
            .filter(|model_id| {
                self.chat_models
                    .iter()
                    .any(|model| &model.model_id == model_id)
            })
            .or(response.default_model_id)
            .or_else(|| self.chat_models.first().map(|model| model.model_id.clone()));
        self.model_state.select(self.selected_model_index());
    }

    fn handle_key(&mut self, key: KeyEvent) -> ClientAction {
        match self.mode {
            InputMode::Normal => self.handle_normal_key(key),
            InputMode::Ask => self.handle_input_key(key, InputMode::Ask),
            InputMode::ContextPreview => self.handle_input_key(key, InputMode::ContextPreview),
            InputMode::Capture => self.handle_input_key(key, InputMode::Capture),
            InputMode::ModelPicker => self.handle_model_picker_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> ClientAction {
        match key.code {
            KeyCode::Char('q') => ClientAction::Quit,
            KeyCode::Char('r') => ClientAction::Refresh,
            KeyCode::Char('a') => {
                self.mode = InputMode::Ask;
                self.input.clear();
                self.set_info("Ask mode. Type a question and press Enter.");
                ClientAction::None
            }
            KeyCode::Char('s') => {
                self.mode = InputMode::ContextPreview;
                self.input.clear();
                self.set_info("Context preview mode. Type a message and press Enter.");
                ClientAction::None
            }
            KeyCode::Char('c') => {
                self.mode = InputMode::Capture;
                self.input.clear();
                self.set_info("Capture mode. Type a memory and press Enter.");
                ClientAction::None
            }
            KeyCode::Char('m') => {
                if self.chat_models.is_empty() {
                    self.set_error("No remote model catalog is available on this server.");
                } else {
                    self.mode = InputMode::ModelPicker;
                    self.model_state
                        .select(self.selected_model_index().or(Some(0)));
                    self.set_info("Model picker. Use j/k and Enter.");
                }
                ClientAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                ClientAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_previous();
                ClientAction::None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.select_first();
                ClientAction::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.select_last();
                ClientAction::None
            }
            _ => ClientAction::None,
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent, submit_mode: InputMode) -> ClientAction {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.input.clear();
                self.set_info("Back to navigation mode.");
                ClientAction::None
            }
            KeyCode::Enter => {
                let input = self.input.trim().to_string();
                if input.is_empty() {
                    self.set_error("Input cannot be empty.");
                    ClientAction::None
                } else {
                    self.mode = InputMode::Normal;
                    self.input.clear();
                    match submit_mode {
                        InputMode::Ask => ClientAction::SubmitAsk {
                            message: input,
                            model_id: self.selected_model_id.clone(),
                        },
                        InputMode::ContextPreview => ClientAction::SubmitAssemble(input),
                        InputMode::Capture => ClientAction::SubmitCapture(input),
                        InputMode::Normal | InputMode::ModelPicker => ClientAction::None,
                    }
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                ClientAction::None
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.input.push(character);
                ClientAction::None
            }
            _ => ClientAction::None,
        }
    }

    fn handle_model_picker_key(&mut self, key: KeyEvent) -> ClientAction {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.set_info("Back to navigation mode.");
                ClientAction::None
            }
            KeyCode::Enter => {
                if let Some(index) = self.model_state.selected()
                    && let Some(model) = self.chat_models.get(index)
                {
                    self.selected_model_id = Some(model.model_id.clone());
                    self.mode = InputMode::Normal;
                    self.set_success(format!("Selected {}.", model.label));
                }
                ClientAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next_model();
                ClientAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_previous_model();
                ClientAction::None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                if !self.chat_models.is_empty() {
                    self.model_state.select(Some(0));
                }
                ClientAction::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                if !self.chat_models.is_empty() {
                    self.model_state.select(Some(self.chat_models.len() - 1));
                }
                ClientAction::None
            }
            _ => ClientAction::None,
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.timeline_state
            .selected()
            .and_then(|index| self.timeline.get(index))
    }

    fn set_chat_response(&mut self, prompt: String, response: ChatResponse) {
        self.recent_turns.push(ConversationTurn {
            role: ConversationRole::User,
            text: prompt.clone(),
        });
        self.recent_turns.push(ConversationTurn {
            role: ConversationRole::Assistant,
            text: response.answer.clone(),
        });
        if self.recent_turns.len() > 12 {
            let keep_from = self.recent_turns.len() - 12;
            self.recent_turns.drain(0..keep_from);
        }
        let model_label = response
            .model_id
            .as_deref()
            .and_then(|model_id| self.model_label(model_id))
            .map(ToOwned::to_owned)
            .or_else(|| response.model_id.clone())
            .unwrap_or_else(|| self.chat_backend.clone());
        self.response_title = format!(
            "Response [{} | {} memories | trace {}]",
            model_label,
            response.selected_memories.len(),
            response.trace_id
        );
        self.response_body = format!("Q: {prompt}\n\n{}", response.answer);
    }

    fn set_context_preview(&mut self, prompt: String, response: AssembleContextResponse) {
        self.response_title = format!(
            "Context [{} | {} selected | {} candidates | trace {}]",
            gate_decision_label(response.decision),
            response.selected_memories.len(),
            response.candidates.len(),
            response.trace_id
        );

        let mut lines = vec![
            format!("Q: {prompt}"),
            String::new(),
            format!(
                "Decision: {} ({:.2})",
                gate_decision_label(response.decision),
                response.gate_confidence
            ),
            format!("Reason: {}", response.gate_reason),
            String::new(),
            "Context".to_string(),
            response
                .context
                .clone()
                .unwrap_or_else(|| "(no context injected)".to_string()),
            String::new(),
            "Selected memories".to_string(),
        ];

        if response.selected_memories.is_empty() {
            lines.push("(none)".to_string());
        } else {
            for (index, memory) in response.selected_memories.iter().enumerate() {
                lines.push(format!("{}. {}", index + 1, format_memory(memory)));
            }
        }

        lines.push(String::new());
        lines.push("Top candidates".to_string());
        if response.candidates.is_empty() {
            lines.push("(none)".to_string());
        } else {
            for (index, candidate) in response.candidates.iter().take(6).enumerate() {
                lines.push(format!(
                    "{}. {:.3} final | {:.3} sem | {:.3} lex | {}",
                    index + 1,
                    candidate.final_score,
                    candidate.semantic_score,
                    candidate.lexical_score,
                    format_memory(&candidate.memory)
                ));
            }
        }

        self.response_body = lines.join("\n");
    }

    fn set_info(&mut self, message: impl Into<String>) {
        self.status = StatusLine {
            kind: StatusKind::Info,
            message: message.into(),
            updated_at: Instant::now(),
        };
    }

    fn set_success(&mut self, message: impl Into<String>) {
        self.status = StatusLine {
            kind: StatusKind::Success,
            message: message.into(),
            updated_at: Instant::now(),
        };
    }

    fn set_error(&mut self, message: impl Into<String>) {
        self.status = StatusLine {
            kind: StatusKind::Error,
            message: message.into(),
            updated_at: Instant::now(),
        };
    }

    fn set_request_error(
        &mut self,
        status_message: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) {
        self.response_title = title.into();
        self.response_body = body.into();
        self.set_error(status_message);
    }

    fn selected_model_index(&self) -> Option<usize> {
        self.selected_model_id.as_deref().and_then(|selected| {
            self.chat_models
                .iter()
                .position(|model| model.model_id == selected)
        })
    }

    fn selected_model_label(&self) -> Option<&str> {
        self.selected_model_id
            .as_deref()
            .and_then(|selected| self.model_label(selected))
    }

    fn model_label(&self, model_id: &str) -> Option<&str> {
        self.chat_models
            .iter()
            .find(|model| model.model_id == model_id)
            .map(|model| model.label.as_str())
    }

    fn select_next(&mut self) {
        if self.timeline.is_empty() {
            self.timeline_state.select(None);
            return;
        }
        let current = self.timeline_state.selected().unwrap_or(0);
        let next = (current + 1).min(self.timeline.len() - 1);
        self.timeline_state.select(Some(next));
    }

    fn select_previous(&mut self) {
        if self.timeline.is_empty() {
            self.timeline_state.select(None);
            return;
        }
        let current = self.timeline_state.selected().unwrap_or(0);
        let next = current.saturating_sub(1);
        self.timeline_state.select(Some(next));
    }

    fn select_first(&mut self) {
        if self.timeline.is_empty() {
            self.timeline_state.select(None);
        } else {
            self.timeline_state.select(Some(0));
        }
    }

    fn select_last(&mut self) {
        if self.timeline.is_empty() {
            self.timeline_state.select(None);
        } else {
            self.timeline_state.select(Some(self.timeline.len() - 1));
        }
    }

    fn select_next_model(&mut self) {
        if self.chat_models.is_empty() {
            self.model_state.select(None);
            return;
        }
        let next = match self.model_state.selected() {
            Some(index) if index + 1 < self.chat_models.len() => index + 1,
            _ => 0,
        };
        self.model_state.select(Some(next));
    }

    fn select_previous_model(&mut self) {
        if self.chat_models.is_empty() {
            self.model_state.select(None);
            return;
        }
        let previous = match self.model_state.selected() {
            Some(0) | None => self.chat_models.len() - 1,
            Some(index) => index - 1,
        };
        self.model_state.select(Some(previous));
    }
}

enum ClientAction {
    None,
    Quit,
    Refresh,
    SubmitAsk {
        message: String,
        model_id: Option<String>,
    },
    SubmitAssemble(String),
    SubmitCapture(String),
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &mut ClientApp) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(COLOR_BG)), area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(7),
        ])
        .split(area);

    draw_header(frame, chunks[0], app);
    draw_main(frame, chunks[1], app);
    draw_footer(frame, chunks[2], app);
    if app.mode == InputMode::ModelPicker {
        draw_model_picker(frame, area, app);
    }
}

fn draw_header(frame: &mut ratatui::Frame<'_>, area: Rect, app: &ClientApp) {
    let mode = match app.mode {
        InputMode::Normal => "NAV",
        InputMode::Ask => "ASK",
        InputMode::ContextPreview => "CONTEXT",
        InputMode::Capture => "CAPTURE",
        InputMode::ModelPicker => "MODELS",
    };
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                " ANCILLA REMOTE ",
                Style::default()
                    .fg(COLOR_BG)
                    .bg(COLOR_ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("service {}", app.base_url),
                Style::default().fg(COLOR_TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("mode {mode}"),
                Style::default().fg(COLOR_ACCENT_WARM),
            ),
            Span::raw("  "),
            Span::styled(
                format!(
                    "model {}",
                    app.selected_model_label()
                        .unwrap_or(match app.chat_backend.as_str() {
                            "synthetic" => "synthetic",
                            "legacy" => "server default",
                            _ => "server default",
                        })
                ),
                Style::default().fg(COLOR_SUCCESS),
            ),
        ]),
        Line::from(vec![Span::styled(
            "Browse timeline, preview retrieval, ask live questions, capture entries, and switch models.",
            Style::default().fg(COLOR_MUTED),
        )]),
    ])
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(COLOR_BORDER))
            .style(Style::default().bg(COLOR_PANEL))
            .padding(Padding::horizontal(1)),
    );
    frame.render_widget(header, area);
}

fn draw_main(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut ClientApp) {
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(36), Constraint::Percentage(64)])
        .split(area);

    draw_timeline(frame, panes[0], app);
    draw_detail_panes(frame, panes[1], app);
}

fn draw_timeline(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut ClientApp) {
    let items = if app.timeline.is_empty() {
        vec![ListItem::new(Line::from(vec![Span::styled(
            "No entries yet. Press 'c' to create one.",
            Style::default().fg(COLOR_MUTED),
        )]))]
    } else {
        app.timeline
            .iter()
            .map(|entry| {
                let title = format!(
                    "{}  {}",
                    entry_kind_label(entry.kind),
                    entry.captured_at.format("%Y-%m-%d %H:%M")
                );
                let summary = entry
                    .raw_text
                    .as_deref()
                    .unwrap_or_else(|| entry.asset_ref.as_deref().unwrap_or("(no content)"));
                ListItem::new(vec![
                    Line::from(Span::styled(
                        title,
                        Style::default().fg(COLOR_TEXT).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        truncate(summary, 56),
                        Style::default().fg(COLOR_MUTED),
                    )),
                ])
            })
            .collect::<Vec<_>>()
    };

    let list = List::new(items)
        .block(
            Block::bordered()
                .title(" Timeline ")
                .title_style(
                    Style::default()
                        .fg(COLOR_ACCENT)
                        .add_modifier(Modifier::BOLD),
                )
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(COLOR_BORDER))
                .style(Style::default().bg(COLOR_PANEL_ALT)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(35, 52, 80))
                .fg(COLOR_TEXT)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    frame.render_stateful_widget(list, area, &mut app.timeline_state);
}

fn draw_detail_panes(frame: &mut ratatui::Frame<'_>, area: Rect, app: &ClientApp) {
    let panes = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    let detail_text = selected_entry_text(app.selected_entry());
    let detail = Paragraph::new(detail_text)
        .wrap(Wrap { trim: false })
        .block(
            Block::bordered()
                .title(" Entry Detail ")
                .title_style(
                    Style::default()
                        .fg(COLOR_ACCENT_WARM)
                        .add_modifier(Modifier::BOLD),
                )
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(COLOR_BORDER))
                .style(Style::default().bg(COLOR_PANEL)),
        );
    frame.render_widget(detail, panes[0]);

    let response = Paragraph::new(app.response_body.as_str())
        .wrap(Wrap { trim: false })
        .block(
            Block::bordered()
                .title(format!(" {} ", app.response_title))
                .title_style(
                    Style::default()
                        .fg(COLOR_ACCENT)
                        .add_modifier(Modifier::BOLD),
                )
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(COLOR_BORDER))
                .style(Style::default().bg(COLOR_PANEL_ALT)),
        );
    frame.render_widget(response, panes[1]);
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &ClientApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(4)])
        .split(area);

    let status_style = match app.status.kind {
        StatusKind::Info => Style::default().fg(COLOR_ACCENT),
        StatusKind::Success => Style::default().fg(COLOR_SUCCESS),
        StatusKind::Error => Style::default()
            .fg(COLOR_ERROR)
            .add_modifier(Modifier::BOLD),
    };
    let age_ms = app.status.updated_at.elapsed().as_millis();
    let status = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                " Status ",
                Style::default().fg(COLOR_BG).bg(COLOR_ACCENT_WARM),
            ),
            Span::raw("  "),
            Span::styled(app.status.message.as_str(), status_style),
        ]),
        Line::from(Span::styled(
            format!("Updated {} ms ago", age_ms),
            Style::default().fg(COLOR_MUTED),
        )),
    ])
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(COLOR_BORDER))
            .style(Style::default().bg(COLOR_PANEL)),
    );
    frame.render_widget(status, chunks[0]);

    let title = match app.mode {
        InputMode::Normal => " Keys ",
        InputMode::Ask => " Ask ",
        InputMode::ContextPreview => " Context ",
        InputMode::Capture => " Capture ",
        InputMode::ModelPicker => " Models ",
    };
    let body = match app.mode {
        InputMode::Normal => Text::from(vec![
            Line::from(vec![
                keycap("j/k"),
                Span::styled(" move  ", Style::default().fg(COLOR_MUTED)),
                keycap("r"),
                Span::styled(" refresh  ", Style::default().fg(COLOR_MUTED)),
                keycap("m"),
                Span::styled(" models  ", Style::default().fg(COLOR_MUTED)),
                keycap("s"),
                Span::styled(" context  ", Style::default().fg(COLOR_MUTED)),
                keycap("a"),
                Span::styled(" ask  ", Style::default().fg(COLOR_MUTED)),
                keycap("c"),
                Span::styled(" capture  ", Style::default().fg(COLOR_MUTED)),
                keycap("q"),
                Span::styled(" quit", Style::default().fg(COLOR_MUTED)),
            ]),
            Line::from(Span::styled(
                "Select an entry to inspect its raw text, metadata, and asset linkage.",
                Style::default().fg(COLOR_TEXT),
            )),
        ]),
        InputMode::Ask => Text::from(vec![
            Line::from(Span::styled(
                "Type a question for the remote service and press Enter.",
                Style::default().fg(COLOR_TEXT),
            )),
            Line::from(Span::styled(
                app.input.as_str(),
                Style::default().fg(COLOR_ACCENT),
            )),
        ]),
        InputMode::ContextPreview => Text::from(vec![
            Line::from(Span::styled(
                "Type a message to preview retrieval and context assembly without calling the model.",
                Style::default().fg(COLOR_TEXT),
            )),
            Line::from(Span::styled(
                app.input.as_str(),
                Style::default().fg(COLOR_SUCCESS),
            )),
        ]),
        InputMode::Capture => Text::from(vec![
            Line::from(Span::styled(
                "Type text to ingest into the live service and press Enter.",
                Style::default().fg(COLOR_TEXT),
            )),
            Line::from(Span::styled(
                app.input.as_str(),
                Style::default().fg(COLOR_ACCENT_WARM),
            )),
        ]),
        InputMode::ModelPicker => Text::from(vec![
            Line::from(Span::styled(
                "Choose the model for future ask requests. Press Enter to confirm.",
                Style::default().fg(COLOR_TEXT),
            )),
            Line::from(Span::styled(
                app.selected_model_label()
                    .unwrap_or("No model selected yet."),
                Style::default().fg(COLOR_ACCENT),
            )),
        ]),
    };

    let input = Paragraph::new(body).wrap(Wrap { trim: false }).block(
        Block::bordered()
            .title(title)
            .title_style(
                Style::default()
                    .fg(COLOR_ACCENT)
                    .add_modifier(Modifier::BOLD),
            )
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(COLOR_BORDER))
            .style(Style::default().bg(COLOR_PANEL_ALT)),
    );
    frame.render_widget(input, chunks[1]);
}

fn draw_model_picker(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut ClientApp) {
    let popup_height = (app.chat_models.len() as u16)
        .saturating_mul(3)
        .clamp(8, 14);
    let popup = centered_rect(72, popup_height, area);
    frame.render_widget(Clear, popup);

    let items = app
        .chat_models
        .iter()
        .map(|model| {
            let mut first_line = vec![Span::styled(
                model.label.clone(),
                Style::default().fg(COLOR_TEXT).add_modifier(Modifier::BOLD),
            )];
            if app
                .selected_model_id
                .as_deref()
                .is_some_and(|selected| selected == model.model_id)
            {
                first_line.push(Span::raw("  "));
                first_line.push(Span::styled(
                    "ACTIVE",
                    Style::default()
                        .fg(COLOR_BG)
                        .bg(COLOR_SUCCESS)
                        .add_modifier(Modifier::BOLD),
                ));
            }

            let mut second_line = Vec::new();
            if let Some(description) = model.description.as_deref() {
                second_line.push(Span::styled(
                    description.to_string(),
                    Style::default().fg(COLOR_MUTED),
                ));
            }
            if let Some(thinking_mode) = model.thinking_mode {
                if !second_line.is_empty() {
                    second_line.push(Span::raw("  "));
                }
                second_line.push(Span::styled(
                    match model.thinking_effort {
                        Some(effort) => format!(
                            "{} / {}",
                            thinking_mode_label(thinking_mode),
                            thinking_effort_label(effort)
                        ),
                        None => thinking_mode_label(thinking_mode).to_string(),
                    },
                    Style::default().fg(COLOR_ACCENT_WARM),
                ));
            }
            if second_line.is_empty() {
                second_line.push(Span::styled(
                    model.model_id.clone(),
                    Style::default().fg(COLOR_MUTED),
                ));
            }

            ListItem::new(vec![Line::from(first_line), Line::from(second_line)])
        })
        .collect::<Vec<_>>();

    let list = List::new(items)
        .block(
            Block::bordered()
                .title(" Model Picker ")
                .title_style(
                    Style::default()
                        .fg(COLOR_ACCENT)
                        .add_modifier(Modifier::BOLD),
                )
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(COLOR_BORDER))
                .style(Style::default().bg(COLOR_PANEL)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(35, 52, 80))
                .fg(COLOR_TEXT)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    frame.render_stateful_widget(list, popup, &mut app.model_state);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(height) / 2),
            Constraint::Length(height.min(area.height)),
            Constraint::Min(0),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(area.width.saturating_sub(width) / 2),
            Constraint::Length(width.min(area.width)),
            Constraint::Min(0),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn thinking_mode_label(mode: crate::model::ChatThinkingMode) -> &'static str {
    match mode {
        crate::model::ChatThinkingMode::Adaptive => "adaptive thinking",
        crate::model::ChatThinkingMode::Enabled => "extended thinking",
    }
}

fn thinking_effort_label(effort: crate::model::ChatThinkingEffort) -> &'static str {
    match effort {
        crate::model::ChatThinkingEffort::Low => "low",
        crate::model::ChatThinkingEffort::Medium => "medium",
        crate::model::ChatThinkingEffort::High => "high",
        crate::model::ChatThinkingEffort::Max => "max",
    }
}

fn keycap(label: &str) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(COLOR_BG)
            .bg(COLOR_ACCENT)
            .add_modifier(Modifier::BOLD),
    )
}

fn gate_decision_label(decision: GateDecision) -> &'static str {
    match decision {
        GateDecision::NoInject => "no inject",
        GateDecision::InjectCompact => "inject compact",
        GateDecision::DeferToTool => "defer to tool",
    }
}

fn format_memory(memory: &MemoryRecord) -> String {
    format!(
        "{} / {}: {}",
        memory_kind_label(memory.kind),
        memory_subtype_label(memory.subtype),
        memory.display_text
    )
}

fn memory_kind_label(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Semantic => "semantic",
        MemoryKind::Episodic => "episodic",
        MemoryKind::Procedural => "procedural",
    }
}

fn memory_subtype_label(subtype: MemorySubtype) -> &'static str {
    match subtype {
        MemorySubtype::Fact => "fact",
        MemorySubtype::Preference => "preference",
        MemorySubtype::Project => "project",
        MemorySubtype::Habit => "habit",
        MemorySubtype::Person => "person",
        MemorySubtype::Place => "place",
        MemorySubtype::Goal => "goal",
    }
}

fn selected_entry_text(entry: Option<&Entry>) -> Text<'static> {
    let Some(entry) = entry else {
        return Text::from(vec![
            Line::from(Span::styled(
                "No entry selected.",
                Style::default().fg(COLOR_MUTED),
            )),
            Line::from(Span::styled(
                "Use 'c' to capture a remote entry and 'r' to refresh.",
                Style::default().fg(COLOR_TEXT),
            )),
        ]);
    };

    Text::from(vec![
        Line::from(vec![
            Span::styled("Kind: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                entry_kind_label(entry.kind),
                Style::default()
                    .fg(COLOR_ACCENT_WARM)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Captured: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                entry
                    .captured_at
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string(),
                Style::default().fg(COLOR_TEXT),
            ),
        ]),
        Line::from(vec![
            Span::styled("Timezone: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(entry.timezone.clone(), Style::default().fg(COLOR_TEXT)),
        ]),
        Line::from(vec![
            Span::styled("Source app: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                entry
                    .source_app
                    .clone()
                    .unwrap_or_else(|| "(none)".to_string()),
                Style::default().fg(COLOR_TEXT),
            ),
        ]),
        Line::from(vec![
            Span::styled("Asset ref: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                entry
                    .asset_ref
                    .clone()
                    .unwrap_or_else(|| "(null)".to_string()),
                Style::default().fg(COLOR_TEXT),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Raw text",
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            entry
                .raw_text
                .clone()
                .unwrap_or_else(|| "(no raw text on this entry)".to_string()),
            Style::default().fg(COLOR_TEXT),
        )),
    ])
}

fn entry_kind_label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Text => "TEXT",
        EntryKind::ChatTurn => "CHAT",
        EntryKind::Import => "IMPORT",
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        let truncated = value
            .chars()
            .take(max_chars.saturating_sub(3))
            .collect::<String>();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GateDecision, MemoryState, ScoredMemory, now_utc};
    use ratatui::{Terminal, backend::TestBackend};
    use uuid::Uuid;

    #[test]
    fn truncate_preserves_short_text() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a much longer sentence", 8), "a muc...");
    }

    #[test]
    fn ask_mode_renders_full_input_line() {
        let mut app = ClientApp::new("http://example.test:3000".to_string());
        app.mode = InputMode::Ask;
        app.input = "what language do i like".to_string();

        let screen = render_screen(&mut app);

        assert!(screen.contains("what language do i like"));
    }

    #[test]
    fn capture_mode_renders_full_input_line() {
        let mut app = ClientApp::new("http://example.test:3000".to_string());
        app.mode = InputMode::Capture;
        app.input = "I want to remember this long sentence.".to_string();

        let screen = render_screen(&mut app);

        assert!(screen.contains("I want to remember this long sentence."));
    }

    #[test]
    fn context_mode_renders_full_input_line() {
        let mut app = ClientApp::new("http://example.test:3000".to_string());
        app.mode = InputMode::ContextPreview;
        app.input = "What am I building right now?".to_string();

        let screen = render_screen(&mut app);

        assert!(screen.contains("What am I building right now?"));
    }

    #[test]
    fn context_preview_formats_selected_memories_and_candidates() {
        let mut app = ClientApp::new("http://example.test:3000".to_string());
        let memory = MemoryRecord {
            id: Uuid::new_v4(),
            lineage_id: Uuid::new_v4(),
            kind: MemoryKind::Semantic,
            subtype: MemorySubtype::Project,
            display_text: "You are building Ancilla.".to_string(),
            retrieval_text: "project Ancilla".to_string(),
            attrs: empty_object(),
            observed_at: Some(now_utc()),
            valid_from: now_utc(),
            valid_to: None,
            confidence: 0.95,
            salience: 0.9,
            state: MemoryState::Accepted,
            embedding: None,
            source_artifact_ids: Vec::new(),
            thread_id: None,
            parent_id: None,
            path: None,
            created_at: now_utc(),
            updated_at: now_utc(),
        };
        app.set_context_preview(
            "What am I building?".to_string(),
            AssembleContextResponse {
                trace_id: Uuid::new_v4(),
                decision: GateDecision::InjectCompact,
                gate_confidence: 0.88,
                gate_reason: "project memory available".to_string(),
                context: Some(
                    "Relevant personal context:\n- You are building Ancilla.".to_string(),
                ),
                selected_memories: vec![memory.clone()],
                candidates: vec![ScoredMemory {
                    memory,
                    semantic_score: 0.8,
                    lexical_score: 0.6,
                    fusion_score: 0.7,
                    temporal_bonus: 0.0,
                    thread_bonus: 0.0,
                    salience_bonus: 0.1,
                    confidence_bonus: 0.1,
                    reinjection_penalty: 0.0,
                    stale_penalty: 0.0,
                    final_score: 0.9,
                    prior_injected: false,
                    candidate_rank: 0,
                }],
            },
        );

        assert!(app.response_title.contains("Context [inject compact"));
        assert!(app.response_body.contains("Selected memories"));
        assert!(
            app.response_body
                .contains("semantic / project: You are building Ancilla.")
        );
        assert!(app.response_body.contains("Top candidates"));
    }

    #[test]
    fn request_error_updates_status_and_response_pane() {
        let mut app = ClientApp::new("http://example.test:3000".to_string());
        app.set_request_error(
            "Chat request failed for Kimi K2 Thinking.",
            "Chat Error [Kimi K2 Thinking]",
            "Q: hello\n\n502 Bad Gateway: upstream model failed",
        );

        assert_eq!(app.status.kind, StatusKind::Error);
        assert_eq!(app.response_title, "Chat Error [Kimi K2 Thinking]");
        assert!(
            app.response_body
                .contains("502 Bad Gateway: upstream model failed")
        );
    }

    fn render_screen(app: &mut ClientApp) -> String {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
        terminal
            .draw(|frame| draw(frame, app))
            .expect("frame should render");

        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        let mut rows = Vec::with_capacity(area.height as usize);
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            rows.push(row);
        }
        rows.join("\n")
    }
}
