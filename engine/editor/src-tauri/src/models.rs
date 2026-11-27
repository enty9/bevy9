//! Native Blender documents owned by the desktop editor. The runtime only sees GLBs.
use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::{self, File},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{anyhow, ensure, Context, Result};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{State, WebviewWindow};
use tempfile::{Builder, TempDir};
use uuid::Uuid;

use crate::{project, supervisor::EngineSupervisor};

const DRIVER: &str = include_str!("../blender/model.py");
const EMBED: &str = include_str!("../blender/embed.py");

fn message(err: anyhow::Error) -> String {
    format!("{err:#}")
}

fn blender() -> Command {
    let mut cmd =
        Command::new(std::env::var_os("BEVY9_BLENDER").unwrap_or_else(|| "blender".into()));
    cmd.args([
        "--factory-startup",
        "--disable-autoexec",
        "-noaudio",
        "--python-exit-code",
        "1",
    ]);
    cmd
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
        ensure!(dir == expected, "The active project has changed");
        let project = project::load_project_summary(Path::new(&dir))?;
        let root = PathBuf::from(project.root_dir);
        let assets = fs::canonicalize(project.assets_dir)?;
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
        let parent = self
            .assets
            .join(rel.parent().unwrap_or(Path::new("")))
            .canonicalize()?;
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

fn create(project: &Project, relative: &str) -> Result<()> {
    let path = project.model(relative)?;
    ensure!(!path.exists(), "A model with that name already exists");
    let work = Builder::new()
        .prefix(".model-")
        .tempdir_in(path.parent().unwrap())?;
    let script = work.path().join("model.py");
    let glb = work.path().join("model.glb");
    fs::write(&script, DRIVER)?;
    let output = blender()
        .args(["--background", "--python"])
        .arg(script)
        .args(["--", "create"])
        .arg(&glb)
        .output()
        .context("Cannot start Blender (install Blender or set BEVY9_BLENDER)")?;
    ensure!(
        output.status.success(),
        "Blender failed to create model: {} {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    fs::hard_link(glb, path)
        .context("Could not publish new model without overwriting another asset")?;
    Ok(())
}

#[derive(Deserialize)]
struct Sidecar {
    uuid: (String,),
}

struct Session {
    child: Child,
    project: PathBuf,
    asset: Uuid,
    relative: String,
    work: TempDir,
    started: Instant,
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
    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
    pub fn has_tabs(&self) -> bool {
        !self.0.lock().unwrap().is_empty()
    }

    fn open(&self, project: Project, relative: &str, id: Uuid, parent: String) -> Result<String> {
        let path = project.model(relative)?;
        ensure!(path.is_file(), "Model no longer exists");
        let marker: Sidecar =
            ron::from_str(&fs::read_to_string(path.with_added_extension("b9meta"))?)?;
        ensure!(
            Uuid::parse_str(&marker.uuid.0)? == id,
            "Model UUID does not match its sidecar"
        );
        let mut sessions = self.0.lock().unwrap();
        if let Some((token, _)) = sessions
            .iter()
            .find(|(_, s)| s.project == project.root && s.asset == id)
        {
            return Ok(token.clone());
        }
        let source = project.source(id)?;
        let work = TempDir::new()?;
        fs::write(
            work.path().join("session.json"),
            serde_json::to_vec(&json!({
                "parent": parent, "source": source, "asset": path, "assets_root": project.assets, "uuid": id.to_string()
            }))?,
        )?;
        fs::write(work.path().join("model.py"), DRIVER)?;
        fs::write(work.path().join("embed.py"), EMBED)?;
        let mut cmd = blender();
        #[cfg(target_os = "linux")]
        {
            ensure!(
                Command::new("xdotool").arg("version").output().is_ok(),
                "Install xdotool to embed Blender"
            );
            cmd.env_remove("WAYLAND_DISPLAY")
                .env_remove("WAYLAND_SOCKET")
                .env("XDG_SESSION_TYPE", "x11")
                .args(["--no-window-frame", "--no-window-focus"]);
        }
        cmd.args(["-w", "-p", "0", "0", "800", "600"]);
        if source.is_file() {
            cmd.arg(source);
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
        let mut sessions = self.0.lock().unwrap();
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
    let parent =
        match handle.as_raw() {
            RawWindowHandle::Xlib(h) => h.window.to_string(),
            RawWindowHandle::Win32(h) => h.hwnd.get().to_string(),
            _ => return Err(
                "Blender tabs require Windows or Linux X11/XWayland (launch with GDK_BACKEND=x11)"
                    .into(),
            ),
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
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    temp.write_all(data)?;
    temp.persist(path)?;
    Ok(())
}

#[tauri::command]
pub fn model_layout(
    models: State<Models>,
    token: String,
    rect: Option<Rect>,
) -> Result<(), String> {
    models.with(&token, |s| {
        replace_file(
            &s.work.path().join("layout.json"),
            &serde_json::to_vec(&rect)?,
        )
    })
}

#[tauri::command]
pub fn model_status(models: State<Models>, token: String) -> Result<Value, String> {
    models.with(&token, |s| {
        let dir = s.work.path();
        if s.child.try_wait()?.is_some() {
            let log = fs::read_to_string(dir.join("blender.log")).unwrap_or_default();
            return Err(anyhow!(
                "Blender exited. {}",
                log.lines()
                    .rev()
                    .take(8)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        match fs::read(dir.join("status.json")) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ensure!(
                    s.started.elapsed() < Duration::from_secs(60),
                    "Blender did not initialize within 60 seconds"
                );
                Ok(json!({"ready":false,"saved":0,"dirty":false}))
            }
            Err(e) => Err(e.into()),
        }
    })
}

#[tauri::command]
pub fn save_model(models: State<Models>, token: String) -> Result<String, String> {
    models.with(&token, |s| {
        let request = Uuid::new_v4().to_string();
        replace_file(&s.work.path().join("save"), request.as_bytes())?;
        Ok(request)
    })
}

#[tauri::command]
pub fn list_model_tabs(models: State<Models>) -> Vec<Value> {
    models.0.lock().unwrap().iter().map(|(token,s)| json!({
        "token": token, "uuid": s.asset.to_string(), "path": s.relative, "project": s.project
    })).collect()
}

#[tauri::command]
pub fn close_model(models: State<Models>, token: String) {
    models.0.lock().unwrap().remove(&token);
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
}
