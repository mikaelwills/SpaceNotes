use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use spacetimedb_sdk::SubscriptionHandle as _;
use spacetimedb_sdk::{DbContext, Event, Table, TableWithPrimaryKey};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use crate::bindings::*;

const HEARTBEAT: Duration = Duration::from_secs(30);
const WAKE_CHECK: Duration = Duration::from_secs(5);
const WAKE_GAP: Duration = Duration::from_secs(15);
const REDUCER_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RECONNECT_BASE_MS: u64 = 1_000;
const RECONNECT_MAX_MS: u64 = 60_000;
const POLL_MAX_MS: u64 = 25_000;
const VAULT_RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
const INBOX_TTL: Duration = Duration::from_secs(48 * 60 * 60);
const INBOX_SWEEP: Duration = Duration::from_secs(60 * 60);
const INBOX_IMAGE_EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "png", "gif", "webp"];
const A2A_DEFAULT_MAX_HOPS: u32 = 4;
const A2A_HOP_DECAY: Duration = Duration::from_secs(5 * 60);

const HOST_ALIASES: [(&str, &str); 1] = [("mikael-NUC10i3FNK", "robert")];

type ReducerOutcome = Result<Result<(), String>, spacetimedb_sdk::__codegen::InternalError>;

fn flatten(outcome: ReducerOutcome) -> Result<(), String> {
    match outcome {
        Ok(inner) => inner,
        Err(panicked) => Err(panicked.to_string()),
    }
}

macro_rules! reduce {
    ($bridge:expr, $live:expr, $name:literal, $method:ident ( $($arg:expr),* $(,)? )) => {{
        let (tx, rx) = oneshot::channel();
        let sent = $live.conn.reducers().$method($($arg),*, move |_ctx, outcome| {
            let _ = tx.send(flatten(outcome));
        });
        match sent {
            Err(e) => Err(anyhow!("{} could not be sent: {e}", $name)),
            Ok(_) => $bridge.await_reducer($name, &$live, rx).await,
        }
    }};
}

#[derive(Clone)]
struct Live {
    conn: Arc<DbConnection>,
    dead: mpsc::UnboundedSender<String>,
}

struct Outcomes<T> {
    done: HashMap<String, T>,
    waiting: HashMap<String, oneshot::Sender<T>>,
}

impl<T> Default for Outcomes<T> {
    fn default() -> Self {
        Self { done: HashMap::new(), waiting: HashMap::new() }
    }
}

#[derive(Default)]
struct A2a {
    send_times: VecDeque<Instant>,
    last_to_target: HashMap<String, Instant>,
    incoming_hop: Option<(u32, Instant)>,
}

pub struct Bridge {
    pub agent_id: String,
    base_name: String,
    host: String,
    client_id: String,
    stdb_uri: String,
    stdb_db: String,
    files_url: String,
    log_file: String,
    inbox_dir: std::path::PathBuf,
    build_sha: String,
    live: Mutex<Option<Live>>,
    shutting_down: AtomicBool,
    heartbeats: AtomicU64,
    permissions: Mutex<Outcomes<String>>,
    questions: Mutex<Outcomes<Option<String>>>,
    a2a: Mutex<A2a>,
    stdout_tx: mpsc::UnboundedSender<String>,
    stdout_rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
    inbound_tx: mpsc::UnboundedSender<Message>,
    inbound_rx: Mutex<Option<mpsc::UnboundedReceiver<Message>>>,
}

fn raw_host() -> String {
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        return "unknown".into();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).to_string();
    name.split('.').next().unwrap_or("unknown").to_string()
}

fn files_url_for(stdb_uri: &str) -> String {
    let http = stdb_uri.replacen("wss://", "https://", 1).replacen("ws://", "http://", 1);
    let Some((scheme, rest)) = http.split_once("://") else {
        return http;
    };
    let host = rest.split('/').next().unwrap_or(rest);
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    format!("{scheme}://{host}:5051")
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

fn short_uuid(len: usize) -> String {
    uuid::Uuid::new_v4().simple().to_string()[..len].to_string()
}

fn vault_link_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"spacenotes://file/([0-9a-fA-F-]{36})\)").expect("valid vault link regex"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendToAgentArgs {
    agent: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PushMessageArgs {
    role: String,
    text: String,
    #[serde(default = "default_source")]
    source: String,
    id: Option<String>,
}

fn default_source() -> String {
    "mod".into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditMessageArgs {
    id: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateMessageArgs {
    id: String,
    text: String,
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PushStatusArgs {
    state: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PushToolEventArgs {
    tool: String,
    detail: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PushContextUsageArgs {
    used: u64,
    window: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestPermissionArgs {
    id: String,
    tool: String,
    input: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PollArgs {
    id: String,
    #[serde(default = "default_poll_ms")]
    timeout_ms: u64,
}

fn default_poll_ms() -> u64 {
    20_000
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuestionArg {
    question: String,
    #[serde(default)]
    header: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    multi_select: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestQuestionArgs {
    questions: Vec<QuestionArg>,
}

fn parse<T: for<'de> Deserialize<'de>>(tool: &str, args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("invalid arguments for {tool}: {e}"))
}

impl Bridge {
    pub fn new(agent: &str, stdb_uri: &str, stdb_db: &str, build_sha: &str) -> Self {
        let raw = raw_host();
        let host = HOST_ALIASES
            .iter()
            .find(|(from, _)| *from == raw)
            .map(|(_, to)| to.to_string())
            .unwrap_or(raw);
        let inbox_dir = std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
            .join(".claude/channels/space-channel/inbox");
        let files_url = std::env::var("SPACE_CHANNEL_FILES_URL").unwrap_or_else(|_| files_url_for(stdb_uri));
        let (stdout_tx, stdout_rx) = mpsc::unbounded_channel();
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        Self {
            agent_id: format!("{agent}@{host}"),
            base_name: agent.to_string(),
            host,
            client_id: uuid::Uuid::new_v4().to_string(),
            stdb_uri: stdb_uri.to_string(),
            stdb_db: stdb_db.to_string(),
            files_url,
            log_file: format!("/tmp/space-channel-{agent}.log"),
            inbox_dir,
            build_sha: build_sha.to_string(),
            live: Mutex::new(None),
            shutting_down: AtomicBool::new(false),
            heartbeats: AtomicU64::new(0),
            permissions: Mutex::new(Outcomes::default()),
            questions: Mutex::new(Outcomes::default()),
            a2a: Mutex::new(A2a::default()),
            stdout_tx,
            stdout_rx: Mutex::new(Some(stdout_rx)),
            inbound_tx,
            inbound_rx: Mutex::new(Some(inbound_rx)),
        }
    }

    pub fn log(&self, msg: &str) {
        crate::log::append(&self.log_file, msg);
    }

    fn emit(&self, event: Value) {
        let _ = self.stdout_tx.send(event.to_string());
    }

    pub fn start(self: Arc<Self>) {
        self.log(&format!("bridge {} starting for {} (stdb {}/{})", self.build_sha, self.agent_id, self.stdb_uri, self.stdb_db));
        if let Some(rx) = self.stdout_rx.lock().unwrap().take() {
            let bridge = self.clone();
            std::thread::spawn(move || bridge.write_stdout(rx));
        }
        if let Some(rx) = self.inbound_rx.lock().unwrap().take() {
            tokio::spawn(self.clone().process_inbound(rx));
        }
        tokio::spawn(self.clone().sweep_inbox_forever());
        tokio::spawn(self.run_connection());
    }

    fn write_stdout(&self, mut rx: mpsc::UnboundedReceiver<String>) {
        while let Some(line) = rx.blocking_recv() {
            let mut out = std::io::stdout().lock();
            let written = out.write_all(line.as_bytes()).and_then(|_| out.write_all(b"\n")).and_then(|_| out.flush());
            if let Err(err) = written {
                self.log(&format!("stdout closed ({err}), exiting"));
                std::process::exit(0);
            }
        }
    }

    fn current(&self) -> Option<Live> {
        self.live.lock().unwrap().clone()
    }

    async fn await_reducer(&self, what: &str, live: &Live, rx: oneshot::Receiver<Result<(), String>>) -> anyhow::Result<()> {
        match timeout(REDUCER_TIMEOUT, rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(reducer_error))) => bail!("{what} failed: {reducer_error}"),
            Ok(Err(_)) => bail!("{what}: connection closed before it was confirmed"),
            Err(_) => {
                let _ = live.dead.send(format!("{what} timed out after {}ms", REDUCER_TIMEOUT.as_millis()));
                bail!("{what} timed out after {}ms, reconnecting", REDUCER_TIMEOUT.as_millis())
            }
        }
    }

    async fn run_connection(self: Arc<Self>) {
        let mut attempt: u32 = 0;
        let mut ever_connected = false;
        loop {
            if self.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            match self.clone().connect_once().await {
                Ok(()) => {
                    ever_connected = true;
                    attempt = 0;
                }
                Err(err) => self.log(&format!("SpacetimeDB connect error: {err:#}")),
            }
            if self.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            let delay = RECONNECT_BASE_MS.saturating_mul(1u64 << attempt.min(16)).min(RECONNECT_MAX_MS);
            attempt += 1;
            self.log(&format!(
                "Reconnecting in {delay}ms (attempt {attempt}{})",
                if ever_connected { "" } else { ", initial" }
            ));
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    }

    async fn connect_once(self: Arc<Self>) -> anyhow::Result<()> {
        let (connected_tx, connected_rx) = oneshot::channel::<Result<String, String>>();
        let connected_tx = Arc::new(Mutex::new(Some(connected_tx)));
        let on_ok = connected_tx.clone();
        let on_err = connected_tx.clone();
        let (dead_tx, mut dead_rx) = mpsc::unbounded_channel::<String>();
        let on_disconnect = dead_tx.clone();

        let conn = DbConnection::builder()
            .with_uri(self.stdb_uri.as_str())
            .with_database_name(self.stdb_db.as_str())
            .on_connect(move |_conn, identity, _token| {
                if let Some(tx) = on_ok.lock().unwrap().take() {
                    let _ = tx.send(Ok(identity.to_hex().to_string()));
                }
            })
            .on_connect_error(move |_ctx, err| {
                if let Some(tx) = on_err.lock().unwrap().take() {
                    let _ = tx.send(Err(err.to_string()));
                }
            })
            .on_disconnect(move |_ctx, err| {
                let reason = err.map(|e| e.to_string()).unwrap_or_else(|| "clean".into());
                let _ = on_disconnect.send(format!("disconnected: {reason}"));
            })
            .build()
            .context("building connection")?;
        let conn = Arc::new(conn);
        conn.run_threaded();

        let identity = match timeout(CONNECT_TIMEOUT, connected_rx).await {
            Ok(Ok(Ok(identity))) => identity,
            Ok(Ok(Err(err))) => {
                let _ = conn.disconnect();
                bail!("{err}");
            }
            _ => {
                let _ = conn.disconnect();
                bail!("no connection within {}s", CONNECT_TIMEOUT.as_secs());
            }
        };
        self.log(&format!(
            "Connected to SpacetimeDB {}/{} as {}…",
            self.stdb_uri,
            self.stdb_db,
            &identity[..identity.len().min(12)]
        ));

        let live = Live { conn: conn.clone(), dead: dead_tx.clone() };
        let registered: anyhow::Result<()> = async {
            reduce!(
                self,
                live,
                "registerAgent",
                register_agent_then(self.agent_id.clone(), self.base_name.clone(), self.host.clone(), self.client_id.clone())
            )
        }
        .await;
        if let Err(err) = registered {
            let _ = conn.disconnect();
            bail!("registerAgent failed: {err:#}");
        }
        self.log(&format!("Registered agent {}", self.agent_id));

        self.install_callbacks(&conn);
        let applied = self.clone();
        let failed = dead_tx.clone();
        let agent = self.agent_id.clone();
        conn.subscription_builder()
            .on_applied(move |_ctx| {
                applied.log("Subscriptions applied");
                applied.emit(json!({ "type": "ready", "agentId": applied.agent_id }));
            })
            .on_error(move |_ctx, err| {
                let _ = failed.send(format!("subscription error: {err}"));
            })
            .subscribe(vec![
                "SELECT * FROM agent".to_string(),
                "SELECT * FROM channel_config".to_string(),
                format!("SELECT * FROM permission_request WHERE agent_id = '{agent}'"),
                format!("SELECT * FROM question_request WHERE agent_id = '{agent}'"),
                format!("SELECT * FROM message WHERE agent_id = '{agent}' AND role = 'user'"),
            ]);

        *self.live.lock().unwrap() = Some(live.clone());

        let mut heartbeat = tokio::time::interval(HEARTBEAT);
        heartbeat.tick().await;
        let mut wake = tokio::time::interval(WAKE_CHECK);
        let mut last_tick = SystemTime::now();
        let reason = loop {
            tokio::select! {
                Some(reason) = dead_rx.recv() => break reason,
                _ = heartbeat.tick() => {
                    if let Err(err) = self.heartbeat(&live).await {
                        break format!("heartbeat failed: {err:#}");
                    }
                }
                _ = wake.tick() => {
                    let now = SystemTime::now();
                    let gap = now.duration_since(last_tick).unwrap_or_default();
                    last_tick = now;
                    if gap > WAKE_GAP {
                        self.log(&format!("Timer gap {}ms, probing the connection after sleep", gap.as_millis()));
                        if let Err(err) = self.heartbeat(&live).await {
                            break format!("heartbeat failed: {err:#}");
                        }
                    }
                }
            }
        };

        self.log(&format!("Dropping connection: {reason}"));
        {
            let mut slot = self.live.lock().unwrap();
            if slot.as_ref().is_some_and(|l| Arc::ptr_eq(&l.conn, &conn)) {
                *slot = None;
            }
        }
        let _ = conn.disconnect();
        Ok(())
    }

    async fn heartbeat(&self, live: &Live) -> anyhow::Result<()> {
        reduce!(self, live, "heartbeat", heartbeat_then(self.agent_id.clone()))?;
        let count = self.heartbeats.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 1 || count % 60 == 0 {
            self.log(&format!("heartbeat ok (count={count})"));
        }
        Ok(())
    }

    fn install_callbacks(self: &Arc<Self>, conn: &DbConnection) {
        let inbound = self.clone();
        conn.db().message().on_insert(move |ctx, row| {
            let applied = matches!(ctx.event, Event::SubscribeApplied);
            inbound.log(&format!(
                "message.onInsert id={} role={} source={} event_tag={}",
                row.id,
                row.role,
                row.source,
                if applied { "SubscribeApplied" } else { "Transaction" }
            ));
            if applied {
                return;
            }
            let _ = inbound.inbound_tx.send(row.clone());
        });

        let inserted = self.clone();
        conn.db().permission_request().on_insert(move |ctx, row| {
            if matches!(ctx.event, Event::SubscribeApplied) {
                return;
            }
            inserted.settle_permission(row);
        });
        let updated = self.clone();
        conn.db().permission_request().on_update(move |_ctx, _old, row| updated.settle_permission(row));

        let inserted = self.clone();
        conn.db().question_request().on_insert(move |ctx, row| {
            if matches!(ctx.event, Event::SubscribeApplied) {
                return;
            }
            inserted.settle_question(row);
        });
        let updated = self.clone();
        conn.db().question_request().on_update(move |_ctx, _old, row| updated.settle_question(row));
    }

    fn settle_permission(&self, row: &PermissionRequest) {
        if row.status == "pending" {
            return;
        }
        settle(&self.permissions, &row.id, row.status.clone());
    }

    fn settle_question(&self, row: &QuestionRequest) {
        if row.status == "pending" {
            return;
        }
        settle(&self.questions, &row.id, row.response.clone());
    }

    async fn process_inbound(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Message>) {
        while let Some(row) = rx.recv().await {
            self.handle_incoming(row).await;
        }
    }

    async fn handle_incoming(&self, row: Message) {
        if row.role == "user" && row.source == "control" {
            self.log(&format!("control received id={} text={}", row.id, row.text));
            self.emit_inbound(&row.id, &row.text, &row.source, "flutter", &[]);
            return;
        }
        let from_agent = row.source.starts_with("agent:");
        if row.role != "user" || (row.source != "flutter" && !from_agent) {
            self.log(&format!("inbound skipped id={} (role/source filter)", row.id));
            return;
        }
        {
            let mut a2a = self.a2a.lock().unwrap();
            a2a.incoming_hop = if from_agent {
                let hop = row
                    .id
                    .strip_prefix("a2a-")
                    .and_then(|rest| rest.split('-').next())
                    .and_then(|n| n.parse::<u32>().ok())
                    .unwrap_or(0);
                Some((hop, Instant::now()))
            } else {
                None
            };
        }
        let sender = row.source.strip_prefix("agent:").unwrap_or("flutter").to_string();
        let images = if row.source == "flutter" {
            self.attach_vault_images(&row).await
        } else {
            Vec::new()
        };
        self.log(&format!(
            "inbound queued id={} sender={sender} text_len={} images={}",
            row.id,
            row.text.len(),
            images.len()
        ));
        self.emit_inbound(&row.id, &row.text, &row.source, &sender, &images);
    }

    fn emit_inbound(&self, id: &str, text: &str, source: &str, sender: &str, images: &[String]) {
        let mut message = json!({ "id": id, "text": text, "source": source, "sender": sender });
        if !images.is_empty() {
            message["imagePaths"] = json!(images);
        }
        self.emit(json!({ "type": "inbound", "message": message }));
    }

    async fn attach_vault_images(&self, row: &Message) -> Vec<String> {
        let ids: Vec<String> = vault_link_re().captures_iter(&row.text).map(|c| c[1].to_string()).collect();
        if ids.is_empty() {
            return Vec::new();
        }
        let Some(live) = self.current() else {
            return Vec::new();
        };
        if let Err(err) = std::fs::create_dir_all(&self.inbox_dir) {
            self.log(&format!("inbox dir failed: {err}"));
            return Vec::new();
        }
        let mut paths = Vec::new();
        for (n, id) in ids.iter().enumerate() {
            let Some(remote) = resolve_file_path(&live.conn, id).await else {
                self.log(&format!("vault link {id} did not resolve to a file"));
                continue;
            };
            let ext = remote.rsplit('.').next().unwrap_or("").to_lowercase();
            if !INBOX_IMAGE_EXTENSIONS.contains(&ext.as_str()) {
                continue;
            }
            let encoded: Vec<String> = remote.split('/').map(encode_segment).collect();
            let url = format!("{}/files/{}", self.files_url, encoded.join("/"));
            let local = self.inbox_dir.join(format!("{}-{}.{ext}", row.id, n + 1));
            let target = local.clone();
            let fetched = tokio::task::spawn_blocking(move || download(&url, &target)).await;
            match fetched {
                Ok(Ok(())) => paths.push(local.to_string_lossy().to_string()),
                Ok(Err(err)) => self.log(&format!("vault image download failed for {remote}: {err}")),
                Err(err) => self.log(&format!("vault image download task failed for {remote}: {err}")),
            }
        }
        paths
    }

    async fn sweep_inbox_forever(self: Arc<Self>) {
        loop {
            self.sweep_inbox();
            tokio::time::sleep(INBOX_SWEEP).await;
        }
    }

    fn sweep_inbox(&self) {
        let Ok(entries) = std::fs::read_dir(&self.inbox_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(modified) = meta.modified() else { continue };
            if modified.elapsed().unwrap_or_default() > INBOX_TTL {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    pub async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
        let Some(live) = self.current() else {
            return Err("not connected to SpacetimeDB".into());
        };
        let result = match tool {
            "send_to_agent" => {
                let a: SendToAgentArgs = parse(tool, args)?;
                return self.send_to_agent(&live, &a.agent, &a.text).await;
            }
            "push_message" => {
                let a: PushMessageArgs = parse(tool, args)?;
                let prefix = if a.role == "assistant" { "reply" } else { "prompt" };
                let id = a.id.unwrap_or_else(|| format!("{prefix}-{}-{}", now_ms(), short_uuid(6)));
                self.log(&format!("push_message id={id} source={} len={}", a.source, a.text.len()));
                reduce!(self, live, "pushMessage", push_message_then(id.clone(), self.agent_id.clone(), a.role, a.text, a.source))
                    .map(|_| json!({ "id": id }))
            }
            "edit_message" => {
                let a: EditMessageArgs = parse(tool, args)?;
                self.log(&format!("edit_message id={} len={}", a.id, a.text.len()));
                reduce!(self, live, "editMessage", edit_message_then(a.id.clone(), a.text)).map(|_| json!({ "id": a.id }))
            }
            "update_message" => {
                let a: UpdateMessageArgs = parse(tool, args)?;
                self.log(&format!("update_message id={} source={} len={}", a.id, a.source, a.text.len()));
                reduce!(self, live, "updateMessage", update_message_then(a.id.clone(), a.text, a.source))
                    .map(|_| json!({ "id": a.id }))
            }
            "push_status" => {
                let a: PushStatusArgs = parse(tool, args)?;
                if !matches!(a.state.as_str(), "idle" | "thinking" | "tool_use") {
                    return Err(format!("invalid arguments for push_status: unknown state {}", a.state));
                }
                reduce!(self, live, "pushStatus", push_status_then(self.agent_id.clone(), a.state.clone()))
                    .map(|_| json!({ "state": a.state }))
            }
            "push_tool_event" => {
                let a: PushToolEventArgs = parse(tool, args)?;
                let id = format!("tool-{}-{}", now_ms(), short_uuid(8));
                reduce!(self, live, "pushToolEvent", push_tool_event_then(id.clone(), self.agent_id.clone(), a.tool, a.detail))
                    .map(|_| json!({ "id": id }))
            }
            "push_context_usage" => {
                let a: PushContextUsageArgs = parse(tool, args)?;
                reduce!(self, live, "pushContextUsage", push_context_usage_then(self.agent_id.clone(), a.used, a.window))
                    .map(|_| json!({}))
            }
            "request_permission" => {
                let a: RequestPermissionArgs = parse(tool, args)?;
                self.permissions.lock().unwrap().done.remove(&a.id);
                reduce!(self, live, "requestPermission", request_permission_then(a.id.clone(), self.agent_id.clone(), a.tool, a.input))
                    .map(|_| json!({ "id": a.id }))
            }
            "poll_permission" => {
                let a: PollArgs = parse(tool, args)?;
                let status = await_outcome(&self.permissions, &a.id, a.timeout_ms).await;
                return Ok(match status {
                    None => json!({ "status": "pending" }),
                    Some(s) if s == "allow" || s == "approved" => json!({ "status": "allow" }),
                    Some(_) => json!({ "status": "deny" }),
                });
            }
            "request_question" => {
                let a: RequestQuestionArgs = parse(tool, args)?;
                let mut ids = Vec::new();
                for q in a.questions {
                    let id = format!("question-{}-{}", now_ms(), short_uuid(8));
                    self.questions.lock().unwrap().done.remove(&id);
                    let options = serde_json::to_string(&q.options).unwrap_or_else(|_| "[]".into());
                    reduce!(
                        self,
                        live,
                        "requestQuestion",
                        request_question_then(id.clone(), self.agent_id.clone(), q.question, q.header, options, q.multi_select)
                    )
                    .map_err(|e| format!("{e:#}"))?;
                    ids.push(id);
                }
                Ok(json!({ "ids": ids }))
            }
            "poll_question" => {
                let a: PollArgs = parse(tool, args)?;
                let response = await_outcome(&self.questions, &a.id, a.timeout_ms).await;
                return Ok(match response {
                    None => json!({ "status": "pending" }),
                    Some(r) => json!({ "status": "answered", "response": r }),
                });
            }
            other => return Err(format!("unknown tool: {other}")),
        };
        result.map_err(|e| format!("{e:#}"))
    }

    async fn send_to_agent(&self, live: &Live, agent: &str, text: &str) -> Result<Value, String> {
        if agent.is_empty() || text.is_empty() {
            return Err("invalid arguments for send_to_agent: agent and text are required".into());
        }
        let cfg = live.conn.db().channel_config().iter().next();
        if cfg.as_ref().is_some_and(|c| !c.a_2_a_enabled) {
            return Err("REFUSED: agent-to-agent messaging is disabled by the kill switch - summarise to the user instead".into());
        }
        let cooldown = Duration::from_secs(cfg.as_ref().map(|c| c.a_2_a_cooldown_secs).unwrap_or(0) as u64);
        let hourly_limit = cfg.as_ref().map(|c| c.a_2_a_hourly_limit).unwrap_or(0) as usize;
        let max_hops = cfg.as_ref().map(|c| c.a_2_a_max_hops).unwrap_or(A2A_DEFAULT_MAX_HOPS);

        let now = Instant::now();
        let chain_hops = {
            let mut a2a = self.a2a.lock().unwrap();
            let inherited = a2a
                .incoming_hop
                .filter(|(_, at)| now.duration_since(*at) < A2A_HOP_DECAY)
                .map(|(hop, _)| hop);
            let chain_hops = inherited.map(|h| h + 1).unwrap_or(0);
            if max_hops > 0 && chain_hops >= max_hops {
                return Err(format!(
                    "REFUSED: agent reply chain reached {max_hops} hops - stop messaging agents and summarise the exchange to the user instead"
                ));
            }
            while a2a.send_times.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(3600)) {
                a2a.send_times.pop_front();
            }
            if hourly_limit > 0 && a2a.send_times.len() >= hourly_limit {
                return Err(format!(
                    "REFUSED: rate limited - {hourly_limit} agent messages per hour already sent; summarise to the user instead"
                ));
            }
            chain_hops
        };

        let registry: Vec<Agent> = live.conn.db().agent().iter().collect();
        let target = if registry.iter().any(|a| a.id == agent) {
            agent.to_string()
        } else {
            let matches: Vec<&Agent> = registry.iter().filter(|a| a.base_name == agent).collect();
            match matches.as_slice() {
                [one] => one.id.clone(),
                [] => {
                    let known: Vec<&str> = registry.iter().map(|a| a.id.as_str()).collect();
                    let known = if known.is_empty() { "none registered".to_string() } else { known.join(", ") };
                    return Err(format!("unknown agent '{agent}'. Registered agents: {known}"));
                }
                many => {
                    let ids: Vec<&str> = many.iter().map(|a| a.id.as_str()).collect();
                    return Err(format!("'{agent}' runs on multiple hosts — specify one of: {}", ids.join(", ")));
                }
            }
        };

        {
            let a2a = self.a2a.lock().unwrap();
            if let Some(last) = a2a.last_to_target.get(&target) {
                let since = now.duration_since(*last);
                if !cooldown.is_zero() && since < cooldown {
                    let wait = (cooldown - since).as_secs_f64().ceil() as u64;
                    return Err(format!(
                        "REFUSED: rate limited - already messaged {target} in the last {}s, retry in {wait}s or batch your points into one message",
                        cooldown.as_secs()
                    ));
                }
            }
        }

        let id = format!("a2a-{chain_hops}-{}", now_ms());
        reduce!(
            self,
            live,
            "pushMessage",
            push_message_then(id.clone(), target.clone(), "user".to_string(), text.to_string(), format!("agent:{}", self.agent_id))
        )
        .map_err(|e| format!("{e:#}"))?;
        let mut a2a = self.a2a.lock().unwrap();
        a2a.send_times.push_back(now);
        a2a.last_to_target.insert(target.clone(), now);
        Ok(json!({ "text": format!("sent to {target} (id: {id}, chain hop {chain_hops})") }))
    }

    pub async fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(live) = self.current() else {
            return;
        };
        let ended: anyhow::Result<()> = async { reduce!(self, live, "endAgent", end_agent_then(self.agent_id.clone())) }.await;
        match ended {
            Ok(()) => self.log(&format!("Agent ended: {}", self.agent_id)),
            Err(err) => self.log(&format!("endAgent on shutdown failed: {err:#}")),
        }
        let _ = live.conn.disconnect();
    }
}

fn settle<T>(outcomes: &Mutex<Outcomes<T>>, id: &str, value: T) {
    let mut o = outcomes.lock().unwrap();
    if let Some(waiter) = o.waiting.remove(id) {
        let _ = waiter.send(value);
        return;
    }
    o.done.insert(id.to_string(), value);
}

async fn await_outcome<T>(outcomes: &Mutex<Outcomes<T>>, id: &str, timeout_ms: u64) -> Option<T> {
    let rx = {
        let mut o = outcomes.lock().unwrap();
        if let Some(value) = o.done.remove(id) {
            return Some(value);
        }
        let (tx, rx) = oneshot::channel();
        o.waiting.insert(id.to_string(), tx);
        rx
    };
    match timeout(Duration::from_millis(timeout_ms.min(POLL_MAX_MS)), rx).await {
        Ok(Ok(value)) => Some(value),
        _ => {
            outcomes.lock().unwrap().waiting.remove(id);
            None
        }
    }
}

async fn resolve_file_path(conn: &DbConnection, id: &str) -> Option<String> {
    if let Some(file) = conn.db().space_file().id().find(&id.to_string()) {
        return Some(file.path);
    }
    let (tx, rx) = oneshot::channel();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let wanted = id.to_string();
    let handle = conn
        .subscription_builder()
        .on_applied(move |ctx| {
            let path = ctx.db().space_file().id().find(&wanted).map(|f| f.path);
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(path);
            }
        })
        .subscribe(vec![format!("SELECT * FROM space_file WHERE id = '{id}'")]);
    let path = timeout(VAULT_RESOLVE_TIMEOUT, rx).await.ok().and_then(|r| r.ok()).flatten();
    let _ = handle.unsubscribe();
    path
}

fn encode_segment(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'!' | b'*' | b'\'' | b'(' | b')' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn download(url: &str, target: &std::path::Path) -> anyhow::Result<()> {
    let response = ureq::get(url).timeout(Duration::from_secs(30)).call().map_err(|e| anyhow!("{e}"))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)?;
    std::fs::write(target, bytes)?;
    Ok(())
}
