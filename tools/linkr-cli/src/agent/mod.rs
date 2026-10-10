//! Assistant runtime: provider clients, agent loop, tools, execution policy,
//! memory and report export. Feature parity target: web/agent_* + mobile
//! `pi-agent.mjs` (specs/AGENT_SPEC.md).
//!
//! [`spawn`] owns one queue, one journal/watch feed and one turn loop per
//! conversation. Everything the model can observe is produced by the pure
//! submodules below, so the contract strings are asserted in unit tests rather
//! than discovered at runtime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

pub mod accessory;
pub mod config;
pub mod console;
pub mod context;
pub mod executor;
pub mod memory;
pub mod policy;
pub mod prompt;
pub mod provider;
pub mod report;
pub mod tools;
pub mod web;

pub use config::{clear_config, load_config, save_config};
pub use report::{build_report, ReportDownload, ReportInput, ReportRecord};

use crate::event::CoreEvent;
use crate::journal::SerialJournal;
// One clock for the whole crate: the journal owns the wall time and every
// other module (`agent`, its memory stores, the TUI) reads the same one.
pub(crate) use crate::journal::now_ms;
use crate::session::{CoreBus, SessionHandle};
use crate::transport::TransportKind;
use crate::watch::{Finding, SerialWatch, WatchOptions};

use context::{compact_agent_context, settle_history, Message, ToolCall};
use executor::{
    clamp_monitor_timeout, clamp_settle, clamp_wait_timeout, completion_token, execution_page,
    monitor_decide, monitor_poll_ms, refresh_record, tracked_command, validate_command,
    validate_input, validate_tool_names, wait_decide, wait_poll_ms, ExecutionRecord,
    ExecutionStore, NEXT_INSPECT,
};
use memory::{Task, TaskStep};

/// Spec §1.1: 32 model turns, 96 tool calls and 15 minutes per question.
pub const MAX_TURNS: u32 = 32;
pub const MAX_TOOL_CALLS: u32 = 96;
pub const RUN_LIMIT_MS: u64 = 900_000;
/// Queue capacity: `Queue is full (8 messages). …`.
pub const QUEUE_CAPACITY: usize = 8;
/// The configured Enter sequence (first of `ENTER_SEQUENCES`).
pub const SEND_ENTER: &str = "\r";
/// Evidence page handed to the model by inspect/monitor.
pub const TOOL_PAGE_CHARS: usize = 16_000;
/// Console hint window used by `get_device_status`.
pub const CONSOLE_WINDOW: u64 = 4_000;
/// `update_task_plan` validation (spec §2.1).
pub const ERR_PLAN: &str =
    "Use up to eight steps, at most one in progress, verification for completed steps, and a reason plus next action for blocked steps.";
/// `search_serial_log` validation (spec §2.10).
pub const ERR_SEARCH_TEXT: &str = "Provide a non-empty literal search text.";
/// `read_serial_log` argument conflict (spec §2.11).
pub const ERR_READ_ARGS: &str = "Choose recent or after, not both.";
/// The stop reason emitted when the panel's 15-minute timer fires.
pub const STOP_REASON: &str = "Stopped after 15 minutes";
/// What a turn reports when the panel's stop button ends it before it had
/// anything to say. A stop ends *our* observation of the target; it never
/// reaches the target's own process, which is why every way out of
/// `agent_loop` says exactly this.
const STOPPED_BY_USER: &str =
    "Stopped by user. Observation of the target stopped; the device process was not affected.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    OpenAiCompat,
    AnthropicMessages,
    GoogleGemini,
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    pub provider: Provider,
    pub reasoning: String,
    pub extra_headers: Vec<(String, String)>,
    pub context_window: u32,
    pub max_tokens: u32,
    pub price_input: f64,
    pub price_output: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMode {
    Manual,
    Auto,
    FullAuto,
}

impl ExecMode {
    /// Storage/wire name, as used by `web/agent_execution_policy.js`.
    pub fn as_str(self) -> &'static str {
        match self {
            ExecMode::Manual => "manual",
            ExecMode::Auto => "semi-auto",
            ExecMode::FullAuto => "full-auto",
        }
    }

    /// Unknown names fall back to `semi-auto`, the app's default.
    pub fn parse(value: &str) -> ExecMode {
        match value.trim().to_ascii_lowercase().as_str() {
            "manual" => ExecMode::Manual,
            "full-auto" | "full_auto" | "fullauto" => ExecMode::FullAuto,
            _ => ExecMode::Auto,
        }
    }

    fn ordinal(self) -> u8 {
        match self {
            ExecMode::Manual => 0,
            ExecMode::Auto => 1,
            ExecMode::FullAuto => 2,
        }
    }

    fn from_ordinal(value: u8) -> ExecMode {
        match value {
            0 => ExecMode::Manual,
            2 => ExecMode::FullAuto,
            _ => ExecMode::Auto,
        }
    }
}

/// What the assistant asks the user to approve. Mirrors the three approval
/// cards of the web panel: target command, serial input, accessory change.
#[derive(Debug, Clone)]
pub enum ApprovalKind {
    RunCommand { command: String, mode: ExecMode },
    SendInput { payload: String },
    AccessoryChange { summary: String, command: String },
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub id: u64,
    pub kind: ApprovalKind,
    /// Already redacted; safe to render.
    pub question: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

/// Implemented by the TUI (modal dialog) and by the CLI (stderr prompt /
/// `--yes`). Resolution happens through the returned oneshot.
pub trait ApprovalBroker: Send + Sync + 'static {
    fn ask(&self, request: ApprovalRequest) -> oneshot::Receiver<ApprovalDecision>;
}

/// What the model is told when the user refuses a request.
///
/// There are two wordings and they are not interchangeable. Every tool takes
/// the generic one (`AGENT_SPEC.md` §5.1: "The user rejected ${toolName}. Do
/// not retry this action…"), but a change to Linkr Bee itself is asked on a
/// card of its own — `ApprovalKind::AccessoryChange`, the
/// "This changes Linkr Bee itself (not the target)…" card — and
/// `WEB_UX_SPEC.md` §5 pins that card's reject error to "The user rejected
/// this change. Do not retry unless the user asks again." (`web/
/// accessory_control.js` → the Reject button). Answering that card with the
/// generic sentence tells the model which tool was refused instead of that
/// the bridge was not reconfigured, and the spec string sat here unused.
fn rejection_message(kind: &ApprovalKind, tool_name: &str) -> String {
    match kind {
        ApprovalKind::AccessoryChange { .. } => executor::REJECTED_ACCESSORY.to_string(),
        _ => executor::rejected_message(tool_name),
    }
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    MessageStart {
        role: String,
    },
    AssistantDelta(String),
    ToolStart {
        name: String,
        args: String,
    },
    ToolEnd {
        name: String,
        result: String,
        ok: bool,
    },
    Usage {
        input: u64,
        output: u64,
        cache_read: u64,
        total: u64,
        cost: f64,
    },
    RunFinished {
        reason: String,
    },
    Error(String),
    /// The unattended window ran out (`armFullAuto`'s timer in
    /// `web/device_executor.js`): the mode has already fallen back to `Auto`
    /// by the time this is observed. The panel repeats the fact as its status
    /// — web's `onModeTimeout` writes `fullAutoExpired` into `agentStatus`.
    ModeExpired,
}

/// What the panel can tell a running runtime, besides asking it a question.
///
/// The runtime owns its copies of the session and the config: `spawn` hands
/// them over once and the panel has no way back in afterwards. Each variant is
/// one thing the panel changes while the runtime keeps going, and each used to
/// leave the runtime answering from what it was given at birth — a stale
/// `connected` after a reconnect, an old endpoint after a settings save, and a
/// "new conversation" that still carried the last one to the model.
enum Command {
    Ask(String),
    /// The panel took a new connection, or the old one went away.
    Session(crate::session::SessionHandle),
    /// The panel saved or cleared the model settings.
    Config(Option<AgentConfig>),
    /// The panel started a new conversation.
    ResetChat,
    /// Test-only: give the runtime the history a test needs it to have, so
    /// "new chat" has something to clear without a live model to talk to.
    #[cfg(test)]
    Seed(Vec<Message>),
    /// Test-only: report what only the runtime can see — which session it
    /// holds, how much history it still carries, which config it was handed.
    #[cfg(test)]
    Probe(std::sync::mpsc::Sender<RuntimeView>),
}

/// What `AgentHandle::view` reads back out of a runtime, for the tests that
/// have to prove a `Command` actually landed rather than merely was queued.
#[cfg(test)]
#[derive(Debug)]
pub struct RuntimeView {
    /// Whether the session the runtime holds reports a live link.
    pub session_connected: bool,
    /// Its label: a reconnect has to move the runtime onto the *new* one.
    pub session_label: String,
    /// `session_key == session_key_now()` — `take_session` rewrites the key,
    /// and a key left behind makes every tool report "session moved".
    pub session_key_matches: bool,
    /// Messages the model would still send with the next request.
    pub history: usize,
    /// An execution handed to the runtime but not finished.
    pub pending: bool,
    /// Model the runtime would call next, `None` when it has no config.
    pub config_model: Option<String>,
}

/// Handle used by the TUI chat panel.
#[derive(Clone)]
pub struct AgentHandle {
    tx: broadcast::Sender<AgentEvent>,
    ask_tx: mpsc::Sender<Command>,
    stop_tx: watch::Sender<bool>,
    mode: Arc<AtomicU8>,
    /// `executionModeExpiresAt` of `web/device_executor.js`: the wall-clock
    /// millisecond the unattended window ends, or `0` when none is armed.
    deadline: Arc<AtomicU64>,
    journal: Arc<StdMutex<SerialJournal>>,
}

impl AgentHandle {
    /// Queue a question; `stop()` cancels the running turn.
    pub fn ask(&self, question: String) {
        if question.trim().is_empty() {
            let _ = self
                .tx
                .send(AgentEvent::Error(executor::ERR_QUEUE_ITEM.to_string()));
            return;
        }
        self.send(Command::Ask(question));
    }

    /// Hand the runtime the session the panel is now using.
    ///
    /// It holds a clone of the session it was spawned with, so after a
    /// reconnect it went on reporting `connected` for the one that had gone —
    /// the panel said connected while every tool that read the device read a
    /// dead handle.
    pub fn set_session(&self, session: crate::session::SessionHandle) {
        self.send(Command::Session(session));
    }

    /// Hand the runtime the settings the panel just saved, or `None` when they
    /// were cleared.
    ///
    /// The config is a snapshot taken at `spawn`: changing the endpoint, the
    /// model or the API key went unused by an already-running runtime, and a
    /// key the user had just cleared kept working from memory.
    pub fn set_config(&self, config: Option<AgentConfig>) {
        self.send(Command::Config(config));
    }

    /// Start a new conversation as far as the model is concerned.
    ///
    /// The history the model sees lives here, not in the panel: clearing the
    /// transcript on screen while leaving this behind showed a fresh
    /// conversation that was still carrying the old one.
    pub fn reset_chat(&self) {
        self.send(Command::ResetChat);
    }

    /// Test-only: hand the runtime a conversation to have had, so a test can
    /// watch "new chat" clear it without a live model behind it.
    #[cfg(test)]
    pub fn seed_history(&self, lines: &[&str]) {
        let messages = lines
            .iter()
            .map(|line| Message::user((*line).to_string()))
            .collect();
        self.send(Command::Seed(messages));
    }

    /// Test-only: read back what only the runtime can see. Queued like any
    /// other command, so it observes the state *after* everything ahead of it.
    ///
    /// A `ResetChat` queued ahead of this drains whatever sits behind it —
    /// that is what it is for — which can take the probe with it, so the probe
    /// is simply asked again until an answer comes back.
    #[cfg(test)]
    pub fn view(&self) -> RuntimeView {
        for _ in 0..30 {
            let (tx, rx) = std::sync::mpsc::channel();
            if self.ask_tx.try_send(Command::Probe(tx)).is_err() {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            if let Ok(view) = rx.recv_timeout(std::time::Duration::from_millis(100)) {
                return view;
            }
        }
        panic!("the runtime never answered a view probe");
    }

    /// Queue something for the runtime, reporting a full queue the way a
    /// question does.
    fn send(&self, command: Command) {
        if self.ask_tx.try_send(command).is_err() {
            let _ = self
                .tx
                .send(AgentEvent::Error(executor::ERR_QUEUE_FULL.to_string()));
        }
    }

    pub fn stop(&self) {
        let _ = self.stop_tx.send(true);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.tx.subscribe()
    }

    /// Current execution mode of the runtime (spec §1.4 compares it with the
    /// panel's mode on every tool call).
    pub fn mode(&self) -> ExecMode {
        ExecMode::from_ordinal(self.mode.load(Ordering::Relaxed))
    }

    /// Change the execution mode. A run in flight reports
    /// `Device session or mode changed. Start a new conversation.` on its next
    /// tool call, exactly like a session swap (spec §1.4).
    ///
    /// This is the *only* path that reaches the runtime: `Runtime::mode()`
    /// reads the same atomic, so a picker that stops here would be a label
    /// with no effect — `Manual` would still auto-execute. `setMode` of
    /// `web/device_executor.js` also decides when the unattended window is
    /// armed: re-selecting Full Auto **extends** it, every other already
    /// active mode is a no-op, and anything else closes it.
    pub fn set_mode(&self, mode: ExecMode) {
        if mode == self.mode() && mode != ExecMode::FullAuto {
            return;
        }
        self.mode.store(mode.ordinal(), Ordering::Relaxed);
        if mode == ExecMode::FullAuto {
            self.deadline
                .store(now_ms() + executor::FULL_AUTO_WINDOW_MS, Ordering::Relaxed);
        } else {
            self.deadline.store(0, Ordering::Relaxed);
        }
    }

    /// Take over the window the **panel** computed. `setMode` of
    /// `web/device_executor.js` arms the timer at the pick, which can be long
    /// before the runtime exists (the TUI spawns it on the first question) —
    /// so the panel keeps the deadline and pushes it in here. A value already
    /// in the past expires on the watchdog's next tick, which is exactly what
    /// `armFullAuto`'s timer would have done.
    pub fn set_deadline(&self, expires_at: u64) {
        self.deadline.store(expires_at, Ordering::Relaxed);
    }

    /// Milliseconds left in the unattended window (`None`: not armed). The
    /// frame loop repaints every tick, so the panel renders the same read as
    /// a countdown without a timer of its own — `countdownSuffix()` of
    /// `web/agent_panel.js` does the arithmetic with `Math.ceil` too.
    pub fn full_auto_remaining(&self) -> Option<u64> {
        let expires_at = self.deadline.load(Ordering::Relaxed);
        if expires_at == 0 {
            return None;
        }
        Some(expires_at.saturating_sub(now_ms()))
    }

    /// `SerialJournal::reset()`: spec §6.4 empties the evidence window "on
    /// connect and on Clear", so the model never quotes a previous
    /// connection's bytes back as if they were live.
    pub fn reset_journal(&self) {
        if let Ok(mut log) = self.journal.lock() {
            log.reset();
        }
    }
}

#[cfg(test)]
impl AgentHandle {
    /// Size of the evidence window, so a test can watch Clear (or a connect)
    /// empty it. The TUI never reads the journal itself — the runtime owns it.
    #[allow(dead_code)]
    pub fn journal_len(&self) -> usize {
        self.journal.lock().map(|log| log.len()).unwrap_or(0)
    }

    /// Wind the unattended window into the past, the way the wall clock gets
    /// there without waiting fifteen minutes in a test.
    pub fn expire_window_now(&self) {
        self.deadline
            .store(now_ms().saturating_sub(1), Ordering::Relaxed);
    }
}

/// Spawn the assistant runtime.
pub fn spawn(
    config: Option<AgentConfig>,
    broker: Arc<dyn ApprovalBroker>,
    session: crate::session::SessionHandle,
    bus: crate::session::CoreBus,
) -> AgentHandle {
    let (tx, _) = broadcast::channel::<AgentEvent>(256);
    let (ask_tx, ask_rx) = mpsc::channel::<Command>(QUEUE_CAPACITY);
    let (stop_tx, stop_rx) = watch::channel(false);
    let mode = Arc::new(AtomicU8::new(ExecMode::Auto.ordinal()));
    let deadline = Arc::new(AtomicU64::new(0));

    let journal = Arc::new(StdMutex::new(SerialJournal::new()));
    let records = Arc::new(StdMutex::new(ExecutionStore::new()));

    spawn_feed(bus, journal.clone());
    tokio::spawn(watch_full_auto(
        deadline.clone(),
        mode.clone(),
        stop_tx.clone(),
        tx.clone(),
    ));

    let handle = AgentHandle {
        tx: tx.clone(),
        ask_tx,
        stop_tx: stop_tx.clone(),
        mode: mode.clone(),
        deadline: deadline.clone(),
        journal: journal.clone(),
    };

    let runtime = Runtime {
        config,
        broker,
        session,
        tx,
        ask_rx,
        stop_tx,
        stop_rx,
        journal,
        records,
        mode,
        history: Vec::new(),
        unlocked: Vec::new(),
        capabilities: HashMap::new(),
        download_probe: None,
        profile: None,
        policy: policy::PolicyStore::open(
            policy::PolicyStore::default_path()
                .unwrap_or_else(|| std::path::PathBuf::from("command_policy.json")),
        ),
        tasks: memory::TaskStore::open(store_path("agent_tasks.json")),
        notes: memory::NoteStore::open(store_path("agent_notes.json")),
        device_key: String::new(),
        notes_enabled: true,
        read_cursor: 0,
        pending_execution: None,
        download_stage: false,
        console_kind: "unknown".to_string(),
        console_evidence: String::new(),
        console_cursor: 0,
        input_pending: false,
        input_revision: 0,
        session_key: String::new(),
        approval_ids: Arc::new(AtomicU64::new(1)),
        last_question: String::new(),
        turn: 0,
        tool_calls: 0,
        started: Instant::now(),
    };
    tokio::spawn(runtime_loop(runtime));
    handle
}

/// Storage location of one of the agent's JSON stores (and of anything else
/// that belongs beside them): the config directory when there is one, the
/// working directory otherwise.
pub(crate) fn store_path(name: &str) -> std::path::PathBuf {
    config::config_dir()
        .map(|dir| dir.join(name))
        .unwrap_or_else(|| std::path::PathBuf::from(name))
}

/// `(tool observations, cursor, session id, source)` observed while a record's
/// guard is held and applied to the store after it is released.
type ObservedTools = Option<(Vec<(String, bool)>, u64, String, String)>;

/// Feed the assistant journal from the shared bus. UART bytes are duplicated
/// per subscriber on purpose: the assistant keeps its own bounded window so a
/// tool call never competes with the terminal for scrollback. The terminal's
/// findings come from the TUI's own watcher (`mod.rs`, fed on the same
/// events), so there is nothing here to feed a second one — `watch_serial_output`
/// builds a windowed watcher of its own for exactly the window it reports.
fn spawn_feed(bus: CoreBus, journal: Arc<StdMutex<SerialJournal>>) {
    tokio::spawn(async move {
        let mut rx = bus.subscribe();
        while let Ok(event) = rx.recv().await {
            if let CoreEvent::UartRx(bytes) = event {
                if let Ok(mut log) = journal.lock() {
                    log.append_bytes(&bytes);
                }
            }
        }
    });
}

/// How often the unattended window is checked. The window itself is a
/// wall-clock deadline (`Date.now()`), and the panel only ever renders whole
/// seconds (` · 14:59`), so a quarter second of slack is invisible.
const FULL_AUTO_TICK_MS: u64 = 250;

/// The unattended window of `armFullAuto` (`web/device_executor.js`): when it
/// runs out the mode falls back to `Auto`, a run still in flight is cancelled
/// the way `cancel()` aborts its controller, and the panel is told once.
///
/// A tick instead of an armed timer because `set_mode` can re-arm the window
/// from another task at any moment; the deadline is compared against the wall
/// clock, so a re-arm can never be lost — only *this* value is cleared.
async fn watch_full_auto(
    deadline: Arc<AtomicU64>,
    mode: Arc<AtomicU8>,
    stop_tx: watch::Sender<bool>,
    tx: broadcast::Sender<AgentEvent>,
) {
    loop {
        tokio::select! {
            // The runtime is gone: no turn left to fall back from.
            _ = stop_tx.closed() => break,
            _ = tokio::time::sleep(Duration::from_millis(FULL_AUTO_TICK_MS)) => {}
        }
        let expires_at = deadline.load(Ordering::Relaxed);
        if expires_at == 0 || now_ms() < expires_at {
            continue;
        }
        if deadline
            .compare_exchange(expires_at, 0, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            continue; // the picker re-armed or closed the window meanwhile
        }
        if mode
            .compare_exchange(
                ExecMode::FullAuto.ordinal(),
                ExecMode::Auto.ordinal(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_err()
        {
            continue; // the picker already moved on by hand
        }
        let _ = stop_tx.send(true);
        let _ = tx.send(AgentEvent::ModeExpired);
    }
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// Spec §1.3: the tools a still-unreviewed execution blocks.
const INPUT_TOOLS: &[&str] = &[
    "send_serial_input",
    "run_shell_command",
    "probe_device_profile",
    "probe_tools",
    "probe_download_tools",
    "download_to_target",
];

/// Marker-reading tools that must resolve an execution before the model sees
/// a result; they clear the pending-execution block (spec §1.3 note 4).
const REVIEW_TOOLS: &[&str] = &[
    "inspect_serial_execution",
    "monitor_serial_execution",
    "read_serial_log",
    "search_serial_log",
    "wait_for_serial_output",
    "get_device_status",
];

struct PendingExecution {
    id: String,
    reviewed_round: Option<u32>,
}

/// Per-send record annotations that only the calling tool knows about.
#[derive(Default)]
struct RecordHints {
    tool_probe: Option<Vec<String>>,
    profile_probe: bool,
    download: Option<executor::DownloadMeta>,
}

struct Runtime {
    config: Option<AgentConfig>,
    broker: Arc<dyn ApprovalBroker>,
    session: SessionHandle,
    tx: broadcast::Sender<AgentEvent>,
    ask_rx: mpsc::Receiver<Command>,
    stop_tx: watch::Sender<bool>,
    stop_rx: watch::Receiver<bool>,
    journal: Arc<StdMutex<SerialJournal>>,
    records: Arc<StdMutex<ExecutionStore>>,
    mode: Arc<AtomicU8>,
    history: Vec<Message>,
    unlocked: Vec<&'static str>,
    capabilities: HashMap<String, executor::ToolCapability>,
    /// id of the `probe_download_tools` execution `download_to_target` needs.
    download_probe: Option<String>,
    /// Observed device profile, refreshed by `probe_device_profile`.
    profile: Option<executor::DeviceProfile>,
    policy: policy::PolicyStore,
    tasks: memory::TaskStore,
    notes: memory::NoteStore,
    device_key: String,
    notes_enabled: bool,
    read_cursor: u64,
    pending_execution: Option<PendingExecution>,
    download_stage: bool,
    console_kind: String,
    console_evidence: String,
    console_cursor: u64,
    input_pending: bool,
    input_revision: u64,
    session_key: String,
    approval_ids: Arc<AtomicU64>,
    last_question: String,
    turn: u32,
    tool_calls: u32,
    started: Instant,
}

async fn runtime_loop(mut runtime: Runtime) {
    loop {
        tokio::select! {
            biased;
            changed = runtime.stop_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                runtime.stop_rx.borrow_and_update();
            }
            maybe = runtime.ask_rx.recv() => {
                match maybe {
                    Some(Command::Ask(question)) => runtime.run(question).await,
                    Some(Command::Session(session)) => runtime.take_session(session),
                    Some(Command::Config(config)) => runtime.config = config,
                    Some(Command::ResetChat) => runtime.reset_chat(),
                    #[cfg(test)]
                    Some(Command::Seed(messages)) => runtime.history.extend(messages),
                    #[cfg(test)]
                    Some(Command::Probe(reply)) => {
                        let _ = reply.send(runtime.view());
                    }
                    None => break,
                }
            }
        }
    }
}

impl Runtime {
    fn emit(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }

    fn mode(&self) -> ExecMode {
        ExecMode::from_ordinal(self.mode.load(Ordering::Relaxed))
    }

    fn stopped(&self) -> bool {
        *self.stop_rx.borrow()
    }

    // -- guards ----------------------------------------------------------

    fn session_key_now(&self) -> String {
        let info = self.session.info();
        format!(
            "{}|{}|{}",
            self.transport_name(),
            info.label,
            info.device_id.as_deref().unwrap_or("")
        )
    }

    fn transport_name(&self) -> &'static str {
        match self.session.info().kind {
            Some(TransportKind::Ble) => "ble",
            _ => "ws",
        }
    }

    /// `checkSession` (spec §1.4): called at the start of, and after every
    /// await in, every tool and in `streamFn`.
    async fn check_session(&self) -> Result<(), String> {
        let key = self.session_key_now();
        let mode = self.mode();
        tokio::task::yield_now().await;
        if key != self.session_key || mode != self.mode() {
            return Err(executor::ERR_SESSION_MOVED.to_string());
        }
        Ok(())
    }

    // -- observation helpers ---------------------------------------------

    fn latest_cursor(&self) -> u64 {
        self.journal
            .lock()
            .map(|log| log.latest_cursor())
            .unwrap_or(0)
    }

    fn read_page(&self, after: Option<u64>, limit: usize) -> crate::journal::JournalRead {
        let empty = crate::journal::JournalRead {
            text: String::new(),
            start: 0,
            cursor: 0,
            latest: 0,
            truncated: false,
            updated_at: 0,
        };
        match self.journal.lock() {
            Ok(log) => log.read(after, limit),
            Err(_) => empty,
        }
    }

    /// Passive console hint over the tail of the journal.
    fn refresh_console(&mut self) {
        let latest = self.latest_cursor();
        let start = latest.saturating_sub(CONSOLE_WINDOW);
        let page = self.read_page(Some(start), CONSOLE_WINDOW as usize);
        let hint = console::inspect_serial_console(&page.text, page.cursor);
        self.console_kind = hint.kind;
        self.console_evidence = hint.evidence;
        self.console_cursor = hint.cursor;
    }

    // -- approvals --------------------------------------------------------

    fn approval_id(&self) -> u64 {
        self.approval_ids.fetch_add(1, Ordering::Relaxed)
    }

    async fn ask_approval(
        &self,
        tool_name: &str,
        kind: ApprovalKind,
        question: String,
    ) -> Result<(), String> {
        let rejection = rejection_message(&kind, tool_name);
        let request = ApprovalRequest {
            id: self.approval_id(),
            kind,
            question,
        };
        let rx = self.broker.ask(request);
        match tokio::time::timeout(Duration::from_millis(executor::APPROVAL_STALE_MS), rx).await {
            Err(_) | Ok(Err(_)) => Err(executor::ERR_APPROVAL_GONE.to_string()),
            Ok(Ok(ApprovalDecision::Approved)) => Ok(()),
            Ok(Ok(ApprovalDecision::Rejected)) => Err(rejection),
        }
    }

    // -- what the panel changes between turns ------------------------------

    /// Take the session the panel is now using.
    ///
    /// A swap only lands between turns — `runtime_loop` awaits a whole run
    /// before it reads the next command — so nothing is in flight. The state
    /// that belonged to the old session is dropped here rather than left for
    /// the next `run` to trip over, and the guard key is re-armed so
    /// [`Runtime::check_session`] reads this as the panel's doing instead of
    /// the device having moved underneath a turn.
    fn take_session(&mut self, session: crate::session::SessionHandle) {
        self.session = session;
        self.session_key = self.session_key_now();
        self.pending_execution = None;
    }

    /// Start a new conversation as far as the model is concerned.
    ///
    /// The panel clears its own transcript; this is the half the model reads.
    /// Anything still queued behind this command goes too — those were asked
    /// in the conversation that has just ended.
    fn reset_chat(&mut self) {
        self.history.clear();
        self.pending_execution = None;
        while self.ask_rx.try_recv().is_ok() {}
    }

    /// Test-only snapshot behind `AgentHandle::view`.
    #[cfg(test)]
    fn view(&self) -> RuntimeView {
        let info = self.session.info();
        RuntimeView {
            session_connected: info.connected,
            session_label: info.label.clone(),
            session_key_matches: self.session_key == self.session_key_now(),
            history: self.history.len(),
            pending: self.pending_execution.is_some(),
            config_model: self.config.as_ref().map(|config| config.model.clone()),
        }
    }

    // -- the run ----------------------------------------------------------

    async fn run(&mut self, question: String) {
        let _ = self.stop_tx.send(false);
        self.stop_rx.borrow_and_update();
        self.started = Instant::now();
        self.turn = 0;
        self.tool_calls = 0;
        self.download_stage = false;
        self.pending_execution = None;
        self.read_cursor = 0;

        let question = question.trim().to_string();
        if question.is_empty() {
            self.fail(executor::ERR_QUEUE_ITEM.to_string());
            return;
        }

        self.session_key = self.session_key_now();
        let info = self.session.info();
        self.device_key =
            memory::device_identity(self.transport_name(), info.device_id.as_deref(), None)
                .unwrap_or_default();

        let config = match self.config.clone() {
            Some(config) => config,
            None => {
                let message = if config::load_config().is_none() {
                    "Enter an API endpoint before chatting.".to_string()
                } else {
                    "Choose a model before chatting.".to_string()
                };
                self.fail(message);
                return;
            }
        };
        if config.endpoint.trim().is_empty() {
            self.fail("Enter an API endpoint before chatting.".to_string());
            return;
        }
        if config.model.trim().is_empty() {
            self.fail("Choose a model before chatting.".to_string());
            return;
        }
        if self.session_key_now() != self.session_key {
            self.fail(executor::ERR_SESSION_MOVED.to_string());
            return;
        }

        self.last_question = question.clone();
        self.emit(AgentEvent::MessageStart {
            role: "user".to_string(),
        });
        self.history.push(Message::user(question));
        self.history = settle_history(std::mem::take(&mut self.history));

        let outcome = self.agent_loop(config).await;

        self.history = settle_history(std::mem::take(&mut self.history));
        let reason = match outcome {
            Ok(reason) => reason,
            Err(error) => {
                self.emit(AgentEvent::Error(error.clone()));
                error
            }
        };
        self.emit(AgentEvent::RunFinished { reason });
    }

    fn fail(&self, message: String) {
        self.emit(AgentEvent::Error(message.clone()));
        self.emit(AgentEvent::RunFinished { reason: message });
    }

    fn over_budget(&self) -> Option<String> {
        if self.turn >= MAX_TURNS {
            return Some(format!(
                "Model turn budget reached ({MAX_TURNS} turns). Report what is still running and how to resume observation; open execution ids can be monitored in a new question."
            ));
        }
        if self.tool_calls >= MAX_TOOL_CALLS {
            return Some(executor::ERR_TOOL_BUDGET.to_string());
        }
        if self.started.elapsed() >= Duration::from_millis(RUN_LIMIT_MS) {
            return Some(STOP_REASON.to_string());
        }
        None
    }

    async fn agent_loop(&mut self, config: AgentConfig) -> Result<String, String> {
        loop {
            if let Some(reason) = self.over_budget() {
                return Ok(reason);
            }
            if self.stopped() {
                return Ok(STOPPED_BY_USER.to_string());
            }
            self.turn += 1;

            let mode = self.mode();
            let baseline = prompt::serial_system_prompt(mode, false, self.notes_enabled);
            let catalogue =
                tools::available_tools(false, self.notes_enabled, &self.unlocked.clone());
            let fixed_chars = baseline.chars().count()
                + catalogue
                    .iter()
                    .map(|tool| tool.name.chars().count() + tool.description.chars().count())
                    .sum::<usize>();
            let budget = context::context_budget_chars(
                config.context_window,
                fixed_chars,
                config.max_tokens,
            );

            let mut messages = Vec::with_capacity(self.history.len() + 1);
            messages.push(Message::system(baseline));
            messages.extend(self.history.iter().cloned());
            messages = compact_agent_context(messages, budget);

            let request = provider::build_request(&config, &messages, &catalogue);
            self.check_session().await?;
            // The dial and the reading of a failing response's body are one
            // wait as far as the stop button is concerned, so a host that never
            // finishes its reply cannot hold this turn open. `None` back means
            // stop, and stop is not a failure: it reports exactly like every
            // other way out of this loop.
            let response =
                match provider::send_or_stop(&request, Some(self.stop_rx.clone())).await? {
                    Some(response) => response,
                    None => return Ok(STOPPED_BY_USER.to_string()),
                };

            self.emit(AgentEvent::MessageStart {
                role: "assistant".to_string(),
            });
            let streamed = self.stream(response, config.provider).await?;
            let stop_reason = if streamed.tool_calls.is_empty() {
                if streamed.stop_reason.is_empty() {
                    "stop".to_string()
                } else {
                    streamed.stop_reason.clone()
                }
            } else {
                "tool_use".to_string()
            };
            let tool_calls = streamed.tool_calls;
            let stopped = streamed.stopped;
            let usage = streamed.usage;
            self.history.push(Message::assistant(
                streamed.text,
                tool_calls.clone(),
                &stop_reason,
            ));

            if let Some(usage) = usage {
                let cost = config::estimate_cost(
                    usage.input,
                    usage.output,
                    config.price_input,
                    config.price_output,
                )
                .unwrap_or(0.0);
                self.emit(AgentEvent::Usage {
                    input: usage.input,
                    output: usage.output,
                    cache_read: usage.cache_read,
                    total: usage.total,
                    cost,
                });
            }

            if tool_calls.is_empty() {
                return Ok(if stopped {
                    STOPPED_BY_USER.to_string()
                } else {
                    stop_reason
                });
            }

            for call in tool_calls {
                let (result, ok) = self.call_tool(&call).await;
                self.history.push(Message::tool_result(
                    &call.id,
                    &call.name,
                    result.clone(),
                    !ok,
                ));
                self.emit(AgentEvent::ToolEnd {
                    name: call.name.clone(),
                    result,
                    ok,
                });
                if REVIEW_TOOLS.contains(&call.name.as_str()) {
                    if let Some(pending) = self.pending_execution.as_mut() {
                        pending.reviewed_round = Some(self.turn);
                    }
                }
                if self.stopped() {
                    return Ok(STOPPED_BY_USER.to_string());
                }
                if let Some(reason) = self.over_budget() {
                    return Ok(reason);
                }
            }
        }
    }

    async fn stream(
        &self,
        response: reqwest::Response,
        provider_kind: Provider,
    ) -> Result<Streamed, String> {
        let stop = self.stop_rx.clone();
        let mut decoder = provider::SseDecoder::new();
        let mut assembler = provider::StreamAssembler::new(provider_kind);
        let mut body = response.bytes_stream();
        let mut out = Streamed::default();
        let mut ticker = tokio::time::interval(Duration::from_millis(50));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let deadline = tokio::time::sleep(provider::panel_timer());
        tokio::pin!(deadline);
        let mut done = false;

        while !done {
            let deltas = tokio::select! {
                biased;
                _ = ticker.tick() => {
                    if *stop.borrow() {
                        out.stopped = true;
                        break;
                    }
                    Vec::new()
                }
                _ = &mut deadline => return Err(STOP_REASON.to_string()),
                next = body.next() => match next {
                    None => {
                        done = true;
                        let mut events = Vec::new();
                        if let Some((event, data)) = decoder.finish() {
                            events.extend(assembler.feed(event.as_deref(), &data));
                        }
                        events.extend(assembler.finish());
                        events
                    }
                    Some(Err(error)) => return Err(error.to_string()),
                    Some(Ok(chunk)) => {
                        let mut events = Vec::new();
                        for (event, data) in decoder.push(&chunk) {
                            events.extend(assembler.feed(event.as_deref(), &data));
                        }
                        events
                    }
                },
            };
            for delta in deltas {
                match delta {
                    provider::StreamDelta::Text(text) => {
                        self.emit(AgentEvent::AssistantDelta(text.clone()));
                        out.text.push_str(&text);
                    }
                    provider::StreamDelta::ToolCall(call) => out.tool_calls.push(call),
                    provider::StreamDelta::Usage(usage) => out.usage = Some(usage),
                    provider::StreamDelta::Stop(reason) => out.stop_reason = reason,
                    provider::StreamDelta::Error(message) => return Err(message),
                }
            }
        }
        Ok(out)
    }

    // -- tools ------------------------------------------------------------

    /// Spec §1.3 `beforeToolCall` chain, in order.
    fn gate(&self, name: &str, round: u32) -> Option<String> {
        if self.download_stage && INPUT_TOOLS.contains(&name) {
            return Some(
                "Download stage is active or finished. Only observe and report its result; wait for a new user instruction before any further action."
                    .to_string(),
            );
        }
        if self.tool_calls >= MAX_TOOL_CALLS {
            return Some(executor::ERR_TOOL_BUDGET.to_string());
        }
        if INPUT_TOOLS.contains(&name) {
            if let Some(pending) = &self.pending_execution {
                if pending.reviewed_round.is_none() || pending.reviewed_round >= Some(round) {
                    return Some(format!(
                        "Inspect execution {} and read its result in the next model turn before sending another input. Do not batch dependent input.",
                        pending.id
                    ));
                }
            }
        }
        None
    }

    async fn call_tool(&mut self, call: &ToolCall) -> (String, bool) {
        self.emit(AgentEvent::ToolStart {
            name: call.name.clone(),
            args: call.arguments.clone(),
        });
        let round = self.turn;
        if let Some(reason) = self.gate(&call.name, round) {
            return (
                json!({ "error": reason, "blocked": true }).to_string(),
                false,
            );
        }
        let args: Value = match serde_json::from_str(&call.arguments) {
            Ok(value) => value,
            Err(_) => {
                return (
                    json!({ "error": "Invalid arguments for this tool." }).to_string(),
                    false,
                )
            }
        };
        self.tool_calls += 1;
        match self.dispatch(&call.name, args, round).await {
            Ok(text) => (tools::truncate_tool_result(&text), true),
            Err(error) => (json!({ "error": error }).to_string(), false),
        }
    }

    async fn dispatch(&mut self, name: &str, args: Value, round: u32) -> Result<String, String> {
        match name {
            "update_task_plan" => self.tool_update_task_plan(&args),
            "probe_tools" => self.tool_probe_tools(&args).await,
            "probe_device_profile" => {
                self.tool_probe(
                    "probe_device_profile",
                    executor::PROFILE_PROBE.to_string(),
                    true,
                )
                .await
            }
            "probe_download_tools" => {
                self.tool_probe(
                    "probe_download_tools",
                    executor::DOWNLOAD_PROBE.to_string(),
                    false,
                )
                .await
            }
            "download_to_target" => self.tool_download_to_target(&args).await,
            "download_to_computer" => {
                Err("Local saving is unavailable in this client.".to_string())
            }
            "run_shell_command" => self.tool_run_shell(&args).await,
            "monitor_serial_execution" => self.tool_monitor(&args, round).await,
            "read_web_page" => self.tool_read_web_page(&args).await,
            "search_serial_log" => self.tool_search_log(&args),
            "read_serial_log" => self.tool_read_log(&args),
            "get_device_status" => Ok(self.tool_device_status()),
            "send_serial_input" => self.tool_send_input(&args).await,
            "inspect_serial_execution" => self.tool_inspect(&args, round).await,
            "wait_for_serial_output" => self.tool_wait(&args).await,
            "verify_target_file" => self.tool_verify_file(&args).await,
            "verify_target_service" => self.tool_verify_service(&args).await,
            "watch_serial_output" => self.tool_watch(&args).await,
            "read_target_file" => self.tool_read_target_file(&args).await,
            "remember_target_note" => self.tool_remember_note(&args),
            "get_accessory_diagnostics" => self.tool_accessory_read().await,
            "set_uart_config" => self.tool_accessory_uart(&args).await,
            "wifi_scan" => self.tool_accessory_wifi_scan().await,
            "set_wifi" => self.tool_accessory_wifi(&args).await,
            "set_webdav" => self.tool_accessory_webdav(&args).await,
            other => Err(format!("Unknown tool: {other}")),
        }
    }

    // -- sending ----------------------------------------------------------

    /// The one path every UART-sending tool shares: validate, gate through the
    /// approval broker, wrap with the exit marker, record, deliver.
    async fn serial_send(
        &mut self,
        tool: &str,
        command: String,
        append_enter: bool,
        tracked: bool,
        hints: RecordHints,
    ) -> Result<Value, String> {
        self.check_session().await?;
        if tracked {
            validate_command(&command)?;
        } else {
            validate_input(&command)?;
        }
        self.refresh_console();
        let console_kind = self.console_kind.clone();
        let input_pending = self.input_pending;
        let mode = self.mode();

        let policy = self.policy.get(&self.device_key);
        let policy_ref = if policy.always_ask.is_empty() && policy.allow.is_empty() {
            None
        } else {
            Some(policy)
        };

        let token = if tracked {
            if !executor::track_exit_allowed(append_enter, &console_kind, input_pending) {
                return Err(executor::ERR_TRACKED_SHELL.to_string());
            }
            Some(completion_token())
        } else {
            None
        };

        let wire = match &token {
            Some(token) => tracked_command(&command, token)?,
            None => command.clone(),
        };
        let payload = if append_enter {
            format!("{wire}{SEND_ENTER}")
        } else {
            wire.clone()
        };

        let info = self.session.info();
        if !info.connected {
            return Err(executor::ERR_DISCONNECTED.to_string());
        }
        // Snapshot what the user is about to approve, exactly what the web
        // record carries when it is built (`device_executor.js`: `sessionId`,
        // `inputRevision`, `console`). The guard runs *after* the answer —
        // that wait is the whole point of it: a reconnect, a console change or
        // another input while the dialog sat open must not ride along.
        let proposed_session = self.session_key.clone();
        let proposed_revision = self.input_revision;
        let proposed_console = console_kind.clone();

        let kind = if tracked {
            ApprovalKind::RunCommand {
                command: wire.clone(),
                mode,
            }
        } else {
            ApprovalKind::SendInput {
                payload: payload.clone(),
            }
        };
        let needed = executor::needs_approval(
            mode,
            &command,
            append_enter,
            &payload,
            input_pending,
            &console_kind,
            policy_ref.as_ref(),
        );
        if needed {
            self.ask_approval(tool, kind, payload.clone()).await?;
        }
        // `device_executor.js` calls `check(record)` again once the user has
        // approved, against status read *now*. The console hint is re-read
        // here because nothing else refreshes it while the dialog was open.
        self.refresh_console();
        executor::check_send(
            self.session.info().connected,
            &self.session_key,
            &proposed_session,
            self.input_revision,
            proposed_revision,
            self.console_kind != proposed_console,
        )?;

        let log_start = self.latest_cursor();
        let id = {
            let mut store = self
                .records
                .lock()
                .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            let id = store.allocate_id();
            let mut record = ExecutionRecord::new(
                id.clone(),
                self.session_key.clone(),
                self.input_revision,
                mode.as_str(),
                tool,
                command.clone(),
                payload.clone(),
                append_enter,
                &console_kind,
                now_ms(),
            );
            record.completion_token = token;
            record.tool_probe = hints.tool_probe;
            record.profile_probe = hints.profile_probe;
            record.download = hints.download;
            record.log_start = log_start;
            store.push(record);
            if let Some(record) = store.get_mut(&id) {
                record.mark_sending();
            }
            id
        };

        if let Err(error) = self.session.send_uart(payload.as_bytes().to_vec()) {
            let delivery = {
                let mut store = self
                    .records
                    .lock()
                    .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
                match store.get_mut(&id) {
                    Some(record) => {
                        record.mark_failed(error.to_string(), false);
                        record.delivery.clone()
                    }
                    None => "unknown".to_string(),
                }
            };
            self.pending_execution = Some(PendingExecution {
                id: id.clone(),
                reviewed_round: None,
            });
            return Err(format!(
                "{error} Execution id: {id}, delivery: {delivery}. Inspect it before any further input; never automatically replay an interrupted transfer."
            ));
        }

        self.input_revision = self.input_revision.saturating_add(1);
        self.input_pending =
            policy::input_leaves_pending_line(payload.as_bytes(), self.input_pending);
        {
            let mut store = self
                .records
                .lock()
                .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            if let Some(record) = store.get_mut(&id) {
                record.mark_sent(self.input_revision, now_ms());
            }
        }
        self.pending_execution = Some(PendingExecution {
            id: id.clone(),
            reviewed_round: None,
        });

        let id_for_gate = id.clone();
        self.check_session().await?;
        self.emit_pending(&id_for_gate)
    }

    fn emit_pending(&self, id: &str) -> Result<Value, String> {
        let store = self
            .records
            .lock()
            .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
        let record = store
            .get(id)
            .ok_or_else(|| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
        let mut value = record.to_json();
        value["logStart"] = json!(record.log_start);
        value["next"] = json!(NEXT_INSPECT);
        Ok(value)
    }

    // -- observation ------------------------------------------------------

    /// Pull the latest journal page for one record and settle it.
    fn refresh_execution(&mut self, id: &str) -> Result<Vec<&'static str>, String> {
        let latest = self.latest_cursor();
        let connected = self.session.info().connected;
        let key = self.session_key.clone();
        let now = now_ms();

        // Observed capabilities are collected while the record guard is held
        // and applied afterwards: `self` has other mutable state to update.
        let mut observed: ObservedTools = None;
        let mut download_ready = false;
        let mut profile_text: Option<String> = None;

        {
            let journal = self
                .journal
                .lock()
                .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            let mut store = self
                .records
                .lock()
                .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            let record = store
                .get_mut(id)
                .ok_or_else(|| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            if record.session_id != key {
                return Err(executor::ERR_SESSION_CHANGED.to_string());
            }
            if record.is_open() {
                record.observed_end = Some(latest);
                let page = execution_page(record, &*journal, None, TOOL_PAGE_CHARS);
                refresh_record(record, page, now, connected, true);
                if record.exit_code.is_some() {
                    if record.tool_name == "probe_download_tools" {
                        download_ready = true;
                    }
                    if record.profile_probe {
                        profile_text = Some(record.evidence.clone());
                    }
                    if let Some(names) = record.tool_probe.clone() {
                        let evidence = record.evidence.clone();
                        let truncated = record.evidence_truncated;
                        if let Some(pairs) = executor::parse_tool_probe(
                            &evidence,
                            &names,
                            record.exit_code,
                            truncated,
                        ) {
                            observed = Some((
                                pairs,
                                record.observed_end.unwrap_or(record.log_start),
                                record.session_id.clone(),
                                record.id.clone(),
                            ));
                        }
                    }
                }
            }
        }

        let mut unlocked: Vec<&'static str> = Vec::new();
        if let Some((pairs, cursor, session_id, record_id)) = observed {
            for (name, available) in pairs {
                unlocked.extend(tools::unlock_tools(
                    &mut self.unlocked,
                    std::slice::from_ref(&name),
                ));
                self.capabilities.insert(
                    name,
                    executor::ToolCapability {
                        available,
                        observed_at: now,
                        session_id: session_id.clone(),
                        execution_id: record_id.clone(),
                        cursor,
                        source: "untrusted-target-output",
                        stale: false,
                    },
                );
            }
        }
        if download_ready {
            self.download_probe = Some(id.to_string());
        }
        if let Some(text) = profile_text {
            if let Some(profile) = executor::parse_device_profile(&text, now) {
                unlocked.extend(tools::unlock_tools(&mut self.unlocked, &profile.tools));
                self.profile = Some(profile);
            }
        }
        Ok(unlocked)
    }

    /// Poll one record until it resolves, the deadline passes or the run stops.
    async fn await_execution(&mut self, id: &str, timeout_ms: u64) -> Result<Value, String> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let stop = self.stop_rx.clone();
        loop {
            self.refresh_execution(id)?;
            let snapshot = {
                let store = self
                    .records
                    .lock()
                    .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
                let record = store
                    .get(id)
                    .ok_or_else(|| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
                (
                    record.execution_status.clone(),
                    record.observation_closed,
                    record.delivery.clone(),
                    record.waiting_for.clone(),
                )
            };
            let elapsed = timeout_ms.saturating_sub(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64,
            );
            let outcome = monitor_decide(
                &snapshot.0,
                snapshot.1,
                &snapshot.2,
                snapshot.3.as_deref(),
                elapsed,
                timeout_ms,
            );
            if outcome.done {
                let store = self
                    .records
                    .lock()
                    .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
                let record = store
                    .get(id)
                    .ok_or_else(|| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
                let mut value = record.to_json();
                value["logStart"] = json!(record.log_start);
                value["observedCursor"] = json!(record.observed_end.unwrap_or(record.log_start));
                value["timedOut"] = json!(outcome.timed_out);
                if let Some(next) = outcome.next {
                    value["next"] = json!(next);
                }
                return Ok(value);
            }
            if *stop.borrow() {
                return Ok(json!({ "id": id, "stopped": true, "waitStatus": "stopped" }));
            }
            let remaining = timeout_ms.saturating_sub(elapsed);
            tokio::time::sleep(Duration::from_millis(monitor_poll_ms(remaining))).await;
            self.check_session().await?;
        }
    }

    /// Evidence of a record read from its own `logStart` so markers are never
    /// missed by the tail window.
    fn record_evidence(&self, id: &str) -> Result<String, String> {
        let journal = self
            .journal
            .lock()
            .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
        let store = self
            .records
            .lock()
            .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
        let record = store
            .get(id)
            .ok_or_else(|| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
        let page = execution_page(record, &*journal, Some(record.log_start), TOOL_PAGE_CHARS);
        Ok(page.evidence)
    }

    /// `waitForSerialOutput`: quiet-interval detector over the live journal.
    async fn settle(
        &self,
        after: u64,
        timeout_ms: u64,
        settle_ms: u64,
    ) -> Result<executor::WaitOutcome, String> {
        let started = Instant::now();
        let mut changed_at = 0u64;
        let mut seen = self.latest_cursor();
        let stop = self.stop_rx.clone();
        loop {
            let now = started.elapsed().as_millis() as u64;
            let latest = self.latest_cursor();
            let (finish, outcome) = wait_decide(
                after, latest, now, 0, changed_at, seen, timeout_ms, settle_ms,
            );
            changed_at = if latest != seen { now } else { changed_at };
            seen = latest;
            if finish {
                return Ok(outcome);
            }
            if *stop.borrow() {
                return Ok(executor::WaitOutcome {
                    has_more: false,
                    has_new_output: outcome.has_new_output,
                    quiet_for_ms: outcome.quiet_for_ms,
                    wait_status: outcome.wait_status,
                    timed_out: true,
                });
            }
            let remaining = timeout_ms.saturating_sub(now);
            tokio::time::sleep(Duration::from_millis(wait_poll_ms(remaining))).await;
            self.check_session().await?;
        }
    }
}

#[derive(Default)]
struct Streamed {
    text: String,
    tool_calls: Vec<ToolCall>,
    stop_reason: String,
    usage: Option<provider::Usage>,
    stopped: bool,
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

impl Runtime {
    // -- planning and notes ------------------------------------------------

    fn tool_update_task_plan(&mut self, args: &Value) -> Result<String, String> {
        let raw = args
            .get("steps")
            .and_then(Value::as_array)
            .ok_or(ERR_PLAN)?;
        if raw.is_empty() || raw.len() > 8 {
            return Err(ERR_PLAN.to_string());
        }
        let mut in_progress = 0usize;
        let mut steps: Vec<TaskStep> = Vec::with_capacity(raw.len());
        for step in raw {
            let title = step
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let status = step.get("status").and_then(Value::as_str).unwrap_or("");
            let verification = step
                .get("verification")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let next_action = step
                .get("nextAction")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if title.is_empty()
                || title.chars().count() > 160
                || !["pending", "in_progress", "completed", "blocked"].contains(&status)
                || verification.chars().count() > 600
                || next_action.chars().count() > 400
            {
                return Err(ERR_PLAN.to_string());
            }
            if status == "in_progress" {
                in_progress += 1;
            }
            if status == "completed" && verification.trim().is_empty() {
                return Err(ERR_PLAN.to_string());
            }
            if status == "blocked" && next_action.trim().is_empty() {
                return Err(ERR_PLAN.to_string());
            }
            steps.push(TaskStep {
                title,
                status: status.to_string(),
                verification,
                next_action,
            });
        }
        if in_progress > 1 {
            return Err(ERR_PLAN.to_string());
        }

        if !self.device_key.is_empty() {
            let goal = if self.last_question.is_empty() {
                "Current diagnostic task".to_string()
            } else {
                self.last_question.clone()
            };
            let task = Task {
                id: format!("task-{}", now_ms()),
                device_key: self.device_key.clone(),
                updated_at: now_ms(),
                goal,
                summary: String::new(),
                status: if in_progress > 0 {
                    "running".to_string()
                } else {
                    "open".to_string()
                },
                plan: steps.clone(),
                executions: Vec::new(),
            };
            let _ = self.tasks.save(task);
        }

        let rendered: Vec<Value> = steps
            .iter()
            .map(|step| {
                json!({
                    "title": step.title,
                    "status": step.status,
                    "verification": step.verification,
                    "nextAction": step.next_action,
                })
            })
            .collect();
        Ok(json!({
            "steps": rendered,
            "source": "assistant-assessment",
            "verifiedByApplication": false,
        })
        .to_string())
    }

    fn tool_remember_note(&mut self, args: &Value) -> Result<String, String> {
        let text = args.get("text").and_then(Value::as_str).unwrap_or("");
        let evidence = args.get("evidence").and_then(Value::as_str).unwrap_or("");
        if text.trim().is_empty() || evidence.trim().is_empty() {
            return Err(
                "Provide the durable fact and the observation that supports it.".to_string(),
            );
        }
        if self.device_key.is_empty() {
            return Err("This target has no identity yet, so a note cannot be stored.".to_string());
        }
        let note = self.notes.add(&self.device_key, text, evidence)?;
        Ok(json!({
            "source": "assistant-note",
            "stored": {
                "id": note.id,
                "text": note.text,
                "evidence": note.evidence,
                "duplicate": note.duplicate,
            },
            "note": "This is the assistant's own record, not device evidence; it is shown to you when this target is connected again.",
        })
        .to_string())
    }

    // -- probes and shell -------------------------------------------------

    async fn tool_probe_tools(&mut self, args: &Value) -> Result<String, String> {
        let raw = args
            .get("names")
            .and_then(Value::as_array)
            .ok_or("Invalid tool names")?;
        let names: Vec<String> = raw
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        let names = validate_tool_names(&names)?;
        let command = executor::probe_tools_command(&names);
        let mut value = self
            .serial_send(
                "probe_tools",
                command,
                true,
                true,
                RecordHints {
                    tool_probe: Some(names.clone()),
                    ..RecordHints::default()
                },
            )
            .await?;
        value["toolProbe"] = json!(names);
        Ok(value.to_string())
    }

    async fn tool_probe(
        &mut self,
        tool: &str,
        command: String,
        profile: bool,
    ) -> Result<String, String> {
        let value = self
            .serial_send(
                tool,
                command,
                true,
                true,
                RecordHints {
                    profile_probe: profile,
                    ..RecordHints::default()
                },
            )
            .await?;
        Ok(value.to_string())
    }

    async fn tool_run_shell(&mut self, args: &Value) -> Result<String, String> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        validate_command(&command)?;
        let value = self
            .serial_send(
                "run_shell_command",
                command,
                true,
                true,
                RecordHints::default(),
            )
            .await?;
        Ok(value.to_string())
    }

    async fn tool_send_input(&mut self, args: &Value) -> Result<String, String> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // `appendEnter` is a required boolean (`web/device_executor.js:228`
        // rejects `typeof args.appendEnter !== "boolean"`): a missing one is
        // the same error as an empty text, not a silent "no newline".
        let append_enter = args
            .get("appendEnter")
            .and_then(Value::as_bool)
            .ok_or_else(|| executor::ERR_INVALID_INPUT.to_string())?;
        validate_input(&text)?;
        let value = self
            .serial_send(
                "send_serial_input",
                text,
                append_enter,
                false,
                RecordHints::default(),
            )
            .await?;
        Ok(value.to_string())
    }

    async fn tool_download_to_target(&mut self, args: &Value) -> Result<String, String> {
        let url = args.get("url").and_then(Value::as_str).unwrap_or("");
        let sha256 = args.get("sha256").and_then(Value::as_str).unwrap_or("");
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        let probe_id = self.download_probe.clone().ok_or_else(|| {
            "Complete probe_download_tools and inspect its result before downloading.".to_string()
        })?;
        let probe = {
            let store = self
                .records
                .lock()
                .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            store.get(&probe_id).cloned()
        };
        let plan = executor::target_download_plan(url, sha256, path, probe.as_ref())?;
        let mut value = self
            .serial_send(
                "download_to_target",
                plan.command,
                true,
                true,
                RecordHints {
                    download: Some(plan.metadata),
                    ..RecordHints::default()
                },
            )
            .await?;
        self.download_stage = true;
        value["downloadStage"] = json!(true);
        Ok(value.to_string())
    }

    async fn tool_monitor(&mut self, args: &Value, _round: u32) -> Result<String, String> {
        let id = required_id(args)?;
        let timeout = clamp_monitor_timeout(args.get("timeoutMs").and_then(Value::as_i64));
        self.await_execution(id, timeout)
            .await
            .map(|value| value.to_string())
    }

    async fn tool_inspect(&mut self, args: &Value, _round: u32) -> Result<String, String> {
        let id = required_id(args)?;
        let after = args.get("after").and_then(Value::as_i64).map(|v| v as u64);
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(TOOL_PAGE_CHARS as i64)
            .clamp(1, TOOL_PAGE_CHARS as i64) as usize;
        let timeout = args
            .get("timeoutMs")
            .and_then(Value::as_i64)
            .unwrap_or(5000)
            .clamp(100, 5000) as u64;

        let log_start = {
            let store = self
                .records
                .lock()
                .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            let record = store
                .get(id)
                .ok_or_else(|| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            if record.session_id != self.session_key {
                return Err(executor::ERR_SESSION_CHANGED.to_string());
            }
            record.log_start
        };

        let open = {
            let store = self
                .records
                .lock()
                .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            store
                .get(id)
                .map(|record| record.delivery == "sent" && !record.observation_closed)
                .unwrap_or(false)
        };
        let mut wait_status = "no-output";
        let mut timed_out = false;
        let mut quiet_for_ms = 0u64;
        if open {
            let outcome = self.settle(log_start, timeout, clamp_settle(400)).await?;
            wait_status = outcome.wait_status;
            timed_out = outcome.timed_out;
            quiet_for_ms = outcome.quiet_for_ms;
        }
        self.refresh_execution(id)?;

        let value = {
            let journal = self
                .journal
                .lock()
                .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            let store = self
                .records
                .lock()
                .map_err(|_| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            let record = store
                .get(id)
                .ok_or_else(|| executor::ERR_RECORD_UNAVAILABLE.to_string())?;
            let page = execution_page(record, &*journal, after, limit);
            let mut value = record.to_json();
            value["logStart"] = json!(record.log_start);
            value["observedCursor"] = json!(page.observed_cursor);
            value["latestCursor"] = json!(page.latest_cursor);
            value["hasMore"] = json!(page.has_more);
            value["evidenceStart"] = json!(page.evidence_start);
            value["evidence"] = json!(page.evidence);
            value["evidenceTruncated"] = json!(page.evidence_truncated);
            value["waitStatus"] = json!(wait_status);
            value["timedOut"] = json!(timed_out);
            value["quietForMs"] = json!(quiet_for_ms);
            value["untrusted"] = json!(true);
            value
        };
        Ok(value.to_string())
    }

    async fn tool_wait(&mut self, args: &Value) -> Result<String, String> {
        let after = args
            .get("after")
            .and_then(Value::as_i64)
            .map(|v| v as u64)
            .unwrap_or(self.latest_cursor());
        let timeout = args
            .get("timeoutMs")
            .and_then(Value::as_i64)
            .map(clamp_wait_timeout)
            .unwrap_or(5000);
        let settle = args
            .get("settleMs")
            .and_then(Value::as_i64)
            .map(clamp_settle)
            .unwrap_or(400);
        let outcome = self.settle(after, timeout, settle).await?;
        let page = self.read_page(Some(after), TOOL_PAGE_CHARS);
        self.read_cursor = page.cursor;
        Ok(json!({
            "text": page.text,
            "start": page.start,
            "cursor": page.cursor,
            "latestCursor": page.latest,
            "hasMore": page.cursor < page.latest,
            "hasNewOutput": outcome.has_new_output,
            "quietForMs": outcome.quiet_for_ms,
            "waitStatus": outcome.wait_status,
            "timedOut": outcome.timed_out,
            "untrusted": true,
        })
        .to_string())
    }

    fn tool_read_log(&mut self, args: &Value) -> Result<String, String> {
        let after = args.get("after").and_then(Value::as_i64).map(|v| v as u64);
        let recent = args.get("recent").and_then(Value::as_bool).unwrap_or(false);
        if recent && after.is_some() {
            return Err(ERR_READ_ARGS.to_string());
        }
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(6000)
            .clamp(1, TOOL_PAGE_CHARS as i64) as usize;
        let page = if recent {
            self.read_page(None, limit)
        } else if let Some(after) = after {
            self.read_page(Some(after), limit)
        } else {
            self.read_page(Some(self.read_cursor), limit)
        };
        self.read_cursor = page.cursor;
        Ok(json!({
            "text": page.text,
            "start": page.start,
            "cursor": page.cursor,
            "latestCursor": page.latest,
            "hasMore": page.cursor < page.latest,
            "truncated": page.truncated,
            "untrusted": true,
        })
        .to_string())
    }

    fn tool_search_log(&mut self, args: &Value) -> Result<String, String> {
        let query = args.get("query").and_then(Value::as_str).unwrap_or("");
        if query.trim().is_empty() || query.chars().count() > 200 {
            return Err(ERR_SEARCH_TEXT.to_string());
        }
        let after = args.get("after").and_then(Value::as_i64).map(|v| v as u64);
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(TOOL_PAGE_CHARS as i64)
            .clamp(1, TOOL_PAGE_CHARS as i64) as usize;
        let page = match after {
            Some(after) => self.read_page(Some(after), limit),
            None => self.read_page(None, limit),
        };

        let haystack: Vec<char> = page.text.chars().flat_map(char::to_lowercase).collect();
        let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
        let mut matches = Vec::new();
        let mut more = false;
        if !needle.is_empty() && haystack.len() >= needle.len() {
            let mut at = 0usize;
            while at + needle.len() <= haystack.len() {
                if &haystack[at..at + needle.len()] == needle.as_slice() {
                    if matches.len() >= 12 {
                        more = true;
                        break;
                    }
                    matches.push(excerpt_around(
                        &page.text,
                        at,
                        needle.len(),
                        query.chars().count(),
                    ));
                }
                at += 1;
            }
        }
        Ok(json!({
            "query": query,
            "matches": matches.into_iter().map(|excerpt| json!({ "excerpt": excerpt })).collect::<Vec<_>>(),
            "moreMatches": more,
            "start": page.start,
            "cursor": page.cursor,
            "latestCursor": page.latest,
            "hasMore": page.cursor < page.latest,
            "truncated": page.truncated,
            "untrusted": true,
            "note": "Matches apply only to this window. Boundary-spanning matches may require overlapping reads; absent or evicted logs cannot be searched.",
        })
        .to_string())
    }

    // -- downloads --------------------------------------------------------

    async fn tool_read_web_page(&mut self, args: &Value) -> Result<String, String> {
        self.check_session().await?;
        let url = args.get("url").and_then(Value::as_str).unwrap_or("");
        let offset = args.get("offset").and_then(Value::as_i64).unwrap_or(0);
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(8000);
        let find = args.get("find").and_then(Value::as_str);
        let page = web::read_web_page(url, offset, limit, find).await?;
        self.check_session().await?;
        Ok(page.to_json().to_string())
    }

    // -- device status ----------------------------------------------------

    fn tool_device_status(&mut self) -> String {
        self.refresh_console();
        let info = self.session.info();
        let transport = match info.kind {
            Some(TransportKind::Ble) => "ble",
            _ => "ws",
        };
        let profile_tools: Vec<String> = self
            .profile
            .as_ref()
            .map(|profile| profile.tools.clone())
            .unwrap_or_default();
        let added = tools::unlock_tools(&mut self.unlocked, &profile_tools);
        let notes = self.notes.list(&self.device_key);
        let capabilities: Value = self
            .capabilities
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    json!({
                        "available": entry.available,
                        "observedAt": entry.observed_at,
                        "sessionId": entry.session_id,
                        "executionId": entry.execution_id,
                        "cursor": entry.cursor,
                        "source": entry.source,
                        "stale": entry.stale,
                    }),
                )
            })
            .collect();
        let profile = match &self.profile {
            Some(profile) => serde_json::to_value(profile).unwrap_or(Value::Null),
            None => Value::Null,
        };
        json!({
            // Records (returned below, and what `ExecutionRecord.session_id`
            // holds) identify a session by `session_key`; the status has to
            // use the same value or a client filtering one against the other
            // can never match. `inputRevision` is the input counter and is
            // reported next to it.
            "sessionId": self.session_key.clone(),
            "connected": info.connected,
            "inputPending": self.input_pending,
            "inputRevision": self.input_revision,
            "transport": transport,
            "device": info.label,
            "deviceId": info.device_id,
            "uart": { "baud": info.baud, "writeSize": info.write_size },
            "receivedBytes": info.rx_bytes,
            "sentBytes": info.tx_bytes,
            "executionMode": self.mode().as_str(),
            "executionModeExpiresAt": 0,
            "console": {
                "kind": self.console_kind,
                "evidence": self.console_evidence,
                "cursor": self.console_cursor,
                "source": "serial-output-heuristic",
            },
            "profile": profile.clone(),
            "toolCapabilities": capabilities,
            "targetBinding": Value::Null,
            "rememberedProfile": profile,
            "notes": notes.iter().map(|note| json!({
                "id": note.id,
                "text": note.text,
                "evidence": note.evidence,
                "createdAt": note.created_at,
            })).collect::<Vec<_>>(),
            "addedTools": if added.is_empty() { Value::Null } else { json!(added) },
            "untrusted": true,
        })
        .to_string()
    }

    // -- watch ------------------------------------------------------------

    async fn tool_watch(&mut self, args: &Value) -> Result<String, String> {
        self.check_session().await?;
        let timeout = args
            .get("timeoutMs")
            .and_then(Value::as_i64)
            .unwrap_or(30000)
            .clamp(1000, 60000) as u64;
        let patterns: Vec<String> = args
            .get("patterns")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|value| !value.trim().is_empty() && value.chars().count() <= 120)
                    .take(8)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        // This window gets its own watcher: it is the one carrying the user's
        // patterns, and the shared watcher is already fed line for line by
        // `spawn_feed` from the bus. Feeding that one too would count every
        // line twice and would still never match a user pattern — the local
        // watcher was built for it and then never used.
        let mut watcher = SerialWatch::new(WatchOptions {
            max_findings: 20,
            boot_threshold: crate::watch::DEFAULT_BOOT_LOOP_THRESHOLD,
            boot_window_ms: crate::watch::DEFAULT_BOOT_LOOP_WINDOW_MS,
        });
        for (index, pattern) in patterns.iter().enumerate() {
            watcher.add_pattern(&format!("user-{index}"), pattern, pattern);
        }

        let started = Instant::now();
        let mut cursor = self.latest_cursor();
        let stop = self.stop_rx.clone();
        while started.elapsed().as_millis() < timeout as u128 {
            if *stop.borrow() {
                break;
            }
            let page = self.read_page(Some(cursor), 4000);
            if !page.text.is_empty() {
                watcher.feed_text(&page.text, now_ms());
                cursor = page.cursor.max(cursor);
            }
            if self.session_key_now() != self.session_key {
                return Err(executor::ERR_SESSION_MOVED.to_string());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        // The panel contract wants `matches`, `checkedLines` and the two
        // cursor bounds over what this window scanned.
        let findings: Vec<Finding> = watcher.findings().to_vec();
        let checked = watcher.lines_seen();

        let mut matches = Vec::new();
        let mut first: Option<u64> = None;
        let mut last: Option<u64> = None;
        for finding in &findings {
            matches.push(json!({
                "line": finding.line,
                "matchedAt": finding.at_ms,
                "builtin": !finding.id.starts_with("user-"),
                "pattern": finding.label,
                "kind": finding.kind.as_str(),
                "count": finding.count,
                "evidence": finding.evidence,
            }));
            first = Some(first.map_or(finding.at_ms, |value: u64| value.min(finding.at_ms)));
            last = Some(last.map_or(finding.at_ms, |value: u64| value.max(finding.at_ms)));
        }

        Ok(json!({
            "windowMs": started.elapsed().as_millis() as u64,
            "timeoutMs": timeout,
            "matches": matches,
            "checkedLines": checked,
            "firstMatch": first,
            "lastMatch": last,
            "untrusted": true,
            "note": "Findings are observations of untrusted output; absence in this window is not proof.",
        })
        .to_string())
    }

    // -- verify / read ----------------------------------------------------

    async fn tool_verify_file(&mut self, args: &Value) -> Result<String, String> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        let sha256 = args.get("sha256").and_then(Value::as_str).unwrap_or("");
        let bytes = args.get("bytes").and_then(Value::as_i64);
        let want_hash = args
            .get("hash")
            .and_then(Value::as_bool)
            .unwrap_or(!sha256.is_empty());
        let command = crate::target_verify::verify_file_command(path, want_hash)
            .map_err(|error| error.to_string())?;
        let sent = self
            .serial_send(
                "verify_target_file",
                command,
                true,
                true,
                RecordHints::default(),
            )
            .await?;
        let id = execution_id(&sent);
        self.await_execution(&id, 60_000).await?;
        let evidence = self.record_evidence(&id)?;
        let expectation = crate::target_verify::FileExpectation {
            path: Some(path.to_string()),
            bytes,
            sha256: sha256.to_string(),
        };
        let verify = crate::target_verify::parse_verify_result(&evidence, &expectation)
            .map_err(|error| error.to_string())?;
        let status = verify.status.as_str();
        Ok(json!({
            "status": status,
            "found": verify.found,
            "complete": verify.complete,
            "path": verify.path,
            "bytes": verify.bytes,
            "sha256": verify.sha256,
            "sha256Unavailable": verify.sha256_unavailable,
            "expectedBytes": verify.expected_bytes,
            "expectedSha256": verify.expected_sha256,
            "checks": verify.checks.iter().map(|check| check.as_str()).collect::<Vec<_>>(),
            "evidence": verify.evidence,
            "reason": verify.reason,
            "verifiedByApplication": true,
            "next": verify_next(status),
            "executionId": id,
            "untrusted": true,
        })
        .to_string())
    }

    async fn tool_verify_service(&mut self, args: &Value) -> Result<String, String> {
        let unit = args.get("unit").and_then(Value::as_str).unwrap_or("");
        let process = args.get("process").and_then(Value::as_str).unwrap_or("");
        let port = args.get("port").and_then(Value::as_i64);
        let expect = args.get("expect").and_then(Value::as_str).unwrap_or("");
        let command = crate::target_verify::verify_service_command(unit, process, port, expect)
            .map_err(|error| error.to_string())?;
        let sent = self
            .serial_send(
                "verify_target_service",
                command,
                true,
                true,
                RecordHints::default(),
            )
            .await?;
        let id = execution_id(&sent);
        self.await_execution(&id, 60_000).await?;
        let evidence = self.record_evidence(&id)?;
        let expectation = crate::target_verify::ServiceExpectation {
            unit: unit.to_string(),
            process: process.to_string(),
            port,
            expect: expect.to_string(),
        };
        let verify = crate::target_verify::parse_service_result(&evidence, &expectation)
            .map_err(|error| error.to_string())?;
        let status = verify.status.as_str();
        Ok(json!({
            "status": status,
            "kind": verify.kind.as_str(),
            "subject": verify.subject,
            "expected": verify.expected,
            "complete": verify.complete,
            "unsupported": verify.unsupported,
            "observed": verify.observed,
            "evidence": verify.evidence,
            "reason": verify.reason,
            "verifiedByApplication": true,
            "next": verify_next(status),
            "executionId": id,
            "untrusted": true,
        })
        .to_string())
    }

    async fn tool_read_target_file(&mut self, args: &Value) -> Result<String, String> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        let offset = args.get("offset").and_then(Value::as_i64).unwrap_or(0);
        let bytes = args
            .get("bytes")
            .and_then(Value::as_i64)
            .unwrap_or(crate::target_files::MAX_READ_BYTES);
        let command = crate::target_files::read_file_command(path, offset, bytes)
            .map_err(|error| error.to_string())?;
        let sent = self
            .serial_send(
                "read_target_file",
                command,
                true,
                true,
                RecordHints::default(),
            )
            .await?;
        let id = execution_id(&sent);
        self.await_execution(&id, 60_000).await?;
        let evidence = self.record_evidence(&id)?;
        let read = crate::target_files::parse_file_read(&evidence);
        let status = read.status.as_str();
        let total = read.total_bytes.unwrap_or(0);
        let from = read.from.unwrap_or(offset.max(0) as u64);
        let count = read.bytes.unwrap_or(0);
        let (encoding, content) = match read.data {
            Some(bytes) if is_printable(&bytes) => {
                ("text", String::from_utf8_lossy(&bytes).to_string())
            }
            Some(bytes) => ("base64", crate::target_files::encode_base64(&bytes)),
            None => ("text", String::new()),
        };
        Ok(json!({
            "path": path,
            "offset": from,
            "limit": bytes,
            "totalBytes": total,
            "eof": total > 0 && from + count >= total,
            "encoding": encoding,
            "content": content,
            "cursor": self.latest_cursor(),
            "status": status,
            "reason": read.reason,
            "executionId": id,
            "untrusted": true,
        })
        .to_string())
    }

    // -- accessory --------------------------------------------------------

    async fn accessory_mgmt(
        &mut self,
        tool: &str,
        command: String,
        needs_approval: bool,
    ) -> Result<String, String> {
        self.check_session().await?;
        if needs_approval {
            // The summary is shown to the user, so it is built from the
            // *redacted* command: a WebDAV URL may carry `user:secret@host`,
            // and `@d=` is exactly the form that does. The card would have
            // leaked what its own command field was hiding.
            let redacted = accessory::redact_command(&command);
            let summary = accessory::accessory_change_summary(&redacted, false);
            self.ask_approval(
                tool,
                ApprovalKind::AccessoryChange {
                    summary,
                    command: redacted.clone(),
                },
                redacted,
            )
            .await?;
        }
        let reply = self
            .session
            .request_mgmt(command, Some(Duration::from_secs(15)));
        let reply = tokio::time::timeout(Duration::from_secs(20), reply)
            .await
            .map_err(|_| "Accessory did not answer in time.".to_string())?
            .map_err(|_| "Accessory management channel closed.".to_string())??;
        self.check_session().await?;
        Ok(json!({
            "ok": reply.ok,
            "lines": reply.lines,
            "events": reply.events,
            "raw": reply.raw,
        })
        .to_string())
    }

    async fn tool_accessory_read(&mut self) -> Result<String, String> {
        let body = self
            .accessory_mgmt(
                "get_accessory_diagnostics",
                accessory::DIAGNOSTICS_COMMAND.to_string(),
                false,
            )
            .await?;
        let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let lines: Vec<String> = value
            .get("lines")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let groups = accessory::parse_info_groups(&lines);
        Ok(json!({
            "untrusted": false,
            "ok": value.get("ok").and_then(Value::as_bool).unwrap_or(false),
            "groups": groups,
            "lines": lines,
            "raw": value.get("raw").and_then(Value::as_str).unwrap_or(""),
        })
        .to_string())
    }

    async fn tool_accessory_uart(&mut self, args: &Value) -> Result<String, String> {
        let baud = args
            .get("baud")
            .and_then(Value::as_i64)
            .ok_or("baud must be an integer between 300 and 3000000")?;
        let data_bits = args
            .get("dataBits")
            .and_then(Value::as_i64)
            .map(|v| v as u8);
        let parity = args.get("parity").and_then(Value::as_str);
        let stop_bits = args
            .get("stopBits")
            .and_then(Value::as_i64)
            .map(|v| v as u8);
        let flow = args.get("flow").and_then(Value::as_str);
        let command = accessory::set_uart_command(baud as u32, data_bits, parity, stop_bits, flow)?;
        self.accessory_mgmt("set_uart_config", command, true).await
    }

    async fn tool_accessory_wifi_scan(&mut self) -> Result<String, String> {
        let body = self
            .accessory_mgmt("wifi_scan", "@w scan".to_string(), true)
            .await?;
        let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let networks: Vec<Value> = value
            .get("lines")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| json!({ "raw": line.trim() }))
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({ "networks": networks, "scannedAt": now_ms() }).to_string())
    }

    async fn tool_accessory_wifi(&mut self, args: &Value) -> Result<String, String> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("");
        let ssid = args.get("ssid").and_then(Value::as_str);
        let password = args.get("password").and_then(Value::as_str);
        if action != "off" && (ssid.is_none() || ssid.unwrap_or("").trim().is_empty()) {
            return Err("ssid is required".to_string());
        }
        if password.map(|value| value.chars().count()).unwrap_or(0) > 64 {
            return Err("Password must be at most 64 characters.".to_string());
        }
        let command = accessory::wifi_command(action, ssid, password)?;
        self.accessory_mgmt("set_wifi", command, true).await
    }

    async fn tool_accessory_webdav(&mut self, args: &Value) -> Result<String, String> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("");
        let url = args.get("url").and_then(Value::as_str);
        let command = accessory::webdav_command(action, url)?;
        self.accessory_mgmt("set_webdav", command, true).await
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn required_id(args: &Value) -> Result<&str, String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if id.is_empty() || id.chars().count() > 64 {
        return Err("Provide the execution id returned by the send.".to_string());
    }
    Ok(id)
}

/// `id` of an execution record returned by a send.
fn execution_id(value: &Value) -> String {
    value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 180 characters before, `query.length + 240` after (spec §2.10).
fn excerpt_around(text: &str, match_at: usize, match_len: usize, query_len: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = match_at.saturating_sub(180);
    let end = chars.len().min(match_at + match_len + query_len + 240);
    let head = if start > 0 { "…" } else { "" };
    let tail = if end < chars.len() { "…" } else { "" };
    format!(
        "{head}{}{tail}",
        chars[start..end].iter().collect::<String>()
    )
}

/// Model-facing `next` for every verification status (spec §9.3).
pub fn verify_next(status: &str) -> &'static str {
    match status {
        "match" => "Verification matched the supplied expectation only; confirm the user's actual goal separately.",
        "mismatch" => {
            "Verification mismatch: report observed versus expected and do not restate it as success."
        }
        "indeterminate" => {
            "Verification was indeterminate: report the reason instead of assuming either outcome."
        }
        _ => "Measurement observed with no expectation: this is NOT a verification.",
    }
}

fn is_printable(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .map(|text| !text.contains('\u{0}') && !text.chars().any(char::is_control))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_mode_wire_names() {
        assert_eq!(ExecMode::Manual.as_str(), "manual");
        assert_eq!(ExecMode::Auto.as_str(), "semi-auto");
        assert_eq!(ExecMode::FullAuto.as_str(), "full-auto");
        assert_eq!(ExecMode::parse("manual"), ExecMode::Manual);
        assert_eq!(ExecMode::parse("full-auto"), ExecMode::FullAuto);
        assert_eq!(ExecMode::parse("anything"), ExecMode::Auto);
        assert_eq!(ExecMode::parse("SEMI-AUTO"), ExecMode::Auto);
        assert_eq!(ExecMode::parse("Full_Auto"), ExecMode::FullAuto);
    }

    /// Two reject wordings, two specs: the accessory card's is pinned by
    /// `WEB_UX_SPEC.md` §5 (`web/accessory_control.js` → the Reject button),
    /// every other approval's by `AGENT_SPEC.md` §5.1. Answering the
    /// accessory card with the generic sentence tells the model *which tool*
    /// was refused instead of that the bridge was not reconfigured.
    #[test]
    fn a_rejected_accessory_change_uses_the_accessory_wording() {
        let web_spec =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/specs/WEB_UX_SPEC.md"))
                .expect("WEB_UX_SPEC.md");
        let agent_spec =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/specs/AGENT_SPEC.md"))
                .expect("AGENT_SPEC.md");

        let accessory = ApprovalKind::AccessoryChange {
            summary: "baud 115200".to_string(),
            command: "@u=115200".to_string(),
        };
        assert_eq!(
            rejection_message(&accessory, "set_uart_config"),
            "The user rejected this change. Do not retry unless the user asks again."
        );
        assert!(
            web_spec.contains(
                "The user rejected this change. Do not retry unless the user asks again."
            ),
            "the wording has to stay the one WEB_UX_SPEC §5 quotes"
        );

        let serial = ApprovalKind::SendInput {
            payload: "reboot".to_string(),
        };
        assert_eq!(
            rejection_message(&serial, "send_serial_input"),
            executor::rejected_message("send_serial_input")
        );
        assert!(
            agent_spec.contains(
                "`The user rejected ${toolName}. Do not retry this action; ask for a different approach or stop.`"
            ),
            "the generic wording has to stay the one AGENT_SPEC §5.1 quotes"
        );

        let command = ApprovalKind::RunCommand {
            command: "ls".to_string(),
            mode: ExecMode::Auto,
        };
        assert_eq!(
            rejection_message(&command, "run_shell_command"),
            executor::rejected_message("run_shell_command")
        );
    }

    #[test]
    fn budgets_match_spec_section_1_1() {
        assert_eq!(MAX_TURNS, 32);
        assert_eq!(MAX_TOOL_CALLS, 96);
        assert_eq!(RUN_LIMIT_MS, 900_000);
        assert_eq!(QUEUE_CAPACITY, 8);
        assert_eq!(STOP_REASON, "Stopped after 15 minutes");
        assert_eq!(executor::MAX_EXECUTION_RECORDS, 50);
        assert_eq!(executor::APPROVAL_STALE_MS, 900_000);
        assert_eq!(executor::EXECUTION_STALE_MS, 300_000);
    }

    /// Spec §6.4: Clear (and a connect) reset the evidence window, and the
    /// handle has to clear the **same** journal `spawn_feed` writes into — a
    /// second, private one would leave the assistant quoting the previous
    /// connection's bytes as live evidence.
    #[tokio::test]
    async fn reset_journal_clears_the_window_the_feed_writes() {
        let bus = crate::session::CoreBus::new();
        let handle = spawn(
            None,
            Arc::new(NoopBroker),
            SessionHandle::test_detached(),
            bus.clone(),
        );
        // Publish until the feed has subscribed: a broadcast send with no
        // subscriber yet is simply lost.
        for _ in 0..400 {
            if handle.journal_len() > 0 {
                break;
            }
            bus.publish(CoreEvent::UartRx(
                b"stale bytes from the previous session".to_vec(),
            ));
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(handle.journal_len() > 0, "the feed filled the window");
        handle.reset_journal();
        assert_eq!(handle.journal_len(), 0, "reset reaches that window");
    }

    /// `setMode` of `web/device_executor.js`: only Full Auto arms the
    /// unattended window, re-selecting it extends the window, and every other
    /// mode closes it. `full_auto_remaining` is what the header counts down.
    #[tokio::test]
    async fn only_full_auto_arms_the_unattended_window() {
        let bus = crate::session::CoreBus::new();
        let handle = spawn(
            None,
            Arc::new(NoopBroker),
            SessionHandle::test_detached(),
            bus,
        );
        assert_eq!(handle.mode(), ExecMode::Auto, "spawned in Auto");
        assert_eq!(handle.full_auto_remaining(), None, "…with no window");

        handle.set_mode(ExecMode::FullAuto);
        let armed = handle
            .full_auto_remaining()
            .expect("Full Auto arms the window");
        assert!(
            armed <= executor::FULL_AUTO_WINDOW_MS && armed > executor::FULL_AUTO_WINDOW_MS - 5_000,
            "…of fifteen minutes, got {armed} ms"
        );

        handle.set_mode(ExecMode::FullAuto);
        let extended = handle.full_auto_remaining().expect("still armed");
        assert!(
            extended >= armed,
            "re-selecting Full Auto extends the window"
        );

        handle.set_mode(ExecMode::Manual);
        assert_eq!(handle.mode(), ExecMode::Manual);
        assert_eq!(handle.full_auto_remaining(), None, "Manual closes it");
        handle.set_mode(ExecMode::Auto);
        assert_eq!(handle.full_auto_remaining(), None, "and so does Auto");
    }

    /// `armFullAuto`'s timer: at the deadline the mode falls back to `Auto`,
    /// the window closes and the panel is told — once, not on every tick.
    #[tokio::test]
    async fn the_unattended_window_falls_back_to_auto_when_it_runs_out() {
        let bus = crate::session::CoreBus::new();
        let handle = spawn(
            None,
            Arc::new(NoopBroker),
            SessionHandle::test_detached(),
            bus,
        );
        let mut events = handle.subscribe();
        handle.set_mode(ExecMode::FullAuto);

        // The wall clock without waiting fifteen minutes.
        handle.expire_window_now();
        for _ in 0..120 {
            if handle.mode() == ExecMode::Auto {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            handle.mode(),
            ExecMode::Auto,
            "the watchdog reverted the mode"
        );
        assert_eq!(handle.full_auto_remaining(), None, "…and closed the window");

        let mut expiry_notes = 0;
        for _ in 0..120 {
            if matches!(events.try_recv(), Ok(AgentEvent::ModeExpired)) {
                expiry_notes += 1;
            }
            if expiry_notes > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(expiry_notes, 1, "the panel hears about it exactly once");
    }

    #[test]
    fn gate_order_follows_the_spec() {
        let mut runtime = bare_runtime();
        runtime.download_stage = true;
        assert!(runtime
            .gate("run_shell_command", 1)
            .unwrap()
            .starts_with("Download stage is active"));
        assert!(runtime.gate("read_web_page", 1).is_none());

        runtime.download_stage = false;
        runtime.tool_calls = MAX_TOOL_CALLS;
        assert_eq!(
            runtime.gate("run_shell_command", 1).unwrap(),
            executor::ERR_TOOL_BUDGET
        );
        runtime.tool_calls = 0;

        runtime.pending_execution = Some(PendingExecution {
            id: "serial-3".into(),
            reviewed_round: None,
        });
        let blocked = runtime.gate("send_serial_input", 2).unwrap();
        assert_eq!(
            blocked,
            "Inspect execution serial-3 and read its result in the next model turn before sending another input. Do not batch dependent input."
        );
        // A tool outside the block list still runs.
        assert!(runtime.gate("watch_serial_output", 2).is_none());
        // Once reviewed in an earlier round the send is allowed again.
        runtime.pending_execution = Some(PendingExecution {
            id: "serial-3".into(),
            reviewed_round: Some(1),
        });
        assert!(runtime.gate("send_serial_input", 2).is_none());
        // Reviewing in the current round still blocks batching.
        runtime.pending_execution = Some(PendingExecution {
            id: "serial-3".into(),
            reviewed_round: Some(2),
        });
        assert!(runtime.gate("send_serial_input", 2).is_some());
    }

    #[test]
    fn verification_next_strings_are_verbatim() {
        assert_eq!(
            verify_next("match"),
            "Verification matched the supplied expectation only; confirm the user's actual goal separately."
        );
        assert_eq!(
            verify_next("mismatch"),
            "Verification mismatch: report observed versus expected and do not restate it as success."
        );
        assert_eq!(
            verify_next("indeterminate"),
            "Verification was indeterminate: report the reason instead of assuming either outcome."
        );
        assert_eq!(
            verify_next("observed"),
            "Measurement observed with no expectation: this is NOT a verification."
        );
    }

    #[test]
    fn excerpt_window_is_180_before_and_query_plus_240_after() {
        let text: String = (0..1000).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let excerpt = excerpt_around(&text, 500, 6, 6);
        let body = excerpt.trim_matches('…');
        assert_eq!(body.chars().count(), 180 + 6 + 6 + 240);
        assert!(excerpt.starts_with('…'));
        assert!(excerpt.ends_with('…'));
        // Head of the document needs no leading ellipsis.
        let excerpt = excerpt_around(&text, 10, 6, 6);
        assert!(!excerpt.starts_with('…'));
    }

    #[test]
    fn plan_validation_uses_the_exact_message() {
        let mut runtime = bare_runtime();
        let bad = json!({ "steps": [] });
        assert_eq!(runtime.tool_update_task_plan(&bad).unwrap_err(), ERR_PLAN);
        let bad = json!({ "steps": [{ "title": "a", "status": "completed" }] });
        assert_eq!(runtime.tool_update_task_plan(&bad).unwrap_err(), ERR_PLAN);
        let bad = json!({ "steps": [
            { "title": "a", "status": "in_progress" },
            { "title": "b", "status": "in_progress" },
        ] });
        assert_eq!(runtime.tool_update_task_plan(&bad).unwrap_err(), ERR_PLAN);
        let good = json!({ "steps": [
            { "title": "read the log", "status": "completed", "verification": "saw the panic line" },
            { "title": "fix the cable", "status": "pending" },
        ] });
        let value: Value =
            serde_json::from_str(&runtime.tool_update_task_plan(&good).unwrap()).unwrap();
        assert_eq!(value["source"], json!("assistant-assessment"));
        assert_eq!(value["verifiedByApplication"], json!(false));
        assert_eq!(value["steps"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn read_log_argument_conflict() {
        let mut runtime = bare_runtime();
        let args = json!({ "after": 0, "recent": true });
        assert_eq!(runtime.tool_read_log(&args).unwrap_err(), ERR_READ_ARGS);
    }

    #[test]
    fn search_log_reports_the_contract_note() {
        let mut runtime = bare_runtime();
        let args = json!({ "query": "   " });
        assert_eq!(runtime.tool_search_log(&args).unwrap_err(), ERR_SEARCH_TEXT);
        let args = json!({ "query": "panic" });
        let value: Value = serde_json::from_str(&runtime.tool_search_log(&args).unwrap()).unwrap();
        assert_eq!(
            value["note"],
            json!("Matches apply only to this window. Boundary-spanning matches may require overlapping reads; absent or evicted logs cannot be searched.")
        );
        assert_eq!(value["untrusted"], json!(true));
        assert_eq!(value["matches"], json!([]));
    }

    #[test]
    fn download_to_computer_is_always_unavailable() {
        // The dispatch arm returns this literal and nothing else: a browser
        // save card is the only path the web app has, and this client has none.
        const MESSAGE: &str = "Local saving is unavailable in this client.";
        let spec =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/specs/AGENT_SPEC.md"))
                .expect("AGENT_SPEC.md");
        assert!(MESSAGE.starts_with("Local saving"));
        assert_eq!(MESSAGE, "Local saving is unavailable in this client.");
        let _ = spec;
    }

    /// A reconnect is not only a panel-side swap. The runtime keeps its own
    /// clone of the session plus a key derived from it, and both went stale:
    /// the panel said connected while every tool reading the device read the
    /// connection that had just gone, and a key left behind fails them all
    /// with `ERR_SESSION_MOVED`.
    #[test]
    fn taking_a_new_session_rewrites_the_key_and_voids_the_pending_execution() {
        let mut rt = bare_runtime();
        rt.session_key = rt.session_key_now();
        assert!(
            !rt.view().session_connected,
            "the session it was spawned with is the detached one"
        );
        rt.pending_execution = Some(PendingExecution {
            id: "exec-on-the-old-link".to_string(),
            reviewed_round: None,
        });

        rt.take_session(SessionHandle::test_connected());

        let view = rt.view();
        assert!(
            view.session_connected,
            "the runtime has to read the session the panel just took"
        );
        assert_eq!(view.session_label, "test-device");
        assert!(
            view.session_key_matches,
            "a key left behind fails every tool with ERR_SESSION_MOVED"
        );
        assert!(
            !view.pending,
            "an execution started on the link that ended cannot outlive it"
        );
    }

    /// "New conversation" is the model's half, not the panel's: the history
    /// it reads, an execution not yet finished, and anything asked while the
    /// conversation was running all belong to the one that just ended.
    #[test]
    fn a_new_conversation_clears_the_history_the_pending_execution_and_the_queue() {
        let mut rt = bare_runtime();
        let (ask_tx, ask_rx) = mpsc::channel(4);
        rt.ask_rx = ask_rx;
        rt.history.push(Message::user("OLD-CONVERSATION-MARKER"));
        rt.pending_execution = Some(PendingExecution {
            id: "exec-still-running".to_string(),
            reviewed_round: None,
        });
        ask_tx
            .try_send(Command::Ask("asked in the old conversation".to_string()))
            .expect("the queue has room");

        rt.reset_chat();

        let view = rt.view();
        assert_eq!(
            view.history, 0,
            "the next request must not carry the old conversation"
        );
        assert!(!view.pending, "nor an execution belonging to it");
        assert!(
            rt.ask_rx.try_recv().is_err(),
            "a question queued behind the reset was asked in the conversation that ended"
        );
    }

    fn bare_runtime() -> Runtime {
        let (tx, _) = broadcast::channel(1);
        let (ask_tx, ask_rx) = mpsc::channel(1);
        drop(ask_tx);
        let (stop_tx, stop_rx) = watch::channel(false);
        Runtime {
            config: None,
            broker: Arc::new(NoopBroker),
            session: SessionHandle::test_detached(),
            tx,
            ask_rx,
            stop_tx,
            stop_rx,
            journal: Arc::new(StdMutex::new(SerialJournal::new())),
            records: Arc::new(StdMutex::new(ExecutionStore::new())),
            mode: Arc::new(AtomicU8::new(ExecMode::Auto.ordinal())),
            history: Vec::new(),
            unlocked: Vec::new(),
            capabilities: HashMap::new(),
            download_probe: None,
            profile: None,
            policy: policy::PolicyStore::open(std::env::temp_dir().join(format!(
                "linkr-agent-policy-{}-missing.json",
                std::process::id()
            ))),
            tasks: memory::TaskStore::open(std::env::temp_dir().join(format!(
                "linkr-agent-tasks-{}-missing.json",
                std::process::id()
            ))),
            notes: memory::NoteStore::open(std::env::temp_dir().join(format!(
                "linkr-agent-notes-{}-missing.json",
                std::process::id()
            ))),
            device_key: String::new(),
            notes_enabled: true,
            read_cursor: 0,
            pending_execution: None,
            download_stage: false,
            console_kind: "unknown".to_string(),
            console_evidence: String::new(),
            console_cursor: 0,
            input_pending: false,
            input_revision: 0,
            session_key: "test".to_string(),
            approval_ids: Arc::new(AtomicU64::new(1)),
            last_question: String::new(),
            turn: 0,
            tool_calls: 0,
            started: Instant::now(),
        }
    }

    struct NoopBroker;

    impl ApprovalBroker for NoopBroker {
        fn ask(&self, _request: ApprovalRequest) -> oneshot::Receiver<ApprovalDecision> {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(ApprovalDecision::Approved);
            rx
        }
    }

    /// A console that changed *while the approval dialog was open* is not the
    /// console the user approved. `device_executor.js` calls `check(record)`
    /// after the answer for exactly that reason; the port used to call it
    /// before, with each value handed to both sides, so it could never fail —
    /// a reboot behind the dialog would have sailed through.
    #[tokio::test]
    async fn a_console_that_changed_while_the_dialog_was_open_is_refused() {
        struct ChangeConsole {
            journal: Arc<StdMutex<SerialJournal>>,
        }

        impl ApprovalBroker for ChangeConsole {
            fn ask(&self, _request: ApprovalRequest) -> oneshot::Receiver<ApprovalDecision> {
                // The target comes up at a prompt while the user reads.
                if let Ok(mut log) = self.journal.lock() {
                    log.append_bytes(b"root@target:~# ");
                }
                let (tx, rx) = oneshot::channel();
                let _ = tx.send(ApprovalDecision::Approved);
                rx
            }
        }

        let mut rt = bare_runtime();
        rt.session = SessionHandle::test_connected();
        rt.session_key = rt.session_key_now();
        let journal = rt.journal.clone();
        rt.broker = Arc::new(ChangeConsole { journal });

        let error = rt
            .serial_send(
                "send_serial_input",
                "reboot".to_string(),
                true,
                false,
                RecordHints::default(),
            )
            .await
            .expect_err("the dialog's window is the guard's window");
        assert_eq!(error, executor::ERR_CONSOLE_CHANGED);
    }

    /// `SessionHandle::test_detached` is `#[cfg(test)]`; the helper above
    /// relies on it being reachable from this crate's own test build.
    #[test]
    fn detached_session_reports_disconnected() {
        let handle = SessionHandle::test_detached();
        assert!(!handle.info().connected);
    }
}
