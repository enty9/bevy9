use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::project::{discover_engine_root, load_project_summary};

const MAX_LOG_ENTRIES: usize = 500;
const PREFERRED_BRP_PORT: u16 = 15702;
const PREFERRED_IPC_PORT: u16 = 15704;
const READY_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineHostState {
    Stopped,
    Starting,
    Running,
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineHostStatus {
    pub state: EngineHostState,
    pub project_dir: Option<String>,
    pub project_name: Option<String>,
    pub brp_url: Option<String>,
    pub ipc_url: Option<String>,
    pub brp_port: Option<u16>,
    pub ipc_port: Option<u16>,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
}

impl Default for EngineHostStatus {
    fn default() -> Self {
        Self {
            state: EngineHostState::Stopped,
            project_dir: None,
            project_name: None,
            brp_url: None,
            ipc_url: None,
            brp_port: None,
            ipc_port: None,
            pid: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineLogMessage {
    pub timestamp_ms: u64,
    pub level: String,
    pub target: String,
    pub message: String,
}

struct SupervisorInner {
    generation: u64,
    status: EngineHostStatus,
    child: Option<Child>,
    logs: VecDeque<EngineLogMessage>,
}

impl Default for SupervisorInner {
    fn default() -> Self {
        Self {
            generation: 0,
            status: EngineHostStatus::default(),
            child: None,
            logs: VecDeque::with_capacity(MAX_LOG_ENTRIES),
        }
    }
}

#[derive(Clone)]
pub struct EngineSupervisor {
    inner: Arc<Mutex<SupervisorInner>>,
}

impl Default for EngineSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EngineSupervisor {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            let _ = self.stop_internal(None);
        }
    }
}

impl EngineSupervisor {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SupervisorInner::default())),
        }
    }

    pub fn status(&self) -> EngineHostStatus {
        self.inner
            .lock()
            .map(|guard| guard.status.clone())
            .unwrap_or_default()
    }

    pub fn recent_logs(&self) -> Vec<EngineLogMessage> {
        self.inner
            .lock()
            .map(|guard| guard.logs.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn clear_logs(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.logs.clear();
        }
    }

    pub fn stop(&self, app: &AppHandle) -> EngineHostStatus {
        self.stop_internal(Some(app))
    }

    pub fn restart(&self, app: &AppHandle) -> Result<EngineHostStatus, String> {
        let project_dir = self
            .status()
            .project_dir
            .ok_or_else(|| "Cannot restart Engine Host: no active project selected".to_owned())?;
        self.start(app, &project_dir)
    }

    pub fn start(&self, app: &AppHandle, project_path: &str) -> Result<EngineHostStatus, String> {
        let summary =
            load_project_summary(Path::new(project_path)).map_err(|err| format!("{err:#}"))?;

        let (my_gen, previous_child) = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| "supervisor state poisoned".to_owned())?;
            guard.generation = guard.generation.wrapping_add(1);
            let my_gen = guard.generation;
            let previous_child = guard.child.take();
            guard.status = EngineHostStatus {
                state: EngineHostState::Starting,
                project_dir: Some(summary.root_dir.clone()),
                project_name: Some(summary.name.clone()),
                brp_url: None,
                ipc_url: None,
                brp_port: None,
                ipc_port: None,
                pid: None,
                last_error: None,
            };
            (my_gen, previous_child)
        };

        if let Some(mut child) = previous_child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.emit_status(app);
        self.push_log(
            Some(app),
            "info",
            "supervisor",
            format!("Starting Engine Host for project '{}'...", summary.name),
        );

        let engine_root = discover_engine_root();
        let runtime_bin =
            match resolve_or_build_runtime_binary(self, app, engine_root.as_deref(), my_gen) {
                Ok(bin) => bin,
                Err(err) => {
                    self.mark_failed(Some(app), my_gen, err.clone());
                    return Err(err);
                }
            };

        if !self.is_generation(my_gen) {
            return Err("Engine Host startup was superseded".to_owned());
        }

        let brp_port = allocate_port(PREFERRED_BRP_PORT);
        let ipc_port = allocate_port(PREFERRED_IPC_PORT);
        let brp_url = format!("http://127.0.0.1:{brp_port}");
        let ipc_url = format!("ws://127.0.0.1:{ipc_port}");

        let mut command = Command::new(&runtime_bin);
        command
            .arg("--mode")
            .arg("editor")
            .arg("--project")
            .arg(&summary.root_dir)
            .arg("--brp-port")
            .arg(brp_port.to_string())
            .arg("--ipc-port")
            .arg(ipc_port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(dir) = &engine_root {
            command.current_dir(dir);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                let msg = format!("Failed to spawn '{}': {err}", runtime_bin.display());
                self.mark_failed(Some(app), my_gen, msg.clone());
                return Err(msg);
            }
        };

        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| "supervisor state poisoned".to_owned())?;
            if guard.generation != my_gen {
                drop(guard);
                let _ = child.kill();
                let _ = child.wait();
                return Err("Engine Host startup was cancelled".to_owned());
            }
            guard.status.brp_port = Some(brp_port);
            guard.status.ipc_port = Some(ipc_port);
            guard.status.brp_url = Some(brp_url);
            guard.status.ipc_url = Some(ipc_url);
            guard.status.pid = Some(pid);
            guard.child = Some(child);
        }
        self.emit_status(app);

        if let Some(out) = stdout {
            self.spawn_stream_reader(app.clone(), my_gen, out, false);
        }
        if let Some(err) = stderr {
            self.spawn_stream_reader(app.clone(), my_gen, err, true);
        }

        if let Err(err) = self.wait_until_ready(app, my_gen, brp_port) {
            self.mark_failed(Some(app), my_gen, err.clone());
            return Err(err);
        }

        let status = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| "supervisor state poisoned".to_owned())?;
            if guard.generation != my_gen {
                return Err("Engine Host startup was cancelled".to_owned());
            }
            guard.status.state = EngineHostState::Running;
            guard.status.clone()
        };

        self.emit_status(app);
        self.push_log(
            Some(app),
            "info",
            "supervisor",
            format!(
                "Engine Host ready (pid={pid}, brp=127.0.0.1:{brp_port}, ipc=127.0.0.1:{ipc_port})"
            ),
        );

        Ok(status)
    }

    fn stop_internal(&self, app: Option<&AppHandle>) -> EngineHostStatus {
        let (child_to_kill, status) = {
            let Ok(mut guard) = self.inner.lock() else {
                return EngineHostStatus::default();
            };
            guard.generation = guard.generation.wrapping_add(1);
            let child = guard.child.take();
            guard.status.state = EngineHostState::Stopped;
            guard.status.pid = None;
            guard.status.brp_port = None;
            guard.status.ipc_port = None;
            guard.status.brp_url = None;
            guard.status.ipc_url = None;
            guard.status.last_error = None;
            (child, guard.status.clone())
        };

        if let Some(mut child) = child_to_kill {
            let _ = child.kill();
            let _ = child.wait();
        }

        if let Some(app_handle) = app {
            let _ = app_handle.emit("engine-status", &status);
            self.push_log(
                Some(app_handle),
                "info",
                "supervisor",
                "Engine Host stopped".to_owned(),
            );
        }

        status
    }

    fn is_generation(&self, expected: u64) -> bool {
        self.inner
            .lock()
            .is_ok_and(|guard| guard.generation == expected)
    }

    fn emit_status(&self, app: &AppHandle) {
        let status = self.status();
        let _ = app.emit("engine-status", &status);
    }

    fn push_log(&self, app: Option<&AppHandle>, level: &str, target: &str, message: String) {
        let entry = EngineLogMessage {
            timestamp_ms: now_millis(),
            level: level.to_owned(),
            target: target.to_owned(),
            message,
        };
        if let Ok(mut guard) = self.inner.lock() {
            if guard.logs.len() >= MAX_LOG_ENTRIES {
                guard.logs.pop_front();
            }
            guard.logs.push_back(entry.clone());
        }
        if let Some(app_handle) = app {
            let _ = app_handle.emit("engine-log", &entry);
        }
    }

    fn mark_failed(&self, app: Option<&AppHandle>, expected_gen: u64, reason: String) {
        let child_to_kill = {
            let Ok(mut guard) = self.inner.lock() else {
                return;
            };
            if guard.generation != expected_gen {
                return;
            }
            let child = guard.child.take();
            guard.status.state = EngineHostState::Crashed;
            guard.status.pid = None;
            guard.status.last_error = Some(reason.clone());
            child
        };

        if let Some(mut child) = child_to_kill {
            let _ = child.kill();
            let _ = child.wait();
        }

        if let Some(app_handle) = app {
            self.emit_status(app_handle);
            self.push_log(Some(app_handle), "error", "supervisor", reason);
        }
    }

    fn spawn_stream_reader<R: Read + Send + 'static>(
        &self,
        app: AppHandle,
        my_gen: u64,
        reader: R,
        watch_exit: bool,
    ) {
        let supervisor = self.clone();
        thread::Builder::new()
            .name(if watch_exit {
                "engine-stderr-log".to_owned()
            } else {
                "engine-stdout-log".to_owned()
            })
            .spawn(move || {
                let buf = BufReader::new(reader);
                for line in buf.lines().map_while(Result::ok) {
                    if !supervisor.is_generation(my_gen) {
                        break;
                    }
                    let cleaned = strip_ansi_codes(&line);
                    let trimmed = cleaned.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let (level, target, message) = parse_log_line(trimmed, watch_exit);
                    supervisor.push_log(Some(&app), level, target, message);
                }

                if watch_exit {
                    supervisor.handle_unexpected_exit(&app, my_gen);
                }
            })
            .ok();
    }

    fn handle_unexpected_exit(&self, app: &AppHandle, expected_gen: u64) {
        let mut status_snapshot = None;
        let mut crashed_msg = None;

        if let Ok(mut guard) = self.inner.lock()
            && guard.generation == expected_gen
            && guard.status.state == EngineHostState::Running
        {
            let exit_desc = if let Some(child) = guard.child.as_mut() {
                match child.wait() {
                    Ok(exit) => format!("Engine Host exited unexpectedly ({exit})"),
                    Err(err) => format!("Engine Host terminated unexpectedly: {err}"),
                }
            } else {
                "Engine Host terminated unexpectedly".to_owned()
            };
            guard.child = None;
            guard.status.state = EngineHostState::Crashed;
            guard.status.pid = None;
            guard.status.last_error = Some(exit_desc.clone());
            status_snapshot = Some(guard.status.clone());
            crashed_msg = Some(exit_desc);
        }

        if let Some(status) = status_snapshot {
            let _ = app.emit("engine-status", &status);
        }
        if let Some(msg) = crashed_msg {
            self.push_log(Some(app), "error", "supervisor", msg);
        }
    }

    fn wait_until_ready(
        &self,
        _app: &AppHandle,
        expected_gen: u64,
        brp_port: u16,
    ) -> Result<(), String> {
        let deadline = Instant::now() + READY_TIMEOUT;
        while Instant::now() < deadline {
            {
                let mut guard = self
                    .inner
                    .lock()
                    .map_err(|_| "supervisor state poisoned".to_owned())?;
                if guard.generation != expected_gen {
                    return Err("Engine Host startup was cancelled".to_owned());
                }
                if let Some(child) = guard.child.as_mut()
                    && let Ok(Some(exit_status)) = child.try_wait()
                {
                    return Err(format!(
                        "Engine Host exited before BRP became ready ({exit_status})"
                    ));
                }
            }

            if probe_brp_ready(brp_port) {
                return Ok(());
            }

            thread::sleep(Duration::from_millis(100));
        }

        Err(format!(
            "Timed out waiting {}s for Engine Host BRP on 127.0.0.1:{brp_port}",
            READY_TIMEOUT.as_secs()
        ))
    }
}

fn resolve_or_build_runtime_binary(
    supervisor: &EngineSupervisor,
    app: &AppHandle,
    engine_root: Option<&Path>,
    my_gen: u64,
) -> Result<PathBuf, String> {
    if let Some(explicit) = std::env::var_os("BEVY9_RUNTIME_BIN") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Ok(path);
        }
    }

    let bin_name = if cfg!(windows) {
        "engine_runtime.exe"
    } else {
        "engine_runtime"
    };

    if let Some(root) = engine_root {
        let debug_bin = root.join("target/debug").join(bin_name);
        if debug_bin.is_file() {
            return Ok(debug_bin);
        }
        let release_bin = root.join("target/release").join(bin_name);
        if release_bin.is_file() {
            return Ok(release_bin);
        }

        supervisor.push_log(
            Some(app),
            "info",
            "supervisor",
            "engine_runtime binary not built yet; running `cargo build -p engine_runtime`..."
                .to_owned(),
        );

        let manifest_path = root.join("Cargo.toml");
        let output = Command::new("cargo")
            .arg("build")
            .arg("--manifest-path")
            .arg(&manifest_path)
            .arg("-p")
            .arg("engine_runtime")
            .current_dir(root)
            .output()
            .map_err(|err| format!("Failed to invoke `cargo build -p engine_runtime`: {err}"))?;

        if !supervisor.is_generation(my_gen) {
            return Err("Engine Host build cancelled".to_owned());
        }

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("`cargo build -p engine_runtime` failed:\n{stderr}"));
        }

        if debug_bin.is_file() {
            return Ok(debug_bin);
        }
    }

    Err("Could not locate `engine_runtime` executable; set BEVY9_RUNTIME_BIN or run from the repository".to_owned())
}

fn allocate_port(preferred: u16) -> u16 {
    if let Ok(listener) = TcpListener::bind(("127.0.0.1", preferred)) {
        return listener.local_addr().map_or(preferred, |addr| addr.port());
    }
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map_or(preferred, |addr| addr.port())
}

fn probe_brp_ready(port: u16) -> bool {
    let addr = ([127, 0, 0, 1], port).into();
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(150)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(300)));

    let body = r#"{"jsonrpc":"2.0","id":0,"method":"engine.world.tree"}"#;
    let req = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }

    let mut buf = [0u8; 512];
    match stream.read(&mut buf) {
        Ok(read_bytes) if read_bytes > 0 => {
            let head = String::from_utf8_lossy(&buf[..read_bytes]);
            head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200")
        }
        _ => false,
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn strip_ansi_codes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for code_ch in chars.by_ref() {
                if ('@'..='~').contains(&code_ch) {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn parse_log_line(line: &str, default_stderr: bool) -> (&'static str, &str, String) {
    for (token, level) in [
        (" ERROR ", "error"),
        (" WARN ", "warn"),
        (" INFO ", "info"),
        (" DEBUG ", "debug"),
        (" TRACE ", "trace"),
    ] {
        if let Some((_, rest)) = line.split_once(token) {
            let rest_trimmed = rest.trim();
            if let Some((target, msg)) = rest_trimmed.split_once(": ") {
                return (level, target.trim(), msg.trim().to_owned());
            }
            return (level, "engine_runtime", rest_trimmed.to_owned());
        }
    }

    let level = if default_stderr { "warn" } else { "info" };
    (level, "engine_runtime", line.to_owned())
}
