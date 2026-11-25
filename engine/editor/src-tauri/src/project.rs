use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};

const SUPPORTED_MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneFileEntry {
    pub name: String,
    pub relative_path: String,
    pub absolute_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub name: String,
    pub id: String,
    pub version: String,
    pub default_mode: String,
    pub startup_scene: String,
    pub root_dir: String,
    pub manifest_path: String,
    pub assets_dir: String,
    pub scenes_dir: String,
    pub scripts_dir: String,
    pub scenes: Vec<SceneFileEntry>,
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    schema_version: u32,
    project: RawProjectSection,
}

#[derive(Debug, Deserialize)]
struct RawProjectSection {
    name: String,
    #[serde(default)]
    id: String,
    #[serde(default = "default_version")]
    version: String,
    #[serde(default = "default_mode")]
    default_mode: String,
    #[serde(default = "default_startup_scene")]
    startup_scene: String,
    #[serde(default = "default_assets_dir")]
    assets_dir: String,
    #[serde(default = "default_scenes_dir")]
    scenes_dir: String,
    #[serde(default = "default_scripts_dir")]
    scripts_dir: String,
}

fn default_version() -> String {
    "0.1.0".to_owned()
}

fn default_mode() -> String {
    "hybrid".to_owned()
}

fn default_startup_scene() -> String {
    "scenes/main.level.ron".to_owned()
}

fn default_assets_dir() -> String {
    "assets".to_owned()
}

fn default_scenes_dir() -> String {
    "scenes".to_owned()
}

fn default_scripts_dir() -> String {
    "scripts".to_owned()
}

pub fn discover_engine_root() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("BEVY9_ENGINE_DIR") {
        let candidate = PathBuf::from(raw);
        if is_engine_root(&candidate) {
            return Some(fs::canonicalize(&candidate).unwrap_or(candidate));
        }
    }

    if let Some(manifest_dir) = option_env!("CARGO_MANIFEST_DIR")
        && let Some(found) = find_engine_in_ancestors(Path::new(manifest_dir))
    {
        return Some(found);
    }

    if let Ok(cwd) = std::env::current_dir()
        && let Some(found) = find_engine_in_ancestors(&cwd)
    {
        return Some(found);
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(found) = find_engine_in_ancestors(&exe)
    {
        return Some(found);
    }

    None
}

fn find_engine_in_ancestors(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        if is_engine_root(ancestor) {
            return Some(fs::canonicalize(ancestor).unwrap_or_else(|_| ancestor.to_path_buf()));
        }
        let nested_engine = ancestor.join("engine");
        if is_engine_root(&nested_engine) {
            return Some(fs::canonicalize(&nested_engine).unwrap_or(nested_engine));
        }
    }
    None
}

fn is_engine_root(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file() && dir.join("crates/engine_runtime/Cargo.toml").is_file()
}

pub fn list_sample_projects() -> Vec<ProjectSummary> {
    let Some(engine_root) = discover_engine_root() else {
        return Vec::new();
    };
    let samples_dir = engine_root.join("samples");
    let Ok(entries) = fs::read_dir(&samples_dir) else {
        return Vec::new();
    };

    let mut samples = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir()
            && path.join("project.toml").is_file()
            && let Ok(summary) = load_project_summary(&path)
        {
            samples.push(summary);
        }
    }

    samples.sort_by_key(|sample| {
        let folder = Path::new(&sample.root_dir)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let rank = match folder {
            "sandbox_3d" => 0,
            "sandbox_2d" => 1,
            "empty_project" => 2,
            _ => 3,
        };
        (rank, sample.name.to_lowercase())
    });

    samples
}

pub fn load_project_summary(path: &Path) -> anyhow::Result<ProjectSummary> {
    let manifest_path = if path.is_dir() {
        path.join("project.toml")
    } else {
        path.to_path_buf()
    };

    let canonical_manifest = fs::canonicalize(&manifest_path).with_context(|| {
        format!(
            "project manifest not found at '{}'",
            manifest_path.display()
        )
    })?;
    let content = fs::read_to_string(&canonical_manifest).with_context(|| {
        format!(
            "failed to read project manifest '{}'",
            canonical_manifest.display()
        )
    })?;
    let parsed: RawManifest = toml::from_str(&content).with_context(|| {
        format!(
            "failed to parse project manifest '{}'",
            canonical_manifest.display()
        )
    })?;

    if parsed.schema_version != SUPPORTED_MANIFEST_VERSION {
        anyhow::bail!(
            "unsupported schema_version {} (expected {SUPPORTED_MANIFEST_VERSION})",
            parsed.schema_version
        );
    }
    if parsed.project.name.trim().is_empty() {
        anyhow::bail!("project.name must not be empty");
    }

    let root_dir = canonical_manifest
        .parent()
        .context("project manifest has no parent directory")?
        .to_path_buf();
    let assets_dir = root_dir.join(&parsed.project.assets_dir);
    let scenes_dir = root_dir.join(&parsed.project.scenes_dir);
    let scripts_dir = root_dir.join(&parsed.project.scripts_dir);

    let mut scenes = Vec::new();
    collect_level_files(&root_dir, &scenes_dir, &mut scenes);
    scenes.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    Ok(ProjectSummary {
        name: parsed.project.name,
        id: parsed.project.id,
        version: parsed.project.version,
        default_mode: parsed.project.default_mode,
        startup_scene: parsed.project.startup_scene,
        root_dir: root_dir.display().to_string(),
        manifest_path: canonical_manifest.display().to_string(),
        assets_dir: assets_dir.display().to_string(),
        scenes_dir: scenes_dir.display().to_string(),
        scripts_dir: scripts_dir.display().to_string(),
        scenes,
    })
}

fn collect_level_files(root_dir: &Path, dir: &Path, out: &mut Vec<SceneFileEntry>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_level_files(root_dir, &path, out);
        } else if let Some(file_name) = path.file_name().and_then(|s| s.to_str())
            && file_name.ends_with(".level.ron")
        {
            let relative = path
                .strip_prefix(root_dir)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let display_name = file_name
                .strip_suffix(".level.ron")
                .unwrap_or(file_name)
                .to_owned();
            out.push(SceneFileEntry {
                name: display_name,
                relative_path: relative,
                absolute_path: path.display().to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_engine_root_and_sample_projects() {
        let root = discover_engine_root().expect("engine root must be discovered");
        assert!(is_engine_root(&root));

        let samples = list_sample_projects();
        assert!(
            samples.len() >= 3,
            "expected at least 3 sample projects, found {}",
            samples.len()
        );
        assert_eq!(samples[0].name, "Sandbox 3D");
        assert_eq!(samples[1].name, "Sandbox 2D");
    }

    #[test]
    fn loads_sandbox_3d_and_2d_summaries_with_scenes() {
        let root = discover_engine_root().expect("engine root must be discovered");
        let sandbox_3d =
            load_project_summary(&root.join("samples/sandbox_3d")).expect("sandbox_3d should load");
        assert_eq!(sandbox_3d.default_mode, "3d");
        assert!(sandbox_3d
            .scenes
            .iter()
            .any(|s| s.relative_path == "scenes/main.level.ron"));

        let sandbox_2d =
            load_project_summary(&root.join("samples/sandbox_2d")).expect("sandbox_2d should load");
        assert_eq!(sandbox_2d.default_mode, "2d");
        assert!(sandbox_2d
            .scenes
            .iter()
            .any(|s| s.relative_path == "scenes/main.level.ron"));
    }
}
