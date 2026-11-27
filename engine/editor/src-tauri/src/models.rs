//! Native Blender documents owned by the desktop editor. The runtime only sees GLBs.
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind},
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, ensure};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{State, WebviewWindow};
use tempfile::{Builder, TempDir};
use uuid::Uuid;

use crate::{project, supervisor::EngineSupervisor};

const DRIVER: &str = include_str!("../blender/model.py");
const EMBED: &str = include_str!("../blender/embed.py");
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

static NEXT_TAB_SEQ: AtomicU64 = AtomicU64::new(1);

fn message(err: anyhow::Error) -> String {
    format!("{err:#}")
}

fn canonical_or_self(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn find_in_path(binary: &OsStr, path_env: Option<&OsStr>) -> Option<PathBuf> {
    let path_env = path_env?;
    #[cfg(windows)]
    let candidates = {
        let mut list = vec![OsString::from(binary)];
        let lower = binary.to_string_lossy().to_ascii_lowercase();
        if !lower.ends_with(".exe") {
            let mut with_exe = OsString::from(binary);
            with_exe.push(".exe");
            list.push(with_exe);
        }
        list
    };
    #[cfg(not(windows))]
    let candidates = [OsString::from(binary)];

    for dir in std::env::split_paths(path_env) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for name in &candidates {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn standard_blender_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "linux")]
    {
        candidates.push(PathBuf::from("/usr/bin/blender"));
        candidates.push(PathBuf::from("/usr/local/bin/blender"));
        candidates.push(PathBuf::from("/snap/bin/blender"));
    }
    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from(
            "/Applications/Blender.app/Contents/MacOS/Blender",
        ));
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(
                PathBuf::from(home).join("Applications/Blender.app/Contents/MacOS/Blender"),
            );
        }
    }
    #[cfg(windows)]
    {
        for env_key in ["ProgramFiles", "ProgramFiles(x86)"] {
            let Some(root) = std::env::var_os(env_key) else {
                continue;
            };
            let foundation = PathBuf::from(root).join("Blender Foundation");
            let Ok(entries) = fs::read_dir(&foundation) else {
                continue;
            };
            let mut version_dirs: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect();
            version_dirs.sort();
            version_dirs.reverse();
            for dir in version_dirs {
                candidates.push(dir.join("blender.exe"));
            }
        }
    }
    candidates
}

fn resolve_blender_with(
    configured: Option<&OsStr>,
    path_env: Option<&OsStr>,
    fallback_candidates: &[PathBuf],
) -> Result<PathBuf> {
    if let Some(raw) = configured.filter(|value| !value.is_empty()) {
        let configured_path = PathBuf::from(raw);
        let looks_like_path =
            configured_path.is_absolute() || configured_path.components().count() > 1;
        if looks_like_path {
            ensure!(
                configured_path.is_file(),
                "BEVY9_BLENDER points to '{}', which does not exist or is not a file",
                configured_path.display()
            );
            return Ok(configured_path);
        }
        return find_in_path(raw, path_env).ok_or_else(|| {
            anyhow!(
                "BEVY9_BLENDER is set to '{}', which was not found on PATH",
                raw.to_string_lossy()
            )
        });
    }

    if let Some(found) = find_in_path(OsStr::new("blender"), path_env) {
        return Ok(found);
    }

    for candidate in fallback_candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }

    Err(anyhow!(
        "Blender was not found. Install Blender (3.6+ / 4.x) and ensure 'blender' is on PATH, or set BEVY9_BLENDER to the Blender executable path."
    ))
}

fn resolve_blender_binary() -> Result<PathBuf> {
    let configured = std::env::var_os("BEVY9_BLENDER");
    let path_env = std::env::var_os("PATH");
    let fallbacks = standard_blender_candidates();
    resolve_blender_with(configured.as_deref(), path_env.as_deref(), &fallbacks)
}

fn blender_command() -> Result<Command> {
    let binary = resolve_blender_binary()?;
    let mut cmd = Command::new(binary);
    cmd.args([
        "--factory-startup",
        "--disable-autoexec",
        "-noaudio",
        "--python-exit-code",
        "1",
    ]);
    Ok(cmd)
}

fn tail_non_empty_lines(text: &str, max_lines: usize) -> String {
    let mut lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(max_lines)
        .collect();
    lines.reverse();
    lines.join("\n")
}

fn summarize_process_output(status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> String {
    let stderr_tail = tail_non_empty_lines(&String::from_utf8_lossy(stderr), 8);
    let stdout_tail = tail_non_empty_lines(&String::from_utf8_lossy(stdout), 8);
    let detail = if !stderr_tail.is_empty() {
        stderr_tail
    } else {
        stdout_tail
    };
    if detail.is_empty() {
        format!("({status})")
    } else {
        format!("({status}): {detail}")
    }
}

struct Project {
    root: PathBuf,
    assets: PathBuf,
}

impl Project {
    fn current(supervisor: &EngineSupervisor, expected: &str) -> Result<Self> {
        let dir = supervisor
            .status()
            .project_dir
            .context("No active project")?;
        ensure!(
            canonical_or_self(Path::new(&dir)) == canonical_or_self(Path::new(expected)),
            "The active project has changed"
        );
        let project = project::load_project_summary(Path::new(&dir))?;
        let root = PathBuf::from(project.root_dir);
        let assets = fs::canonicalize(&project.assets_dir).with_context(|| {
            format!(
                "Project assets directory '{}' does not exist",
                project.assets_dir
            )
        })?;
        ensure!(
            assets.starts_with(&root),
            "Assets directory is outside the project"
        );
        Ok(Self { root, assets })
    }

    fn model(&self, relative: &str) -> Result<PathBuf> {
        let rel = Path::new(relative);
        ensure!(
            !relative.is_empty()
                && rel.components().all(|c| matches!(c, Component::Normal(_)))
                && rel
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|e| e.eq_ignore_ascii_case("glb")),
            "Expected a relative .glb asset path"
        );
        let parent_rel = rel.parent().unwrap_or(Path::new(""));
        let parent = self
            .assets
            .join(parent_rel)
            .canonicalize()
            .with_context(|| format!("Model folder does not exist for '{relative}'"))?;
        ensure!(
            parent.starts_with(&self.assets),
            "Model folder is outside assets"
        );
        let path = parent.join(rel.file_name().context("Missing model name")?);
        ensure!(!path.is_symlink(), "Model symlinks are not supported");
        Ok(path)
    }

    fn source(&self, id: Uuid) -> Result<PathBuf> {
        let dir = self.root.join("model_sources");
        ensure!(!dir.is_symlink(), "Model source folder cannot be a symlink");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{id}.blend"));
        ensure!(!path.is_symlink(), "Model source cannot be a symlink");
        Ok(path)
    }
}

fn publish_new_file(src: &Path, dst: &Path) -> Result<()> {
    match fs::hard_link(src, dst) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            Err(anyhow!("A model with that name already exists"))
        }
        Err(err) if err.kind() == ErrorKind::Unsupported => {
            let mut input = File::open(src)?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dst)
                .map_err(|open_err| {
                    if open_err.kind() == ErrorKind::AlreadyExists {
                        anyhow!("A model with that name already exists")
                    } else {
                        anyhow::Error::from(open_err)
                    }
                })?;
            io::copy(&mut input, &mut output)?;
            output.sync_all()?;
            Ok(())
        }
        Err(err) => Err(err).context("Could not publish new model without overwriting another asset"),
    }
}

fn create(project: &Project, relative: &str) -> Result<()> {
    let path = project.model(relative)?;
    ensure!(!path.exists(), "A model with that name already exists");
    let work = Builder::new()
        .prefix(".model-")
        .tempdir_in(path.parent().context("Model path has no parent folder")?)?;
    let script = work.path().join("model.py");
    let glb = work.path().join("model.glb");
    fs::write(&script, DRIVER)?;
    let output = blender_command()?
        .args(["--background", "--python"])
        .arg(&script)
        .args(["--", "create"])
        .arg(&glb)
        .output()
        .context("Cannot start Blender (install Blender or set BEVY9_BLENDER)")?;
    ensure!(
        output.status.success(),
        "Blender failed to create model {}",
        summarize_process_output(output.status, &output.stdout, &output.stderr)
    );
    ensure!(
        glb.is_file(),
        "Blender finished without exporting a .glb model"
    );
    publish_new_file(&glb, &path)?;
    Ok(())
}

#[derive(Deserialize)]
struct Sidecar {
    uuid: (String,),
}

struct Session {
    child: Child,
    exited: Option<ExitStatus>,
    opened_seq: u64,
    project: PathBuf,
    asset: Uuid,
    relative: String,
    work: TempDir,
    started: Instant,
}

impl Session {
    fn poll_exited(&mut self) -> Result<Option<ExitStatus>> {
        if let Some(status) = self.exited {
            return Ok(Some(status));
        }
        if let Some(status) = self.child.try_wait()? {
            self.exited = Some(status);
            return Ok(Some(status));
        }
        Ok(None)
    }

    fn is_alive(&mut self) -> bool {
        self.poll_exited().ok().flatten().is_none()
    }

    fn hide(&self) {
        let _ = replace_file(&self.work.path().join("layout.json"), b"null");
    }

    fn exit_error(&self, status: ExitStatus) -> anyhow::Error {
        let log = fs::read_to_string(self.work.path().join("blender.log")).unwrap_or_default();
        let snippet = tail_non_empty_lines(&log, 8);
        if snippet.is_empty() {
            anyhow!(
                "Blender exited unexpectedly ({status}). Verify your Blender installation or set BEVY9_BLENDER."
            )
        } else {
            anyhow!("Blender exited ({status}):\n{snippet}")
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
pub struct Models(Mutex<HashMap<String, Session>>);

impl Models {
    fn lock_sessions(&self) -> MutexGuard<'_, HashMap<String, Session>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn clear(&self) {
        self.lock_sessions().clear();
    }

    pub fn has_live_tabs(&self) -> bool {
        let mut sessions = self.lock_sessions();
        sessions.values_mut().any(Session::is_alive)
    }

    pub fn has_live_tabs_for_other_project(&self, target_project: &Path) -> bool {
        let target = canonical_or_self(target_project);
        let mut sessions = self.lock_sessions();
        sessions
            .values_mut()
            .any(|session| session.is_alive() && canonical_or_self(&session.project) != target)
    }

    pub fn close_tabs_for_other_projects(&self, target_project: &Path) {
        let target = canonical_or_self(target_project);
        let mut sessions = self.lock_sessions();
        sessions.retain(|_, session| canonical_or_self(&session.project) == target);
    }

    pub fn hide_all(&self) {
        let mut sessions = self.lock_sessions();
        for session in sessions.values_mut() {
            if session.is_alive() {
                session.hide();
            }
        }
    }

    fn open(&self, project: Project, relative: &str, id: Uuid, parent: String) -> Result<String> {
        let path = project.model(relative)?;
        ensure!(path.is_file(), "Model no longer exists");
        let sidecar_path = path.with_added_extension("b9meta");
        let sidecar_text = fs::read_to_string(&sidecar_path).with_context(|| {
            format!("Missing asset metadata for '{relative}' (try rescanning project assets)")
        })?;
        let marker: Sidecar = ron::from_str(&sidecar_text)
            .with_context(|| format!("Invalid asset metadata sidecar for '{relative}'"))?;
        ensure!(
            Uuid::parse_str(&marker.uuid.0)? == id,
            "Model UUID does not match its sidecar"
        );

        #[cfg(target_os = "linux")]
        {
            let xdotool_ok = Command::new("xdotool")
                .arg("version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            ensure!(
                xdotool_ok,
                "Install xdotool to embed Blender inside the editor on Linux (X11/XWayland)"
            );
        }

        let mut cmd = blender_command()?;

        let mut sessions = self.lock_sessions();
        // Prune any dead sessions from other projects, and reuse an existing live tab for this model.
        sessions.retain(|_, session| {
            canonical_or_self(&session.project) == canonical_or_self(&project.root)
                || session.is_alive()
        });

        let mut dead_duplicate = None;
        for (token, session) in sessions.iter_mut() {
            if session.project == project.root && session.asset == id {
                if session.is_alive() {
                    return Ok(token.clone());
                }
                dead_duplicate = Some(token.clone());
                break;
            }
        }
        if let Some(dead_token) = dead_duplicate {
            sessions.remove(&dead_token);
        }

        let source = project.source(id)?;
        let work = TempDir::new()?;
        fs::write(
            work.path().join("session.json"),
            serde_json::to_vec(&json!({
                "parent": parent,
                "source": source,
                "asset": path,
                "assets_root": project.assets,
                "uuid": id.to_string(),
            }))?,
        )?;
        fs::write(work.path().join("model.py"), DRIVER)?;
        fs::write(work.path().join("embed.py"), EMBED)?;

        #[cfg(target_os = "linux")]
        {
            cmd.env_remove("WAYLAND_DISPLAY")
                .env_remove("WAYLAND_SOCKET")
                .env("XDG_SESSION_TYPE", "x11")
                .args(["--no-window-frame", "--no-window-focus"]);
        }
        cmd.args(["-w", "-p", "0", "0", "800", "600"]);
        if source.is_file() {
            cmd.arg(&source);
        }
        let log = File::create(work.path().join("blender.log"))?;
        let child = cmd
            .arg("--python")
            .arg(work.path().join("model.py"))
            .args(["--", "edit"])
            .arg(work.path())
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("Cannot start Blender (check BEVY9_BLENDER or PATH)")?;
        let token = Uuid::new_v4().to_string();
        sessions.insert(
            token.clone(),
            Session {
                child,
                exited: None,
                opened_seq: NEXT_TAB_SEQ.fetch_add(1, Ordering::Relaxed),
                project: project.root,
                asset: id,
                relative: relative.to_owned(),
                work,
                started: Instant::now(),
            },
        );
        Ok(token)
    }

    fn with<T>(&self, token: &str, f: impl FnOnce(&mut Session) -> Result<T>) -> Result<T, String> {
        let mut sessions = self.lock_sessions();
        f(sessions.get_mut(token).ok_or("Model tab is closed")?).map_err(message)
    }
}

#[tauri::command]
pub async fn create_model(
    supervisor: State<'_, EngineSupervisor>,
    project: String,
    path: String,
) -> Result<(), String> {
    let project = Project::current(&supervisor, &project).map_err(message)?;
    tauri::async_runtime::spawn_blocking(move || create(&project, &path).map_err(message))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn open_model(
    window: WebviewWindow,
    supervisor: State<EngineSupervisor>,
    models: State<Models>,
    project: String,
    path: String,
    uuid: String,
) -> Result<String, String> {
    let handle = window.window_handle().map_err(|e| e.to_string())?;
    let parent = match handle.as_raw() {
        RawWindowHandle::Xlib(h) => h.window.to_string(),
        RawWindowHandle::Xcb(h) => h.window.get().to_string(),
        RawWindowHandle::Win32(h) => h.hwnd.get().to_string(),
        _ => {
            return Err(
                "Blender tabs require Windows or Linux X11/XWayland (launch with GDK_BACKEND=x11)"
                    .into(),
            );
        }
    };
    models
        .open(
            Project::current(&supervisor, &project).map_err(message)?,
            &path,
            Uuid::parse_str(&uuid).map_err(|e| e.to_string())?,
            parent,
        )
        .map_err(message)
}

#[derive(Deserialize, Serialize)]
pub struct Rect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn replace_file(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("Target file has no parent folder")?;
    for attempt in 0..5 {
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(data)?;
        match temp.persist(path) {
            Ok(_) => return Ok(()),
            Err(err) if err.error.kind() == ErrorKind::PermissionDenied && attempt < 4 => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(err) => return Err(err.error.into()),
        }
    }
    unreachable!("loop always returns by the final attempt")
}

#[tauri::command]
pub fn model_layout(
    models: State<Models>,
    token: String,
    rect: Option<Rect>,
) -> Result<(), String> {
    models.with(&token, |s| {
        if !s.is_alive() {
            return Ok(());
        }
        replace_file(
            &s.work.path().join("layout.json"),
            &serde_json::to_vec(&rect)?,
        )
    })
}

#[tauri::command]
pub fn hide_model_tabs(models: State<Models>) {
    models.hide_all();
}

#[tauri::command]
pub fn model_status(models: State<Models>, token: String) -> Result<Value, String> {
    models.with(&token, |s| {
        if let Some(status) = s.poll_exited()? {
            return Err(s.exit_error(status));
        }
        let dir = s.work.path();
        match fs::read(dir.join("status.json")) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                if s.started.elapsed() >= STARTUP_TIMEOUT {
                    let log = fs::read_to_string(dir.join("blender.log")).unwrap_or_default();
                    let snippet = tail_non_empty_lines(&log, 8);
                    if snippet.is_empty() {
                        return Err(anyhow!(
                            "Blender did not initialize within {} seconds",
                            STARTUP_TIMEOUT.as_secs()
                        ));
                    }
                    return Err(anyhow!(
                        "Blender did not initialize within {} seconds:\n{snippet}",
                        STARTUP_TIMEOUT.as_secs()
                    ));
                }
                Ok(json!({"ready": false, "saved": 0, "dirty": false}))
            }
            Err(e) => Err(e.into()),
        }
    })
}

#[tauri::command]
pub fn save_model(models: State<Models>, token: String) -> Result<String, String> {
    models.with(&token, |s| {
        if let Some(status) = s.poll_exited()? {
            return Err(s.exit_error(status));
        }
        let request = Uuid::new_v4().to_string();
        replace_file(&s.work.path().join("save"), request.as_bytes())?;
        Ok(request)
    })
}

#[tauri::command]
pub fn list_model_tabs(models: State<Models>) -> Vec<Value> {
    let sessions = models.lock_sessions();
    let mut ordered: Vec<(&String, &Session)> = sessions.iter().collect();
    ordered.sort_by_key(|(_, session)| session.opened_seq);
    ordered
        .into_iter()
        .map(|(token, s)| {
            json!({
                "token": token,
                "uuid": s.asset.to_string(),
                "path": s.relative,
                "project": s.project,
            })
        })
        .collect()
}

#[tauri::command]
pub fn close_model(models: State<Models>, token: String) {
    models.lock_sessions().remove(&token);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creation_is_scoped_and_never_replaces_an_existing_model() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let project = Project {
            assets: root.join("assets"),
            root,
        };
        fs::create_dir(&project.assets).unwrap();
        fs::write(project.assets.join("used.glb"), b"art").unwrap();
        assert!(create(&project, "used.glb").is_err());
        assert_eq!(fs::read(project.assets.join("used.glb")).unwrap(), b"art");
        for invalid in ["../other.glb", "/tmp/other.glb", "file.blend", ""] {
            assert!(project.model(invalid).is_err());
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&project.root, project.assets.join("escape")).unwrap();
            assert!(project.model("escape/other.glb").is_err());
        }
        let _: Sidecar = ron::from_str(include_str!(
            "../../../samples/sandbox_3d/assets/crystal.glb.b9meta"
        ))
        .unwrap();
    }

    #[test]
    fn blender_binary_resolution_handles_env_path_and_fallbacks() {
        let temp = TempDir::new().unwrap();
        let fake_bin = temp.path().join("custom-blender");
        fs::write(&fake_bin, b"#!/bin/sh\n").unwrap();

        // Explicit path via BEVY9_BLENDER succeeds.
        let resolved =
            resolve_blender_with(Some(fake_bin.as_os_str()), None, &[]).expect("explicit path");
        assert_eq!(resolved, fake_bin);

        // Missing explicit path produces a clear error naming BEVY9_BLENDER.
        let missing = temp.path().join("does-not-exist");
        let err = resolve_blender_with(Some(missing.as_os_str()), None, &[]).unwrap_err();
        assert!(format!("{err:#}").contains("BEVY9_BLENDER"));

        // Empty BEVY9_BLENDER falls back to PATH discovery.
        let path_dir = temp.path().join("bin");
        fs::create_dir(&path_dir).unwrap();
        let path_blender = path_dir.join(if cfg!(windows) {
            "blender.exe"
        } else {
            "blender"
        });
        fs::write(&path_blender, b"#!/bin/sh\n").unwrap();
        let resolved_from_path =
            resolve_blender_with(Some(OsStr::new("")), Some(path_dir.as_os_str()), &[])
                .expect("PATH fallback");
        assert_eq!(resolved_from_path, path_blender);

        // Fallback candidate list works when PATH is empty.
        let resolved_from_fallback =
            resolve_blender_with(None, None, std::slice::from_ref(&fake_bin))
                .expect("standard path fallback");
        assert_eq!(resolved_from_fallback, fake_bin);

        // Nothing found yields actionable instructions.
        let not_found_err = resolve_blender_with(None, Some(OsStr::new("")), &[]).unwrap_err();
        let msg = format!("{not_found_err:#}");
        assert!(msg.contains("BEVY9_BLENDER"));
        assert!(msg.contains("PATH"));
    }

    #[test]
    fn replace_file_and_log_tail_helpers_work_cleanly() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("layout.json");
        replace_file(&target, b"{\"x\":1}").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "{\"x\":1}");
        replace_file(&target, b"null").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "null");

        let lines = (1..=12)
            .map(|n| format!("line {n}\n\n"))
            .collect::<String>();
        let tail = tail_non_empty_lines(&lines, 3);
        assert_eq!(tail, "line 10\nline 11\nline 12");
    }

    #[cfg(unix)]
    #[test]
    fn models_session_lifecycle_tracks_live_tabs_hiding_and_project_switching() {
        let temp = TempDir::new().unwrap();
        let project_a = temp.path().join("proj_a");
        let project_b = temp.path().join("proj_b");
        fs::create_dir(&project_a).unwrap();
        fs::create_dir(&project_b).unwrap();

        let models = Models::default();
        assert!(!models.has_live_tabs());

        // Insert a live session for project A and a dead session for project B.
        let work_a = TempDir::new().unwrap();
        let child_a = Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let work_b = TempDir::new().unwrap();
        let mut child_b = Command::new("sh")
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let _ = child_b.wait();

        let layout_a = work_a.path().join("layout.json");
        replace_file(&layout_a, b"{\"x\":10,\"y\":20,\"width\":400,\"height\":300}").unwrap();

        {
            let mut guard = models.lock_sessions();
            guard.insert(
                "tok-a".into(),
                Session {
                    child: child_a,
                    exited: None,
                    opened_seq: 1,
                    project: project_a.clone(),
                    asset: Uuid::new_v4(),
                    relative: "a.glb".into(),
                    work: work_a,
                    started: Instant::now(),
                },
            );
            guard.insert(
                "tok-b".into(),
                Session {
                    child: child_b,
                    exited: None,
                    opened_seq: 2,
                    project: project_b.clone(),
                    asset: Uuid::new_v4(),
                    relative: "b.glb".into(),
                    work: work_b,
                    started: Instant::now(),
                },
            );
        }

        assert!(models.has_live_tabs());
        // Project A has a live tab, so switching to Project B is blocked.
        assert!(models.has_live_tabs_for_other_project(&project_b));
        // Project B's tab is already dead, so staying on Project A is NOT blocked.
        assert!(!models.has_live_tabs_for_other_project(&project_a));

        // hide_all writes null to live session's layout.json.
        models.hide_all();
        assert_eq!(fs::read_to_string(&layout_a).unwrap(), "null");

        // Closing tabs for other projects drops dead Project B tab while keeping Project A.
        models.close_tabs_for_other_projects(&project_a);
        assert_eq!(models.lock_sessions().len(), 1);
        assert!(models.lock_sessions().contains_key("tok-a"));

        // Clear terminates and removes all remaining sessions.
        models.clear();
        assert!(!models.has_live_tabs());
    }
}
