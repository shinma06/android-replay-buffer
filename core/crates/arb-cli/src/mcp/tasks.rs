//! Per-stdio-session task lifecycle. Pending result requests are answered by the
//! worker on completion, so a waiting client may still cancel on the same pipe.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::{Failure, McpServer, rpc_error, rpc_ok, write_message};
const MAX_TASKS: usize = 64;
const MAX_RETENTION_MS: u64 = 3_600_000;
const RELATED: &str = "io.modelcontextprotocol/related-task";
const LEVELS: [&str; 8] = [
    "debug",
    "info",
    "notice",
    "warning",
    "error",
    "critical",
    "alert",
    "emergency",
];

#[derive(Default)]
pub(super) struct Activity {
    tool: String,
    token: Option<Value>,
    progress: u64,
    task: Option<String>,
}

impl Activity {
    pub(super) fn new(tool: &str, token: Option<Value>) -> Self {
        Self {
            tool: tool.into(),
            token,
            ..Self::default()
        }
    }
    fn emit(&self, method: &str, mut params: Value) {
        if let Some(id) = &self.task {
            params["_meta"][RELATED] = json!({"taskId":id});
        }
        let _ = write_message(&json!({"jsonrpc":"2.0","method":method,"params":params}));
    }
    pub(super) fn check(&self, s: &Session) -> Result<(), Failure> {
        if s.closed.load(Ordering::Acquire)
            || self.task.as_ref().is_some_and(|id| {
                s.tasks
                    .lock()
                    .expect("task registry")
                    .get(id)
                    .is_none_or(|t| t.result.is_some())
            })
        {
            return Err(cancelled());
        }
        Ok(())
    }
    pub(super) fn notify(&mut self, s: &Session, phase: &str) {
        if self.check(s).is_err() {
            return;
        }
        if let Some(token) = &self.token {
            self.progress += 1;
            self.emit(
                "notifications/progress",
                json!({"progressToken":token,"progress":self.progress,"message":phase}),
            );
        }
        if self.token.is_none() {
            self.log(s, 0, phase);
        }
    }
    fn log(&self, s: &Session, level: u8, phase: &str) {
        if !self.tool.is_empty() && level >= s.level.load(Ordering::Relaxed) {
            self.emit("notifications/message",json!({"level":LEVELS[usize::from(level)],"logger":"android-replay-buffer","data":{"tool":self.tool,"phase":phase}}));
        }
    }
    pub(super) fn finish(&mut self, s: &Session, error: Option<&Failure>) {
        if self.check(s).is_err() {
            return;
        }
        if error.is_none() {
            self.notify(s, "完了");
        } else {
            self.notify(s, "失敗");
            self.log(s, 4, "失敗");
        }
    }
}

fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    arb_core::time::format_unix_secs_iso8601(i64::try_from(secs).unwrap_or(i64::MAX))
}
fn cancelled() -> Failure {
    Failure::new(-32800, "CANCELLED", "タスクはキャンセルされました")
}

struct Task {
    view: Value,
    expires: Instant,
    result: Option<Arc<Value>>,
    waiters: Vec<Value>,
    running: bool,
}
impl Task {
    fn settle(
        &mut self,
        status: &str,
        message: &str,
        mut result: Value,
    ) -> (Vec<Value>, Arc<Value>) {
        self.view["status"] = json!(status);
        self.view["statusMessage"] = json!(message);
        self.view["lastUpdatedAt"] = json!(now());
        result["_meta"][RELATED] = json!({"taskId":self.view["taskId"]});
        let result = Arc::new(result);
        self.result = Some(Arc::clone(&result));
        (std::mem::take(&mut self.waiters), result)
    }
}

pub(super) struct Session {
    level: AtomicU8,
    seq: AtomicU64,
    closed: AtomicBool,
    tasks: Mutex<BTreeMap<String, Task>>,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            level: AtomicU8::new(8),
            seq: AtomicU64::new(1),
            closed: AtomicBool::default(),
            tasks: Mutex::default(),
        }
    }
}

fn send_replies((ids, r): (Vec<Value>, Arc<Value>)) {
    for id in ids {
        let _ = write_message(&rpc_ok(&id, &r));
    }
}
impl Session {
    pub(super) fn set_level(&self, id: &Value, p: &Value) -> Value {
        if let Some(n) = LEVELS.iter().position(|l| p["level"] == *l) {
            self.level
                .store(u8::try_from(n).expect("eight levels"), Ordering::Relaxed);
            rpc_ok(id, &json!({}))
        } else {
            rpc_error(id, -32602, "未知のログレベルです")
        }
    }
    pub(super) fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        self.tasks.lock().expect("task registry").clear();
    }
    pub(super) fn reap(&self) {
        let now = Instant::now();
        self.tasks
            .lock()
            .expect("task registry")
            .retain(|_, t| t.running || t.expires > now);
    }
    pub(super) fn request(&self, method: &str, id: &Value, p: &Value) -> Option<Value> {
        self.reap();
        let mut tasks = self.tasks.lock().expect("task registry");
        if method == "tasks/list" {
            return Some(rpc_ok(
                id,
                &json!({"tasks":tasks.values().map(|t|&t.view).collect::<Vec<_>>()}),
            ));
        }
        let Some(key) = p.get("taskId") else {
            return Some(rpc_error(id, -32602, "taskId は必須です"));
        };
        let Some(key) = key.as_str() else {
            return Some(rpc_error(id, -32602, "taskId は文字列で指定してください"));
        };
        let Some(t) = tasks.get_mut(key) else {
            return Some(rpc_error(id, -32602, "未知または期限切れの taskId です"));
        };
        Some(match method {
            "tasks/get" => rpc_ok(id, &t.view),
            "tasks/cancel" if t.result.is_none() => {
                let replies = t.settle("cancelled", "キャンセル", cancelled().result());
                let view = t.view.clone();
                drop(tasks);
                send_replies(replies);
                rpc_ok(id, &view)
            }
            "tasks/cancel" => rpc_error(id, -32602, "終了済みタスクです"),
            _ => {
                if let Some(r) = &t.result {
                    rpc_ok(id, r)
                } else if t.waiters.len() >= MAX_TASKS {
                    rpc_error(id, -32603, "待機要求が上限に達しました")
                } else {
                    t.waiters.push(id.clone());
                    return None;
                }
            }
        })
    }
    fn finish(&self, id: &str, result: Value) {
        let mut tasks = self.tasks.lock().expect("task registry");
        if let Some(t) = tasks.get_mut(id) {
            t.running = false;
            if t.result.is_none() {
                let failed = result["isError"] == true;
                let (status, msg) = if failed {
                    ("failed", "実行に失敗しました")
                } else {
                    ("completed", "完了")
                };
                let replies = t.settle(status, msg, result);
                drop(tasks);
                send_replies(replies);
            }
        }
    }
}

impl McpServer {
    pub(super) fn start_task(
        &self,
        id: &Value,
        params: &Value,
        args: Value,
        mut activity: Activity,
    ) -> Value {
        if !params.is_object()
            || params
                .get("ttl")
                .is_some_and(|v| !v.is_null() && !v.is_u64())
        {
            return rpc_error(id, -32602, "task.ttl はミリ秒単位の非負整数です");
        }
        self.session.reap();
        let ttl = params["ttl"]
            .as_u64()
            .unwrap_or(60_000)
            .clamp(1, MAX_RETENTION_MS);
        let key = self.session.seq.fetch_add(1, Ordering::Relaxed).to_string();
        let created = now();
        let view = json!({"taskId":key,"status":"working","statusMessage":"実行中","createdAt":created,"lastUpdatedAt":created,"ttl":ttl,"pollInterval":100});
        {
            let mut tasks = self.session.tasks.lock().expect("task registry");
            if tasks.len() >= MAX_TASKS {
                return rpc_error(id, -32603, "保持中のタスクが上限に達しました");
            }
            tasks.insert(
                key.clone(),
                Task {
                    view: view.clone(),
                    expires: Instant::now() + Duration::from_millis(ttl),
                    result: None,
                    waiters: Vec::new(),
                    running: true,
                },
            );
        }
        let name = activity.tool.clone();
        activity.task = Some(key.clone());
        let mut worker = Self {
            paths: self.paths.clone(),
            client: None,
            version: self.version.clone(),
            session: Arc::clone(&self.session),
            activity,
            external_bundles: BTreeMap::new(),
        };
        let worker_key = key.clone();
        let spawned = std::thread::Builder::new()
            .name("mcp-task".into())
            .spawn(move || {
                let def = super::contract::for_version(&worker.version)
                    .iter()
                    .find(|d| d["name"] == name)
                    .expect("accepted tool");
                let output = worker.run_tool(def, args);
                worker.session.finish(&worker_key, output);
            });
        if let Err(e) = spawned {
            self.session
                .tasks
                .lock()
                .expect("task registry")
                .remove(&key);
            return rpc_error(id, -32603, e.to_string());
        }
        rpc_ok(id, &json!({"task":view,"_meta":{(RELATED):{"taskId":key}}}))
    }
}
