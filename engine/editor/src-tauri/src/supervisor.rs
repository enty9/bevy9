use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::project::{discover_engine_root, load_project_summary};

const MAX_LOG_ENTRIES: usize = 500;
const MAX_RECENT_STDERR_LINES: usize = 24;
const STDERR_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
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
    recent_stderr: VecDeque<String>,
    verified_build: Option<(PathBuf, SystemTime)>,
}

impl Default for SupervisorInner {
    fn default() -> Self {
        Self {
            generation: 0,
            status: EngineHostStatus::default(),
            child: None,
            logs: VecDeque::with_capacity(MAX_LOG_ENTRIES),
            recent_stderr: VecDeque::with_capacity(MAX_RECENT_STDERR_LINES),
            verified_build: None,
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
            guard.recent_stderr.clear();
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
            self.spawn_stream_reader(app.clone(), my_gen, out, false, None);
        }
        let stderr_done = stderr.map(|err| {
            let signal = Arc::new((Mutex::new(false), Condvar::new()));
            self.spawn_stream_reader(app.clone(), my_gen, err, true, Some(Arc::clone(&signal)));
            signal
        });

        if let Err(err) = self.wait_until_ready(app, my_gen, brp_port, stderr_done.as_ref()) {
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

    fn record_stderr_line(&self, expected_gen: u64, line: &str) {
        if let Ok(mut guard) = self.inner.lock()
            && guard.generation == expected_gen
        {
            if guard.recent_stderr.len() >= MAX_RECENT_STDERR_LINES {
                guard.recent_stderr.pop_front();
            }
            guard.recent_stderr.push_back(line.to_owned());
        }
    }

    fn recent_stderr_for_generation(&self, expected_gen: u64) -> Vec<String> {
        self.inner
            .lock()
            .ok()
            .filter(|guard| guard.generation == expected_gen)
            .map(|guard| guard.recent_stderr.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn verified_build_for(&self, bin_path: &Path) -> Option<SystemTime> {
        self.inner
            .lock()
            .ok()
            .and_then(|guard| guard.verified_build.clone())
            .and_then(|(path, mtime)| (path == bin_path).then_some(mtime))
    }

    fn set_verified_build(&self, bin_path: PathBuf, source_mtime: SystemTime) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.verified_build = Some((bin_path, source_mtime));
        }
    }

    fn spawn_stream_reader<R: Read + Send + 'static>(
        &self,
        app: AppHandle,
        my_gen: u64,
        reader: R,
        watch_exit: bool,
        done_signal: Option<Arc<(Mutex<bool>, Condvar)>>,
    ) {
        let supervisor = self.clone();
        let thread_signal = done_signal.clone();
        let spawned = thread::Builder::new()
            .name(if watch_exit {
                "engine-stderr-log".to_owned()
            } else {
                "engine-stdout-log".to_owned()
            })
            .spawn(move || {
                struct SignalOnDrop(Option<Arc<(Mutex<bool>, Condvar)>>);
                impl Drop for SignalOnDrop {
                    fn drop(&mut self) {
                        if let Some(signal) = self.0.take()
                            && let Ok(mut done) = signal.0.lock()
                        {
                            *done = true;
                            signal.1.notify_all();
                        }
                    }
                }

                let _signal_guard = SignalOnDrop(thread_signal);
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
                    if watch_exit {
                        supervisor.record_stderr_line(my_gen, trimmed);
                    }
                    let (level, target, message) = parse_log_line(trimmed, watch_exit);
                    supervisor.push_log(Some(&app), level, target, message);
                }

                drop(_signal_guard);

                if watch_exit {
                    supervisor.handle_unexpected_exit(&app, my_gen);
                }
            });

        if spawned.is_err()
            && let Some(signal) = done_signal
            && let Ok(mut done) = signal.0.lock()
        {
            *done = true;
            signal.1.notify_all();
        }
    }

    fn handle_unexpected_exit(&self, app: &AppHandle, expected_gen: u64) {
        let mut status_snapshot = None;
        let mut crashed_msg = None;

        if let Ok(mut guard) = self.inner.lock()
            && guard.generation == expected_gen
            && guard.status.state == EngineHostState::Running
        {
            let base_desc = if let Some(child) = guard.child.as_mut() {
                match child.wait() {
                    Ok(exit) => format!("Engine Host exited unexpectedly ({exit})"),
                    Err(err) => format!("Engine Host terminated unexpectedly: {err}"),
                }
            } else {
                "Engine Host terminated unexpectedly".to_owned()
            };
            let stderr_lines: Vec<String> = guard.recent_stderr.iter().cloned().collect();
            let exit_desc = append_stderr_summary(&base_desc, &stderr_lines);
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
        stderr_done: Option<&Arc<(Mutex<bool>, Condvar)>>,
    ) -> Result<(), String> {
        let deadline = Instant::now() + READY_TIMEOUT;
        while Instant::now() < deadline {
            let exited_status: Option<ExitStatus> = {
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
                    Some(exit_status)
                } else {
                    None
                }
            };

            if let Some(exit_status) = exited_status {
                if let Some(signal) = stderr_done {
                    wait_for_stderr_drain(signal, STDERR_DRAIN_TIMEOUT);
                }
                let stderr_lines = self.recent_stderr_for_generation(expected_gen);
                return Err(format_early_exit_error(exit_status, &stderr_lines));
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

fn wait_for_stderr_drain(signal: &(Mutex<bool>, Condvar), timeout: Duration) {
    let Ok(mut done) = signal.0.lock() else {
        return;
    };
    let deadline = Instant::now() + timeout;
    while !*done {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Ok((guard, wait_result)) = signal.1.wait_timeout(done, remaining) else {
            break;
        };
        done = guard;
        if wait_result.timed_out() {
            break;
        }
    }
}

fn format_early_exit_error(exit_status: ExitStatus, stderr_lines: &[String]) -> String {
    let base = format!("Engine Host exited before BRP became ready ({exit_status})");
    append_stderr_summary(&base, stderr_lines)
}

fn append_stderr_summary(base: &str, stderr_lines: &[String]) -> String {
    let summary = summarize_stderr_lines(stderr_lines);
    if summary.is_empty() {
        base.to_owned()
    } else {
        format!("{base}: {summary}")
    }
}

fn summarize_stderr_lines(stderr_lines: &[String]) -> String {
    const MAX_LINES: usize = 4;
    const MAX_CHARS: usize = 600;

    let non_empty: Vec<&str> = stderr_lines
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if non_empty.is_empty() {
        return String::new();
    }

    // Prefer lines starting from the last explicit failure/panic marker when present.
    let start_idx = non_empty
        .iter()
        .rposition(|line| {
            line.contains("engine_runtime failed:")
                || line.contains("panicked at")
                || line.contains(" ERROR ")
        })
        .unwrap_or_else(|| non_empty.len().saturating_sub(MAX_LINES));

    let end_idx = (start_idx + MAX_LINES).min(non_empty.len());
    let mut joined = non_empty[start_idx..end_idx].join(" | ");
    if joined.chars().count() > MAX_CHARS {
        joined = joined.chars().take(MAX_CHARS).collect::<String>();
        joined.push_str("...");
    }
    joined
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
        let prefer_release = !cfg!(debug_assertions);
        let debug_bin = root.join("target/debug").join(bin_name);
        let release_bin = root.join("target/release").join(bin_name);
        let (preferred_bin, fallback_bin) = if prefer_release {
            (&release_bin, &debug_bin)
        } else {
            (&debug_bin, &release_bin)
        };

        let newest_source_mtime = latest_runtime_source_mtime(root);

        if is_runtime_binary_fresh(
            preferred_bin,
            newest_source_mtime,
            supervisor.verified_build_for(preferred_bin),
        ) {
            return Ok(preferred_bin.clone());
        }

        if !preferred_bin.is_file()
            && is_runtime_binary_fresh(
                fallback_bin,
                newest_source_mtime,
                supervisor.verified_build_for(fallback_bin),
            )
        {
            return Ok(fallback_bin.clone());
        }

        let build_cmd_label = if prefer_release {
            "cargo build --release -p engine_runtime"
        } else {
            "cargo build -p engine_runtime"
        };
        let reason = if preferred_bin.is_file() {
            format!("engine_runtime binary is out of date; running `{build_cmd_label}`...")
        } else {
            format!("engine_runtime binary not built yet; running `{build_cmd_label}`...")
        };
        supervisor.push_log(Some(app), "info", "supervisor", reason);

        let manifest_path = root.join("Cargo.toml");
        let mut cmd = Command::new("cargo");
        cmd.arg("build");
        if prefer_release {
            cmd.arg("--release");
        }
        cmd.arg("--manifest-path")
            .arg(&manifest_path)
            .arg("-p")
            .arg("engine_runtime")
            .current_dir(root)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut build_child = cmd
            .spawn()
            .map_err(|err| format!("Failed to invoke `{build_cmd_label}`: {err}"))?;

        let mut stderr_tail = VecDeque::with_capacity(40);
        if let Some(stderr) = build_child.stderr.take() {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if !supervisor.is_generation(my_gen) {
                    let _ = build_child.kill();
                    let _ = build_child.wait();
                    return Err("Engine Host build cancelled".to_owned());
                }
                let cleaned = strip_ansi_codes(&line);
                let trimmed = cleaned.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if stderr_tail.len() >= 40 {
                    stderr_tail.pop_front();
                }
                stderr_tail.push_back(trimmed.to_owned());
                let level = if trimmed.starts_with("error") {
                    "error"
                } else {
                    "info"
                };
                supervisor.push_log(Some(app), level, "cargo", trimmed.to_owned());
            }
        }

        let status = build_child
            .wait()
            .map_err(|err| format!("Failed while waiting for `{build_cmd_label}`: {err}"))?;

        if !supervisor.is_generation(my_gen) {
            return Err("Engine Host build cancelled".to_owned());
        }

        if !status.success() {
            let tail_text = stderr_tail
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            return Err(format!("`{build_cmd_label}` failed ({status}):\n{tail_text}"));
        }

        if preferred_bin.is_file() {
            if let Some(source_mtime) = newest_source_mtime {
                supervisor.set_verified_build(preferred_bin.clone(), source_mtime);
            }
            return Ok(preferred_bin.clone());
        }
    }

    Err("Could not locate `engine_runtime` executable; set BEVY9_RUNTIME_BIN or run from the repository".to_owned())
}

fn is_runtime_binary_fresh(
    bin_path: &Path,
    newest_source_mtime: Option<SystemTime>,
    verified_source_mtime: Option<SystemTime>,
) -> bool {
    if !bin_path.is_file() {
        return false;
    }
    let Some(newest_source) = newest_source_mtime else {
        return true;
    };
    let Ok(bin_mtime) = fs::metadata(bin_path).and_then(|meta| meta.modified()) else {
        return false;
    };
    let effective_mtime = verified_source_mtime.map_or(bin_mtime, |verified| bin_mtime.max(verified));
    effective_mtime >= newest_source
}

fn latest_runtime_source_mtime(engine_root: &Path) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;

    for top_file in ["Cargo.toml", "Cargo.lock"] {
        record_file_mtime(&engine_root.join(top_file), &mut newest);
    }
    scan_dir_mtime(&engine_root.join(".cargo"), &mut newest);
    scan_dir_mtime(&engine_root.join("crates"), &mut newest);

    newest
}

fn record_file_mtime(path: &Path, newest: &mut Option<SystemTime>) {
    if let Ok(meta) = fs::metadata(path)
        && meta.is_file()
        && let Ok(modified) = meta.modified()
    {
        *newest = Some(newest.map_or(modified, |prev| prev.max(modified)));
    }
}

fn scan_dir_mtime(dir: &Path, newest: &mut Option<SystemTime>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some("target" | "tests" | "benches" | "examples" | ".git")
            ) {
                continue;
            }
            scan_dir_mtime(&path, newest);
        } else if file_type.is_file()
            && let Ok(meta) = entry.metadata()
            && let Ok(modified) = meta.modified()
        {
            *newest = Some(newest.map_or(modified, |prev| prev.max(modified)));
        }
    }
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

    let level = if default_stderr {
        if line.contains("panicked at")
            || line.starts_with("engine_runtime failed:")
            || line.starts_with("error:")
            || line.starts_with("Error:")
        {
            "error"
        } else {
            "warn"
        }
    } else {
        "info"
    };
    (level, "engine_runtime", line.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_binary_freshness_detects_newer_crate_sources_and_ignores_tests() {
        let tmp = tempfile::tempdir().expect("tempdir should create");
        let root = tmp.path();
        let src_dir = root.join("crates/engine_scripting/src");
        let tests_dir = root.join("crates/engine_scripting/tests");
        let bin_dir = root.join("target/debug");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&tests_dir).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();

        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
        fs::write(src_dir.join("lib.rs"), "// initial\n").unwrap();

        thread::sleep(Duration::from_millis(20));
        let bin_path = bin_dir.join("engine_runtime");
        fs::write(&bin_path, b"binary").unwrap();

        let newest_initial = latest_runtime_source_mtime(root);
        assert!(
            is_runtime_binary_fresh(&bin_path, newest_initial, None),
            "binary built after sources must be considered fresh"
        );

        // Editing an integration test file must not mark the runtime binary stale.
        thread::sleep(Duration::from_millis(20));
        fs::write(tests_dir.join("smoke.rs"), "// test only\n").unwrap();
        let newest_after_test_edit = latest_runtime_source_mtime(root);
        assert!(
            is_runtime_binary_fresh(&bin_path, newest_after_test_edit, None),
            "test-only edits should not invalidate engine_runtime binary"
        );

        // Editing a crate source file after the binary was built must mark the binary stale.
        thread::sleep(Duration::from_millis(20));
        fs::write(src_dir.join("lib.rs"), "// updated script component\n").unwrap();
        let newest_after_src_edit = latest_runtime_source_mtime(root);
        assert!(
            !is_runtime_binary_fresh(&bin_path, newest_after_src_edit, None),
            "source edits newer than binary must force rebuild"
        );

        // Once verified at `newest_after_src_edit`, it stays fresh until a newer edit arrives.
        assert!(is_runtime_binary_fresh(
            &bin_path,
            newest_after_src_edit,
            newest_after_src_edit
        ));
    }

    #[test]
    fn summarize_stderr_surfaces_runtime_failure_and_panics() {
        let lines = vec![
            "2026-11-28T11:20:30Z  INFO bevy_app: starting".to_owned(),
            "engine_runtime failed: failed to parse level RON: no registration found for `engine_scripting::component::ScriptComponent`".to_owned(),
        ];
        let summary = append_stderr_summary(
            "Engine Host exited before BRP became ready (exit status: 1)",
            &lines,
        );
        assert!(summary.contains("exit status: 1"));
        assert!(summary.contains("engine_runtime failed:"));
        assert!(summary.contains("ScriptComponent"));

        let empty = append_stderr_summary(
            "Engine Host exited before BRP became ready (exit status: 1)",
            &[],
        );
        assert_eq!(
            empty,
            "Engine Host exited before BRP became ready (exit status: 1)"
        );
    }

    #[test]
    fn parse_log_line_marks_runtime_errors_and_panics_as_error() {
        let (level, target, msg) = parse_log_line(
            "engine_runtime failed: failed to parse level RON",
            true,
        );
        assert_eq!(level, "error");
        assert_eq!(target, "engine_runtime");
        assert!(msg.contains("failed to parse level RON"));

        let (panic_level, _, _) =
            parse_log_line("thread 'main' panicked at crates/foo.rs:10:5:", true);
        assert_eq!(panic_level, "error");
    }
}
