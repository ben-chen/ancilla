import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react'

type MemoryKind = 'semantic' | 'episodic' | 'procedural'
type ConversationRole = 'user' | 'assistant'
type ActiveView = 'chat' | 'memories'
type ThemeMode = 'system' | 'light' | 'dark'

type ChatModelOption = {
  label: string
  model_id: string
}

type ChatModelsResponse = {
  default_model_id?: string | null
  models: ChatModelOption[]
}

type ConversationTurn = {
  role: ConversationRole
  text: string
}

type MemoryRecord = {
  id: string
  title: string
  kind: MemoryKind
  state?: 'candidate' | 'accepted' | 'superseded' | 'rejected' | 'deleted'
  tags: string[]
  content_markdown: string
}

type CaptureEntryResponse = {
  memories: MemoryRecord[]
}

type LlmTokenUsage = {
  total_tokens: number
}

type LlmCostBreakdown = {
  total_usd: number
}

type LlmCallMetrics = {
  usage: LlmTokenUsage
  cost: LlmCostBreakdown
}

type ChatStreamEvent =
  | {
      type: 'start'
      selected_memories: MemoryRecord[]
      gate_metrics?: LlmCallMetrics | null
      remember_current_conversation_used?: boolean
      remembered_memories_count?: number
    }
  | { type: 'delta'; delta: string }
  | {
      type: 'done'
      answer: string
      chat_metrics?: LlmCallMetrics | null
    }
  | { type: 'error'; error: string }

const CHAT_MODEL_KEY = 'ancilla.web.chat_model_id'
const GATE_MODEL_KEY = 'ancilla.web.gate_model_id'
const MEMORY_ALERTS_ENABLED_KEY = 'ancilla.web.memory_alerts_enabled'
const SPEAK_ENABLED_KEY = 'ancilla.web.speak_enabled'
const THEME_MODE_KEY = 'ancilla.web.theme_mode'
const DEFAULT_MEMORY_TEMPLATE =
  '# Title\n\nTags: one-tag, another-tag\n\nWrite the memory here.\n'

type MemoryToolAlert = {
  id: number
  message: string
}

function readActiveViewFromLocation() {
  if (typeof window === 'undefined') {
    return 'chat' as ActiveView
  }

  const params = new URLSearchParams(window.location.search)
  return params.get('view') === 'memories' ? 'memories' : 'chat'
}

function writeActiveViewToLocation(view: ActiveView, replace = false) {
  if (typeof window === 'undefined') {
    return
  }

  const nextUrl = new URL(window.location.href)
  if (view === 'chat') {
    nextUrl.searchParams.delete('view')
  } else {
    nextUrl.searchParams.set('view', view)
  }

  const nextRelativeUrl = `${nextUrl.pathname}${nextUrl.search}${nextUrl.hash}`
  const currentRelativeUrl =
    `${window.location.pathname}${window.location.search}${window.location.hash}`

  if (nextRelativeUrl === currentRelativeUrl) {
    return
  }

  const method = replace ? 'replaceState' : 'pushState'
  window.history[method](null, '', nextRelativeUrl)
}

function createConversationId() {
  return window.crypto?.randomUUID?.() ?? `conversation-${Date.now()}`
}

function stripMarkdown(markdown: string) {
  return markdown
    .replace(/^#\s+/gm, '')
    .replace(/^Tags:\s+/gim, '')
    .replace(/[*_`>#-]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
}

function excerpt(markdown: string, maxChars = 120) {
  const clean = stripMarkdown(markdown)
  if (clean.length <= maxChars) {
    return clean
  }
  return `${clean.slice(0, maxChars).trimEnd()}…`
}

function formatUsd(value: number) {
  return `$${value.toFixed(2)}`
}

function gateDefault(models: ChatModelOption[], defaultModelId?: string | null) {
  return (
    models.find((model) => model.model_id.includes('haiku'))?.model_id ??
    defaultModelId ??
    models[0]?.model_id ??
    ''
  )
}

function App() {
  const [activeView, setActiveView] = useState<ActiveView>(readActiveViewFromLocation)
  const [modelsResponse, setModelsResponse] = useState<ChatModelsResponse | null>(
    null,
  )
  const [selectedChatModelId, setSelectedChatModelId] = useState(
    () => window.localStorage.getItem(CHAT_MODEL_KEY) ?? '',
  )
  const [selectedGateModelId, setSelectedGateModelId] = useState(
    () => window.localStorage.getItem(GATE_MODEL_KEY) ?? '',
  )
  const [memoryAlertsEnabled, setMemoryAlertsEnabled] = useState(
    () => window.localStorage.getItem(MEMORY_ALERTS_ENABLED_KEY) !== 'false',
  )
  const [speakEnabled, setSpeakEnabled] = useState(
    () => window.localStorage.getItem(SPEAK_ENABLED_KEY) !== 'false',
  )
  const [themeMode, setThemeMode] = useState<ThemeMode>(() => {
    const stored = window.localStorage.getItem(THEME_MODE_KEY)
    if (stored === 'light' || stored === 'dark' || stored === 'system') {
      return stored
    }
    return 'system'
  })
  const [systemPrefersDark, setSystemPrefersDark] = useState(() =>
    typeof window !== 'undefined' &&
    'matchMedia' in window &&
    window.matchMedia('(prefers-color-scheme: dark)').matches,
  )
  const [memories, setMemories] = useState<MemoryRecord[]>([])
  const [selectedMemoryId, setSelectedMemoryId] = useState<string | null>(null)
  const [memoryFilter, setMemoryFilter] = useState('')
  const [draftKind, setDraftKind] = useState<MemoryKind>('semantic')
  const [draftMarkdown, setDraftMarkdown] = useState(DEFAULT_MEMORY_TEMPLATE)
  const [chatInput, setChatInput] = useState('')
  const [conversationId, setConversationId] = useState(createConversationId)
  const [turns, setTurns] = useState<ConversationTurn[]>([])
  const [pendingUserMessage, setPendingUserMessage] = useState('')
  const [streamedAnswer, setStreamedAnswer] = useState('')
  const [selectedMemories, setSelectedMemories] = useState<MemoryRecord[]>([])
  const [gateMetrics, setGateMetrics] = useState<LlmCallMetrics | null>(null)
  const [chatMetrics, setChatMetrics] = useState<LlmCallMetrics | null>(null)
  const [runningGateUsd, setRunningGateUsd] = useState(0)
  const [runningChatUsd, setRunningChatUsd] = useState(0)
  const [memoryAlert, setMemoryAlert] = useState<MemoryToolAlert | null>(null)
  const [status, setStatus] = useState('Ready')
  const [loadingMemories, setLoadingMemories] = useState(false)
  const [savingMemory, setSavingMemory] = useState(false)
  const [chatting, setChatting] = useState(false)
  const audioRef = useRef<HTMLAudioElement | null>(null)
  const audioUrlRef = useRef<string | null>(null)

  const models = modelsResponse?.models ?? []
  const resolvedTheme = themeMode === 'system'
    ? systemPrefersDark
      ? 'dark'
      : 'light'
    : themeMode

  const filteredMemories = useMemo(() => {
    const query = memoryFilter.trim().toLowerCase()
    if (!query) {
      return memories
    }
    return memories.filter((memory) =>
      [memory.title, memory.kind, memory.tags.join(' '), memory.content_markdown]
        .join(' ')
        .toLowerCase()
        .includes(query),
    )
  }, [memories, memoryFilter])

  const selectedMemory =
    memories.find((memory) => memory.id === selectedMemoryId) ?? null

  useEffect(() => {
    window.localStorage.setItem(CHAT_MODEL_KEY, selectedChatModelId)
  }, [selectedChatModelId])

  useEffect(() => {
    window.localStorage.setItem(GATE_MODEL_KEY, selectedGateModelId)
  }, [selectedGateModelId])

  useEffect(() => {
    window.localStorage.setItem(
      MEMORY_ALERTS_ENABLED_KEY,
      String(memoryAlertsEnabled),
    )
  }, [memoryAlertsEnabled])

  useEffect(() => {
    window.localStorage.setItem(SPEAK_ENABLED_KEY, String(speakEnabled))
  }, [speakEnabled])

  useEffect(() => {
    window.localStorage.setItem(THEME_MODE_KEY, themeMode)
  }, [themeMode])

  useEffect(() => {
    if (typeof window === 'undefined' || !('matchMedia' in window)) {
      return
    }

    const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)')
    const updatePreference = (event?: MediaQueryListEvent) => {
      setSystemPrefersDark(event?.matches ?? mediaQuery.matches)
    }

    updatePreference()
    mediaQuery.addEventListener('change', updatePreference)
    return () => mediaQuery.removeEventListener('change', updatePreference)
  }, [])

  useEffect(() => {
    document.documentElement.dataset.theme = resolvedTheme
    document.documentElement.style.colorScheme = resolvedTheme
  }, [resolvedTheme])

  useEffect(() => {
    if (memoryAlertsEnabled) {
      return
    }
    setMemoryAlert(null)
  }, [memoryAlertsEnabled])

  useEffect(() => {
    if (!memoryAlert) {
      return
    }

    const timeout = window.setTimeout(() => {
      setMemoryAlert((current) =>
        current?.id === memoryAlert.id ? null : current,
      )
    }, 2600)
    return () => window.clearTimeout(timeout)
  }, [memoryAlert])

  useEffect(() => {
    return () => {
      stopSpeech()
    }
  }, [])

  useEffect(() => {
    void loadModels()
    void loadMemories()
  }, [])

  useEffect(() => {
    const handlePopState = () => {
      setActiveView(readActiveViewFromLocation())
    }

    writeActiveViewToLocation(readActiveViewFromLocation(), true)
    window.addEventListener('popstate', handlePopState)
    return () => window.removeEventListener('popstate', handlePopState)
  }, [])

  useEffect(() => {
    if (!models.length) {
      return
    }
    if (!selectedChatModelId) {
      setSelectedChatModelId(
        modelsResponse?.default_model_id ?? models[0]?.model_id ?? '',
      )
    }
    if (!selectedGateModelId) {
      setSelectedGateModelId(gateDefault(models, modelsResponse?.default_model_id))
    }
  }, [models, modelsResponse, selectedChatModelId, selectedGateModelId])

  function selectMemory(memory: MemoryRecord) {
    setSelectedMemoryId(memory.id)
    setDraftKind(memory.kind)
    setDraftMarkdown(memory.content_markdown)
  }

  function resetMemoryEditor() {
    setSelectedMemoryId(null)
    setDraftKind('semantic')
    setDraftMarkdown(DEFAULT_MEMORY_TEMPLATE)
  }

  function stopSpeech() {
    audioRef.current?.pause()
    audioRef.current = null
    if (audioUrlRef.current) {
      window.URL.revokeObjectURL(audioUrlRef.current)
      audioUrlRef.current = null
    }
  }

  function showMemoryToolAlert(createdCount: number) {
    if (!memoryAlertsEnabled) {
      return
    }

    const message =
      createdCount > 1
        ? `Saved ${createdCount} memories.`
        : createdCount === 1
          ? 'Saved 1 memory.'
          : 'Conversation reviewed for memory.'

    setMemoryAlert({
      id: Date.now(),
      message,
    })
  }

  async function speakText(text: string) {
    if (!speakEnabled || !text.trim()) {
      return
    }

    stopSpeech()
    try {
      const response = await fetch('/v1/speak', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ text }),
      })
      if (!response.ok) {
        return
      }
      const audioBlob = await response.blob()
      if (!audioBlob.size) {
        return
      }

      const audioUrl = window.URL.createObjectURL(audioBlob)
      const audio = new Audio(audioUrl)
      audioRef.current = audio
      audioUrlRef.current = audioUrl
      const cleanup = () => {
        if (audioUrlRef.current === audioUrl) {
          window.URL.revokeObjectURL(audioUrl)
          audioUrlRef.current = null
        }
        if (audioRef.current === audio) {
          audioRef.current = null
        }
      }
      audio.onended = cleanup
      audio.onerror = cleanup
      await audio.play()
    } catch {
      stopSpeech()
    }
  }

  async function apiRequest<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await fetch(path, {
      cache: init?.cache ?? 'no-store',
      credentials: 'same-origin',
      ...init,
      headers: {
        ...(init?.body ? { 'Content-Type': 'application/json' } : {}),
        ...(init?.headers ?? {}),
      },
    })
    const text = await response.text()
    if (!response.ok) {
      try {
        const parsed = JSON.parse(text) as { error?: string }
        throw new Error(parsed.error || `Request failed with ${response.status}`)
      } catch (error) {
        throw error instanceof Error
          ? error
          : new Error(`Request failed with ${response.status}`)
      }
    }
    if (!text) {
      return undefined as T
    }
    return JSON.parse(text) as T
  }

  async function loadModels() {
    try {
      const response = await apiRequest<ChatModelsResponse>('/v1/chat/models')
      setModelsResponse(response)
    } catch (error) {
      setStatus(error instanceof Error ? error.message : 'Failed to load models.')
    }
  }

  async function loadMemories(preferredId?: string | null) {
    setLoadingMemories(true)
    try {
      const nextMemories = (await apiRequest<MemoryRecord[]>('/v1/memories')).filter(
        (memory) => memory.state !== 'deleted',
      )
      setMemories(nextMemories)

      if (preferredId === null) {
        setStatus(`${nextMemories.length} mem`)
        return
      }

      const nextSelectedId =
        preferredId ??
        (selectedMemoryId && nextMemories.some((memory) => memory.id === selectedMemoryId)
          ? selectedMemoryId
          : nextMemories[0]?.id)

      if (nextSelectedId) {
        const nextSelected = nextMemories.find(
          (memory) => memory.id === nextSelectedId,
        )
        if (nextSelected) {
          selectMemory(nextSelected)
        }
      }
      setStatus(`${nextMemories.length} mem`)
    } catch (error) {
      setStatus(error instanceof Error ? error.message : 'Failed to load memories.')
    } finally {
      setLoadingMemories(false)
    }
  }

  async function saveMemory() {
    if (!draftMarkdown.trim()) {
      setStatus('Memory markdown cannot be empty')
      return
    }
    setSavingMemory(true)
    try {
      if (selectedMemory) {
        const updated = await apiRequest<MemoryRecord>(
          `/v1/memories/${selectedMemory.id}`,
          {
            method: 'PATCH',
            body: JSON.stringify({ content_markdown: draftMarkdown }),
          },
        )
        await loadMemories(updated.id)
        setStatus('Saved')
      } else {
        const response = await apiRequest<CaptureEntryResponse>('/v1/memories', {
          method: 'POST',
          body: JSON.stringify({
            content_markdown: draftMarkdown,
            kind: draftKind,
            source_app: 'web',
          }),
        })
        const created = response.memories[0]
        await loadMemories(created?.id ?? undefined)
        if (created) {
          selectMemory(created)
        }
        setStatus('Created')
      }
    } catch (error) {
      setStatus(error instanceof Error ? error.message : 'Failed to save memory.')
    } finally {
      setSavingMemory(false)
    }
  }

  async function deleteMemory() {
    if (!selectedMemory) {
      return
    }
    if (!window.confirm(`Delete "${selectedMemory.title}"?`)) {
      return
    }
    const deletedId = selectedMemory.id
    setSavingMemory(true)
    try {
      await apiRequest<MemoryRecord>(`/v1/memories/${deletedId}`, {
        method: 'DELETE',
      })
      resetMemoryEditor()
      setMemories((current) => current.filter((memory) => memory.id !== deletedId))
      await loadMemories(null)
      setStatus('Deleted')
    } catch (error) {
      setStatus(
        error instanceof Error ? error.message : 'Failed to delete memory.',
      )
    } finally {
      setSavingMemory(false)
    }
  }

  async function sendChat() {
    const message = chatInput.trim()
    if (!message || chatting) {
      return
    }
    stopSpeech()
    setChatting(true)
    setPendingUserMessage(message)
    setStreamedAnswer('')
    setGateMetrics(null)
    setChatMetrics(null)
    setSelectedMemories([])
    setStatus('Streaming')

    try {
      const response = await fetch('/v1/chat/respond/stream', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          message,
          model_id: selectedChatModelId || undefined,
          gate_model_id: selectedGateModelId || undefined,
          conversation_id: conversationId,
        }),
      })

      if (!response.ok || !response.body) {
        const text = await response.text()
        try {
          const parsed = JSON.parse(text) as { error?: string }
          throw new Error(parsed.error || `Request failed with ${response.status}`)
        } catch (error) {
          throw error instanceof Error
            ? error
            : new Error(`Request failed with ${response.status}`)
        }
      }

      const reader = response.body.getReader()
      const decoder = new TextDecoder()
      let buffer = ''
      let finalAnswer = ''

      while (true) {
        const { done, value } = await reader.read()
        if (done) {
          break
        }
        buffer += decoder.decode(value, { stream: true })
        const lines = buffer.split('\n')
        buffer = lines.pop() ?? ''

        for (const line of lines) {
          if (!line.trim()) {
            continue
          }
          const event = JSON.parse(line) as ChatStreamEvent
          if (event.type === 'start') {
            setSelectedMemories(event.selected_memories)
            setGateMetrics(event.gate_metrics ?? null)
            if (event.remember_current_conversation_used) {
              showMemoryToolAlert(event.remembered_memories_count ?? 0)
            }
            setRunningGateUsd((current) =>
              current + (event.gate_metrics?.cost.total_usd ?? 0),
            )
          } else if (event.type === 'delta') {
            finalAnswer += event.delta
            setStreamedAnswer(finalAnswer)
          } else if (event.type === 'done') {
            finalAnswer = event.answer
            setChatMetrics(event.chat_metrics ?? null)
            setRunningChatUsd((current) =>
              current + (event.chat_metrics?.cost.total_usd ?? 0),
            )
          } else if (event.type === 'error') {
            throw new Error(event.error)
          }
        }
      }

      setTurns((current) => [
        ...current,
        { role: 'user', text: message },
        { role: 'assistant', text: finalAnswer },
      ])
      setChatInput('')
      setPendingUserMessage('')
      setStreamedAnswer('')
      setStatus('Done')
      void speakText(finalAnswer)
      await loadMemories(undefined)
    } catch (error) {
      setStatus(error instanceof Error ? error.message : 'Chat failed.')
    } finally {
      setChatting(false)
      setPendingUserMessage('')
      setStreamedAnswer('')
    }
  }

  function handleChatInputKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key !== 'Enter' || !event.metaKey || event.shiftKey || event.altKey || event.ctrlKey) {
      return
    }

    event.preventDefault()
    void sendChat()
  }

  function startNewMemory() {
    resetMemoryEditor()
  }

  function startNewConversation() {
    stopSpeech()
    setConversationId(createConversationId())
    setTurns([])
    setPendingUserMessage('')
    setStreamedAnswer('')
    setSelectedMemories([])
    setGateMetrics(null)
    setChatMetrics(null)
    setRunningGateUsd(0)
    setRunningChatUsd(0)
    setStatus('New conversation')
  }

  function selectView(view: ActiveView) {
    setActiveView(view)
    writeActiveViewToLocation(view)
  }

  return (
    <div className="shell">
      <header className="topbar">
        <div className="brand">Ancilla</div>
        <nav className="view-switch" aria-label="View">
          <button
            type="button"
            className={activeView === 'chat' ? 'is-active' : ''}
            onClick={() => selectView('chat')}
          >
            Chat
          </button>
          <button
            type="button"
            className={activeView === 'memories' ? 'is-active' : ''}
            onClick={() => selectView('memories')}
          >
            Memories
          </button>
        </nav>
        <div className="topbar-spacer" />
        <select
          className="theme-select"
          aria-label="Theme"
          value={themeMode}
          onChange={(event) => setThemeMode(event.target.value as ThemeMode)}
        >
          <option value="system">System</option>
          <option value="light">Light</option>
          <option value="dark">Dark</option>
        </select>
        <div className={`status-box ${activeView === 'chat' ? 'is-chat' : 'is-memories'}`}>
          {activeView === 'chat' ? (
            <span className="status-total">Total {formatUsd(runningGateUsd + runningChatUsd)}</span>
          ) : (
            <span className="status-text">{status}</span>
          )}
        </div>
      </header>

      {activeView === 'chat' ? (
        <main className="panel chat-panel">
          <div className="chat-toolbar">
            <div className="selectors">
              <label>
                Chat
                <select
                  value={selectedChatModelId}
                  onChange={(event) => setSelectedChatModelId(event.target.value)}
                >
                  {models.map((model) => (
                    <option key={model.model_id} value={model.model_id}>
                      {model.label}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                Gate
                <select
                  value={selectedGateModelId}
                  onChange={(event) => setSelectedGateModelId(event.target.value)}
                >
                  {models.map((model) => (
                    <option key={`gate-${model.model_id}`} value={model.model_id}>
                      {model.label}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <div className="memory-actions">
              <button
                type="button"
                className="button button-secondary"
                onClick={() => {
                  if (speakEnabled) {
                    stopSpeech()
                  }
                  setSpeakEnabled((current) => !current)
                }}
              >
                {speakEnabled ? 'Voice On' : 'Voice Off'}
              </button>
              <button
                type="button"
                className="button button-secondary"
                onClick={startNewConversation}
              >
                New
              </button>
            </div>
          </div>

          <div className="transcript">
            {turns.map((turn, index) => (
              <article key={`${turn.role}-${index}`} className={`bubble ${turn.role}`}>
                <span>{turn.role}</span>
                <p>{turn.text}</p>
              </article>
            ))}
            {pendingUserMessage && (
              <article className="bubble user">
                <span>user</span>
                <p>{pendingUserMessage}</p>
              </article>
            )}
            {streamedAnswer && (
              <article className="bubble assistant">
                <span>assistant</span>
                <p>{streamedAnswer}</p>
              </article>
            )}
            {!turns.length && !pendingUserMessage && !streamedAnswer && (
              <div className="empty-state">No messages yet.</div>
            )}
          </div>

          <div className="chat-composer">
            <textarea
              value={chatInput}
              onChange={(event) => setChatInput(event.target.value)}
              onKeyDown={handleChatInputKeyDown}
              placeholder="Ask something"
              disabled={chatting}
            />
            <button
              type="button"
              className="button"
              onClick={() => void sendChat()}
              disabled={chatting || !chatInput.trim()}
            >
              {chatting ? 'Streaming…' : 'Send'}
            </button>
          </div>

          {(selectedMemories.length > 0 || gateMetrics || chatMetrics) && (
            <section className="recall-strip">
              {selectedMemories.length > 0 && (
                <div className="recall-list">
                  {selectedMemories.map((memory) => (
                    <article key={`selected-${memory.id}`} className="recall-card">
                      <strong>{memory.title}</strong>
                      <p>{excerpt(memory.content_markdown, 140)}</p>
                    </article>
                  ))}
                </div>
              )}
              <div className="metric-strip">
                {gateMetrics && (
                  <span>
                    Gate {gateMetrics.usage.total_tokens.toLocaleString()} tok ·{' '}
                    {formatUsd(gateMetrics.cost.total_usd)}
                  </span>
                )}
                {chatMetrics && (
                  <span>
                    Chat {chatMetrics.usage.total_tokens.toLocaleString()} tok ·{' '}
                    {formatUsd(chatMetrics.cost.total_usd)}
                  </span>
                )}
              </div>
            </section>
          )}
        </main>
      ) : (
        <main className="panel memories-panel">
          <div className="memory-toolbar">
            <input
              className="filter-input"
              value={memoryFilter}
              onChange={(event) => setMemoryFilter(event.target.value)}
              placeholder="Filter"
            />
            <div className="memory-actions">
              <label className="toggle-setting">
                <input
                  type="checkbox"
                  checked={memoryAlertsEnabled}
                  onChange={(event) =>
                    setMemoryAlertsEnabled(event.target.checked)
                  }
                />
                <span>Corner alerts</span>
              </label>
              <button
                type="button"
                className="button button-secondary"
                onClick={startNewMemory}
              >
                New
              </button>
              <button
                type="button"
                className="button button-secondary"
                onClick={() => void loadMemories()}
                disabled={loadingMemories}
              >
                {loadingMemories ? 'Refreshing…' : 'Refresh'}
              </button>
            </div>
          </div>

          <div className="memory-layout">
            <aside className="memory-list">
              {filteredMemories.map((memory) => (
                <button
                  key={memory.id}
                  type="button"
                  className={`memory-card ${selectedMemoryId === memory.id ? 'is-selected' : ''}`}
                  onClick={() => selectMemory(memory)}
                >
                  <div className="memory-card-head">
                    <strong>{memory.title}</strong>
                    <span>{memory.kind}</span>
                  </div>
                  <p>{excerpt(memory.content_markdown)}</p>
                  <div className="tag-row">
                    {memory.tags.map((tag) => (
                      <span key={`${memory.id}-${tag}`} className="tag">
                        {tag}
                      </span>
                    ))}
                  </div>
                </button>
              ))}
              {!filteredMemories.length && (
                <div className="empty-state">No memories.</div>
              )}
            </aside>

            <section className="editor-pane">
              <div className="editor-toolbar">
                <div className="selectors">
                  <label className="memory-kind-field">
                    Kind
                    <select
                      value={draftKind}
                      onChange={(event) =>
                        setDraftKind(event.target.value as MemoryKind)
                      }
                      disabled={Boolean(selectedMemory)}
                    >
                      <option value="semantic">Semantic</option>
                      <option value="episodic">Episodic</option>
                      <option value="procedural">Procedural</option>
                    </select>
                  </label>
                </div>
                <div className="memory-actions">
                  {selectedMemory && (
                    <button
                      type="button"
                      className="button button-danger"
                      onClick={() => void deleteMemory()}
                      disabled={savingMemory}
                    >
                      Delete
                    </button>
                  )}
                  <button
                    type="button"
                    className="button"
                    onClick={() => void saveMemory()}
                    disabled={savingMemory}
                  >
                    {savingMemory ? 'Saving…' : selectedMemory ? 'Save' : 'Create'}
                  </button>
                </div>
              </div>

              <textarea
                className="editor"
                value={draftMarkdown}
                onChange={(event) => setDraftMarkdown(event.target.value)}
                spellCheck={false}
              />
            </section>
          </div>
        </main>
      )}

      {memoryAlert && (
        <div className="memory-alert-stack" aria-live="polite" aria-atomic="true">
          <div className="memory-alert">{memoryAlert.message}</div>
        </div>
      )}
    </div>
  )
}

export default App
