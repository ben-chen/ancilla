use std::{
    io,
    time::{Duration, Instant},
};

use crate::{
    client_config::{ClientConfig, normalize_base_url},
    model::{
        ApiErrorBody, AssembleContextRequest, AssembleContextResponse, CaptureEntryResponse,
        ChatModelOption, ChatModelsResponse, ChatRespondRequest, ChatResponse, ChatStreamEvent,
        ConversationRole, ConversationTurn, CreateMemoryRequest, Entry, EntryKind, GateDecision,
        MemoryKind, MemoryRecord, MemorySubtype, empty_object,
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
use tokio::sync::mpsc;
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
        app.drain_stream_events();
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
                app.set_info(format!(
                    "Refreshing memories and timeline from {}",
                    api.base_url
                ));
                if let Err(error) = app.refresh_remote_state(&api).await {
                    app.set_request_error("Refresh failed.", "Refresh Error", error.to_string());
                }
            }
            ClientAction::SubmitAsk { message, model_id } => {
                app.set_info("Sending question to remote service...");
                let receiver = api.start_ask_stream(
                    &message,
                    model_id.as_deref(),
                    app.conversation_id,
                    &app.recent_turns,
                );
                app.begin_chat_stream(message, receiver);
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
                        if let Err(error) = app.refresh_browse_data(&api).await {
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

#[derive(Clone)]
struct RemoteApi {
    base_url: String,
    http: reqwest::Client,
    stream_http: reqwest::Client,
}

impl RemoteApi {
    fn new(base_url: String, config: &ClientConfig) -> anyhow::Result<Self> {
        let headers = basic_auth_headers(config)?;
        let http = build_http_client(headers.clone(), Duration::from_secs(20))?;
        let stream_http = build_http_client(headers, Duration::from_secs(60 * 60))?;
        Ok(Self {
            base_url,
            http,
            stream_http,
        })
    }

    async fn get_timeline(&self) -> anyhow::Result<Vec<Entry>> {
        self.get_json("/v1/timeline").await
    }

    async fn get_memories(&self) -> anyhow::Result<Vec<MemoryRecord>> {
        self.get_json("/v1/memories").await
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

    fn start_ask_stream(
        &self,
        message: &str,
        model_id: Option<&str>,
        conversation_id: Uuid,
        recent_turns: &[ConversationTurn],
    ) -> mpsc::Receiver<RemoteChatUpdate> {
        let request = ChatRespondRequest {
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
        };
        let url = self.url("/v1/chat/respond/stream");
        let client = self.stream_http.clone();
        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            if let Err(error) = pump_chat_stream(client, url, request, tx.clone()).await {
                let _ = tx.send(RemoteChatUpdate::Error(error.to_string())).await;
            }
        });

        rx
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

fn basic_auth_headers(config: &ClientConfig) -> anyhow::Result<reqwest::header::HeaderMap> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let (Some(username), Some(password)) = (
        config.basic_auth_username.as_deref(),
        config.basic_auth_password.as_deref(),
    ) {
        let token = BASE64.encode(format!("{username}:{password}"));
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Basic {token}"))
                .context("invalid basic auth header value")?,
        );
    }
    Ok(headers)
}

fn build_http_client(
    headers: reqwest::header::HeaderMap,
    timeout: Duration,
) -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .default_headers(headers)
        .build()
        .context("failed to build HTTP client")
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

async fn pump_chat_stream(
    client: reqwest::Client,
    url: String,
    request: ChatRespondRequest,
    tx: mpsc::Sender<RemoteChatUpdate>,
) -> anyhow::Result<()> {
    let mut response = client
        .post(url)
        .json(&request)
        .send()
        .await
        .with_context(|| "request failed for POST /v1/chat/respond/stream")?;
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
        bail!("{status}: {message}");
    }

    let mut buffer = String::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| "failed to read chat stream chunk")?
    {
        let chunk =
            std::str::from_utf8(&chunk).with_context(|| "chat stream chunk was not UTF-8")?;
        buffer.push_str(chunk);
        while let Some(line_end) = buffer.find('\n') {
            let line = buffer.drain(..=line_end).collect::<String>();
            if let Some(event) = parse_stream_line(&line)? {
                if tx.send(RemoteChatUpdate::Event(event)).await.is_err() {
                    return Ok(());
                }
            }
        }
    }

    if let Some(event) = parse_stream_line(&buffer)? {
        let _ = tx.send(RemoteChatUpdate::Event(event)).await;
    }
    Ok(())
}

fn parse_stream_line(line: &str) -> anyhow::Result<Option<ChatStreamEvent>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    serde_json::from_str::<ChatStreamEvent>(trimmed)
        .map(Some)
        .with_context(|| "failed to decode chat stream event")
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
enum BrowseTab {
    Memories,
    Timeline,
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

struct ActiveChatStream {
    prompt: String,
    answer: String,
    trace_id: Option<Uuid>,
    injected_context: Option<String>,
    selected_memories: Vec<MemoryRecord>,
    model_id: Option<String>,
}

enum RemoteChatUpdate {
    Event(ChatStreamEvent),
    Error(String),
}

struct ClientApp {
    base_url: String,
    conversation_id: Uuid,
    recent_turns: Vec<ConversationTurn>,
    mode: InputMode,
    browse_tab: BrowseTab,
    memories: Vec<MemoryRecord>,
    memory_state: ListState,
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
    stream_receiver: Option<mpsc::Receiver<RemoteChatUpdate>>,
    active_stream: Option<ActiveChatStream>,
}

impl ClientApp {
    fn new(base_url: String) -> Self {
        let mut memory_state = ListState::default();
        memory_state.select(Some(0));
        let mut timeline_state = ListState::default();
        timeline_state.select(Some(0));
        Self {
            base_url,
            conversation_id: Uuid::new_v4(),
            recent_turns: Vec::new(),
            mode: InputMode::Normal,
            browse_tab: BrowseTab::Memories,
            memories: Vec::new(),
            memory_state,
            timeline: Vec::new(),
            timeline_state,
            input: String::new(),
            response_title: "Response".to_string(),
            response_body:
                "Press 's' to preview retrieval context, 'a' to ask the live service, or 'c' to capture a new memory. The memory browser is the default view; press Tab to switch to the raw timeline."
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
            stream_receiver: None,
            active_stream: None,
        }
    }

    async fn refresh_remote_state(&mut self, api: &RemoteApi) -> anyhow::Result<()> {
        self.refresh_models(api).await?;
        self.refresh_browse_data(api).await
    }

    async fn refresh_browse_data(&mut self, api: &RemoteApi) -> anyhow::Result<()> {
        self.refresh_memories(api).await?;
        self.refresh_timeline(api).await?;
        self.set_success(format!(
            "Loaded {} memories and {} entries from {}.",
            self.memories.len(),
            self.timeline.len(),
            api.base_url
        ));
        Ok(())
    }

    async fn refresh_memories(&mut self, api: &RemoteApi) -> anyhow::Result<()> {
        let memories = api.get_memories().await?;
        self.memories = memories;
        let next_selected = match self.memory_state.selected() {
            Some(index) if !self.memories.is_empty() => Some(index.min(self.memories.len() - 1)),
            _ if self.memories.is_empty() => None,
            _ => Some(0),
        };
        self.memory_state.select(next_selected);
        Ok(())
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

    fn has_active_stream(&self) -> bool {
        self.stream_receiver.is_some()
    }

    fn begin_chat_stream(&mut self, prompt: String, receiver: mpsc::Receiver<RemoteChatUpdate>) {
        self.active_stream = Some(ActiveChatStream {
            prompt: prompt.clone(),
            answer: String::new(),
            trace_id: None,
            injected_context: None,
            selected_memories: Vec::new(),
            model_id: None,
        });
        self.stream_receiver = Some(receiver);
        self.response_title = "Response [streaming]".to_string();
        self.response_body = format!("Q: {prompt}\n\n");
        self.set_info("Streaming response from the remote service...");
    }

    fn drain_stream_events(&mut self) {
        let Some(mut receiver) = self.stream_receiver.take() else {
            return;
        };

        let mut keep_receiver = true;
        loop {
            match receiver.try_recv() {
                Ok(update) => match update {
                    RemoteChatUpdate::Event(event) => match event {
                        ChatStreamEvent::Start {
                            trace_id,
                            model_id,
                            injected_context,
                            selected_memories,
                        } => {
                            let model_label = model_id
                                .as_deref()
                                .and_then(|value| self.model_label(value))
                                .map(ToOwned::to_owned)
                                .or_else(|| model_id.clone())
                                .unwrap_or_else(|| self.chat_backend.clone());
                            let Some(stream) = self.active_stream.as_mut() else {
                                continue;
                            };
                            stream.trace_id = Some(trace_id);
                            stream.model_id = model_id.clone();
                            stream.injected_context = injected_context;
                            stream.selected_memories = selected_memories;
                            self.response_title = format!(
                                "Response [{} | {} memories | trace {} | streaming]",
                                model_label,
                                stream.selected_memories.len(),
                                trace_id
                            );
                        }
                        ChatStreamEvent::Delta { delta } => {
                            if let Some(stream) = self.active_stream.as_mut() {
                                stream.answer.push_str(&delta);
                                self.response_body =
                                    format!("Q: {}\n\n{}", stream.prompt, stream.answer);
                            }
                        }
                        ChatStreamEvent::Done {
                            answer,
                            trace_id,
                            model_id,
                            ..
                        } => {
                            if let Some(stream) = self.active_stream.take() {
                                self.set_chat_response(
                                    stream.prompt,
                                    ChatResponse {
                                        answer,
                                        trace_id,
                                        injected_context: stream.injected_context,
                                        selected_memories: stream.selected_memories,
                                        model_id,
                                    },
                                );
                                self.set_success("Answer received.");
                            }
                            keep_receiver = false;
                        }
                        ChatStreamEvent::Error {
                            error, model_id, ..
                        } => {
                            let stream = self.active_stream.take();
                            let model_label = model_id
                                .as_deref()
                                .and_then(|value| self.model_label(value))
                                .map(ToOwned::to_owned)
                                .or(model_id)
                                .unwrap_or_else(|| {
                                    self.selected_model_label()
                                        .unwrap_or("server default")
                                        .to_string()
                                });
                            let body = if let Some(stream) = stream {
                                if stream.answer.is_empty() {
                                    format!("Q: {}\n\n{}", stream.prompt, error)
                                } else {
                                    format!(
                                        "Q: {}\n\nPartial response:\n{}\n\n{}",
                                        stream.prompt, stream.answer, error
                                    )
                                }
                            } else {
                                error
                            };
                            self.set_request_error(
                                format!("Chat stream failed for {model_label}."),
                                format!("Chat Error [{model_label}]"),
                                body,
                            );
                            keep_receiver = false;
                        }
                    },
                    RemoteChatUpdate::Error(error) => {
                        let prompt = self
                            .active_stream
                            .take()
                            .map(|stream| stream.prompt)
                            .unwrap_or_default();
                        let body = if prompt.is_empty() {
                            error
                        } else {
                            format!("Q: {prompt}\n\n{error}")
                        };
                        let model_label = self
                            .selected_model_label()
                            .unwrap_or("server default")
                            .to_string();
                        self.set_request_error(
                            format!("Chat request failed for {model_label}."),
                            format!("Chat Error [{model_label}]"),
                            body,
                        );
                        keep_receiver = false;
                    }
                },
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    if self.active_stream.is_some() {
                        let disconnected_body = self
                            .active_stream
                            .take()
                            .map(|stream| {
                                if stream.answer.is_empty() {
                                    format!("Q: {}\n\nstream disconnected", stream.prompt)
                                } else {
                                    format!(
                                        "Q: {}\n\nPartial response:\n{}\n\nstream disconnected",
                                        stream.prompt, stream.answer
                                    )
                                }
                            })
                            .unwrap_or_else(|| "stream disconnected".to_string());
                        self.set_request_error(
                            "Chat stream ended unexpectedly.",
                            "Chat Error [stream disconnected]",
                            disconnected_body,
                        );
                    }
                    keep_receiver = false;
                    break;
                }
            }
        }

        if keep_receiver {
            self.stream_receiver = Some(receiver);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ClientAction {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return ClientAction::Quit;
        }
        match self.mode {
            InputMode::Normal => self.handle_normal_key(key),
            InputMode::Ask => self.handle_input_key(key, InputMode::Ask),
            InputMode::ContextPreview => self.handle_input_key(key, InputMode::ContextPreview),
            InputMode::Capture => self.handle_input_key(key, InputMode::Capture),
            InputMode::ModelPicker => self.handle_model_picker_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> ClientAction {
        if self.has_active_stream()
            && matches!(
                key.code,
                KeyCode::Char('a') | KeyCode::Char('c') | KeyCode::Char('r') | KeyCode::Char('s')
            )
        {
            self.set_error("Wait for the current streamed response to finish.");
            return ClientAction::None;
        }
        match key.code {
            KeyCode::Char('q') => ClientAction::Quit,
            KeyCode::Char('r') => ClientAction::Refresh,
            KeyCode::Tab | KeyCode::Char('v') => {
                self.toggle_browse_tab();
                ClientAction::None
            }
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

    fn selected_memory(&self) -> Option<&MemoryRecord> {
        self.memory_state
            .selected()
            .and_then(|index| self.memories.get(index))
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
        match self.browse_tab {
            BrowseTab::Memories => {
                if self.memories.is_empty() {
                    self.memory_state.select(None);
                    return;
                }
                let current = self.memory_state.selected().unwrap_or(0);
                let next = (current + 1).min(self.memories.len() - 1);
                self.memory_state.select(Some(next));
            }
            BrowseTab::Timeline => {
                if self.timeline.is_empty() {
                    self.timeline_state.select(None);
                    return;
                }
                let current = self.timeline_state.selected().unwrap_or(0);
                let next = (current + 1).min(self.timeline.len() - 1);
                self.timeline_state.select(Some(next));
            }
        }
    }

    fn select_previous(&mut self) {
        match self.browse_tab {
            BrowseTab::Memories => {
                if self.memories.is_empty() {
                    self.memory_state.select(None);
                    return;
                }
                let current = self.memory_state.selected().unwrap_or(0);
                let next = current.saturating_sub(1);
                self.memory_state.select(Some(next));
            }
            BrowseTab::Timeline => {
                if self.timeline.is_empty() {
                    self.timeline_state.select(None);
                    return;
                }
                let current = self.timeline_state.selected().unwrap_or(0);
                let next = current.saturating_sub(1);
                self.timeline_state.select(Some(next));
            }
        }
    }

    fn select_first(&mut self) {
        match self.browse_tab {
            BrowseTab::Memories => {
                if self.memories.is_empty() {
                    self.memory_state.select(None);
                } else {
                    self.memory_state.select(Some(0));
                }
            }
            BrowseTab::Timeline => {
                if self.timeline.is_empty() {
                    self.timeline_state.select(None);
                } else {
                    self.timeline_state.select(Some(0));
                }
            }
        }
    }

    fn select_last(&mut self) {
        match self.browse_tab {
            BrowseTab::Memories => {
                if self.memories.is_empty() {
                    self.memory_state.select(None);
                } else {
                    self.memory_state.select(Some(self.memories.len() - 1));
                }
            }
            BrowseTab::Timeline => {
                if self.timeline.is_empty() {
                    self.timeline_state.select(None);
                } else {
                    self.timeline_state.select(Some(self.timeline.len() - 1));
                }
            }
        }
    }

    fn toggle_browse_tab(&mut self) {
        self.browse_tab = match self.browse_tab {
            BrowseTab::Memories => BrowseTab::Timeline,
            BrowseTab::Timeline => BrowseTab::Memories,
        };
        let label = match self.browse_tab {
            BrowseTab::Memories => "Memory browser",
            BrowseTab::Timeline => "Timeline browser",
        };
        self.set_info(format!("{label} active. Press Tab to switch views."));
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
            "Browse durable memories by default, switch to the raw timeline with Tab, preview retrieval, ask live questions, capture entries, and switch models.",
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

    draw_browser(frame, panes[0], app);
    draw_detail_panes(frame, panes[1], app);
}

fn draw_browser(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut ClientApp) {
    match app.browse_tab {
        BrowseTab::Memories => draw_memory_list(frame, area, app),
        BrowseTab::Timeline => draw_timeline_list(frame, area, app),
    }
}

fn draw_memory_list(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut ClientApp) {
    let items = if app.memories.is_empty() {
        vec![ListItem::new(Line::from(vec![Span::styled(
            "No memories yet. Press 'c' to create one.",
            Style::default().fg(COLOR_MUTED),
        )]))]
    } else {
        app.memories
            .iter()
            .map(|memory| {
                let title = format!(
                    "{} / {}  {}",
                    memory_kind_label(memory.kind),
                    memory_subtype_label(memory.subtype),
                    memory.updated_at.format("%Y-%m-%d %H:%M")
                );
                ListItem::new(vec![
                    Line::from(Span::styled(
                        title,
                        Style::default().fg(COLOR_TEXT).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        truncate(&memory.display_text, 56),
                        Style::default().fg(COLOR_MUTED),
                    )),
                ])
            })
            .collect::<Vec<_>>()
    };

    let title = browser_title(app.browse_tab, app.memories.len(), app.timeline.len());
    let list = List::new(items)
        .block(browser_block(title))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(35, 52, 80))
                .fg(COLOR_TEXT)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    frame.render_stateful_widget(list, area, &mut app.memory_state);
}

fn draw_timeline_list(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut ClientApp) {
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

    let title = browser_title(app.browse_tab, app.memories.len(), app.timeline.len());
    let list = List::new(items)
        .block(browser_block(title))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(35, 52, 80))
                .fg(COLOR_TEXT)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    frame.render_stateful_widget(list, area, &mut app.timeline_state);
}

fn browser_title(active_tab: BrowseTab, memory_count: usize, timeline_count: usize) -> String {
    let memory_label = match active_tab {
        BrowseTab::Memories => format!("[Memories {memory_count}]"),
        BrowseTab::Timeline => format!(" Memories {memory_count} "),
    };
    let timeline_label = match active_tab {
        BrowseTab::Timeline => format!("[Timeline {timeline_count}]"),
        BrowseTab::Memories => format!(" Timeline {timeline_count} "),
    };
    format!(" {memory_label}  {timeline_label} ")
}

fn browser_block(title: String) -> Block<'static> {
    Block::bordered()
        .title(title)
        .title_style(
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(COLOR_BORDER))
        .style(Style::default().bg(COLOR_PANEL_ALT))
}

fn draw_detail_panes(frame: &mut ratatui::Frame<'_>, area: Rect, app: &ClientApp) {
    let panes = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    let (detail_title, detail_text) = match app.browse_tab {
        BrowseTab::Memories => (
            " Memory Detail ",
            selected_memory_text(app.selected_memory()),
        ),
        BrowseTab::Timeline => (" Entry Detail ", selected_entry_text(app.selected_entry())),
    };
    let detail = Paragraph::new(detail_text)
        .wrap(Wrap { trim: false })
        .block(
            Block::bordered()
                .title(detail_title)
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
                keycap("tab"),
                Span::styled(" switch view  ", Style::default().fg(COLOR_MUTED)),
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
                "Select a memory to inspect the durable recall record, or switch to Timeline for raw entries and chat turns.",
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

fn selected_memory_text(memory: Option<&MemoryRecord>) -> Text<'static> {
    let Some(memory) = memory else {
        return Text::from(vec![
            Line::from(Span::styled(
                "No memory selected.",
                Style::default().fg(COLOR_MUTED),
            )),
            Line::from(Span::styled(
                "Use 'c' to capture a durable memory and 'Tab' to switch to the raw timeline.",
                Style::default().fg(COLOR_TEXT),
            )),
        ]);
    };

    Text::from(vec![
        Line::from(vec![
            Span::styled("Kind: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                memory_kind_label(memory.kind),
                Style::default()
                    .fg(COLOR_ACCENT_WARM)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("Subtype: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                memory_subtype_label(memory.subtype),
                Style::default()
                    .fg(COLOR_ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("State: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                format!("{:?}", memory.state).to_lowercase(),
                Style::default().fg(COLOR_TEXT),
            ),
        ]),
        Line::from(vec![
            Span::styled("Updated: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                memory
                    .updated_at
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string(),
                Style::default().fg(COLOR_TEXT),
            ),
        ]),
        Line::from(vec![
            Span::styled("Confidence: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                format!("{:.2}", memory.confidence),
                Style::default().fg(COLOR_TEXT),
            ),
            Span::raw("  "),
            Span::styled("Salience: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                format!("{:.2}", memory.salience),
                Style::default().fg(COLOR_TEXT),
            ),
        ]),
        Line::from(vec![
            Span::styled("Thread: ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                memory
                    .thread_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "(none)".to_string()),
                Style::default().fg(COLOR_TEXT),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Display text",
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            memory.display_text.clone(),
            Style::default().fg(COLOR_TEXT),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Retrieval text",
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            memory.retrieval_text.clone(),
            Style::default().fg(COLOR_TEXT),
        )),
    ])
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
    fn ctrl_c_quits_from_any_mode() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        let mut app = ClientApp::new("http://example.test:3000".to_string());
        assert!(matches!(app.handle_key(key), ClientAction::Quit));

        let mut app = ClientApp::new("http://example.test:3000".to_string());
        app.mode = InputMode::Ask;
        assert!(matches!(app.handle_key(key), ClientAction::Quit));

        let mut app = ClientApp::new("http://example.test:3000".to_string());
        app.mode = InputMode::ModelPicker;
        assert!(matches!(app.handle_key(key), ClientAction::Quit));
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
    fn memory_browser_is_the_default_view() {
        let mut app = ClientApp::new("http://example.test:3000".to_string());

        let screen = render_screen(&mut app);

        assert!(screen.contains("[Memories 0]"));
        assert!(screen.contains("No memory selected."));
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
            "Chat request failed for Kimi K2.5.",
            "Chat Error [Kimi K2.5]",
            "Q: hello\n\n502 Bad Gateway: upstream model failed",
        );

        assert_eq!(app.status.kind, StatusKind::Error);
        assert_eq!(app.response_title, "Chat Error [Kimi K2.5]");
        assert!(
            app.response_body
                .contains("502 Bad Gateway: upstream model failed")
        );
    }

    #[test]
    fn parse_stream_line_decodes_ndjson_events() {
        let trace_id = Uuid::new_v4();
        let event = parse_stream_line(
            &format!(
                "{{\"type\":\"done\",\"answer\":\"Hello\",\"trace_id\":\"{trace_id}\",\"model_id\":\"moonshotai.kimi-k2.5\",\"stop_reason\":\"end_turn\"}}"
            ),
        )
        .unwrap()
        .unwrap();

        assert!(matches!(
            event,
            ChatStreamEvent::Done {
                answer,
                model_id: Some(model_id),
                ..
            } if answer == "Hello" && model_id == "moonshotai.kimi-k2.5"
        ));
    }

    #[test]
    fn draining_stream_events_updates_response_incrementally() {
        let trace_id = Uuid::new_v4();
        let mut app = ClientApp::new("http://example.test:3000".to_string());
        app.apply_chat_models(ChatModelsResponse {
            backend: "bedrock".to_string(),
            default_model_id: Some("moonshotai.kimi-k2.5".to_string()),
            models: vec![ChatModelOption {
                label: "Kimi K2.5".to_string(),
                model_id: "moonshotai.kimi-k2.5".to_string(),
                description: None,
                thinking_mode: None,
                thinking_effort: None,
                thinking_budget_tokens: None,
            }],
        });
        let (tx, rx) = mpsc::channel(8);
        app.begin_chat_stream("What am I building?".to_string(), rx);

        tx.try_send(RemoteChatUpdate::Event(ChatStreamEvent::Start {
            trace_id,
            model_id: Some("moonshotai.kimi-k2.5".to_string()),
            injected_context: Some(
                "Relevant personal context:\n- You are building Ancilla.".to_string(),
            ),
            selected_memories: vec![MemoryRecord {
                id: Uuid::new_v4(),
                lineage_id: Uuid::new_v4(),
                kind: MemoryKind::Semantic,
                subtype: MemorySubtype::Project,
                display_text: "You are building Ancilla.".to_string(),
                retrieval_text: "building Ancilla".to_string(),
                attrs: empty_object(),
                observed_at: Some(now_utc()),
                valid_from: now_utc(),
                valid_to: None,
                confidence: 0.9,
                salience: 0.8,
                state: MemoryState::Accepted,
                embedding: None,
                source_artifact_ids: Vec::new(),
                thread_id: None,
                parent_id: None,
                path: None,
                created_at: now_utc(),
                updated_at: now_utc(),
            }],
        }))
        .unwrap();
        tx.try_send(RemoteChatUpdate::Event(ChatStreamEvent::Delta {
            delta: "You are building ".to_string(),
        }))
        .unwrap();
        tx.try_send(RemoteChatUpdate::Event(ChatStreamEvent::Delta {
            delta: "Ancilla.".to_string(),
        }))
        .unwrap();
        tx.try_send(RemoteChatUpdate::Event(ChatStreamEvent::Done {
            answer: "You are building Ancilla.".to_string(),
            trace_id,
            model_id: Some("moonshotai.kimi-k2.5".to_string()),
            stop_reason: Some("end_turn".to_string()),
        }))
        .unwrap();

        app.drain_stream_events();

        assert!(app.response_title.contains("Kimi K2.5"));
        assert!(app.response_body.contains("You are building Ancilla."));
        assert!(app.stream_receiver.is_none());
        assert!(app.active_stream.is_none());
        assert_eq!(app.recent_turns.len(), 2);
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
