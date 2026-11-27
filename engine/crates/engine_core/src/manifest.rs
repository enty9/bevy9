//! Project manifest (`project.toml`) loading, validation, and path resolution.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use bevy_ecs::resource::Resource;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::world_mode::WorldMode;

/// Current supported `schema_version` of `project.toml`.
pub const CURRENT_MANIFEST_VERSION: u32 = 1;

/// Standard filename for an engine project manifest.
pub const MANIFEST_FILE_NAME: &str = "project.toml";

/// Errors raised while loading, validating, or saving a [`ProjectManifest`].
#[derive(Debug, Error)]
pub enum ProjectManifestError {
    /// Underlying filesystem error while reading or writing a manifest file.
    #[error("failed to access project manifest at '{}': {source}", path.display())]
    Io {
        /// Path that caused the I/O failure.
        path: PathBuf,
        /// Source OS error.
        #[source]
        source: std::io::Error,
    },
    /// TOML syntax or schema deserialization failure.
    #[error("failed to parse project manifest '{}': {source}", path.display())]
    TomlParse {
        /// Path of the invalid TOML file.
        path: PathBuf,
        /// Source TOML parser error.
        #[source]
        source: toml::de::Error,
    },
    /// TOML serialization failure.
    #[error("failed to serialize project manifest: {source}")]
    TomlSerialize {
        /// Source TOML serializer error.
        #[source]
        source: toml::ser::Error,
    },
    /// Semantic validation error (unsupported version, invalid paths, empty fields, etc.).
    #[error("invalid project manifest: {0}")]
    Validation(String),
}

/// Core project metadata stored under `[project]` in `project.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMetadata {
    /// Human-readable project name.
    pub name: String,
    /// Stable project UUID.
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    /// Semantic version string of the project.
    #[serde(default = "default_project_version")]
    pub version: String,
    /// Default dimensional mode used when opening the project.
    #[serde(default)]
    pub default_mode: WorldMode,
    /// Relative path to the initial scene file.
    #[serde(default = "default_startup_scene")]
    pub startup_scene: String,
    /// Relative path to the assets directory.
    #[serde(default = "default_assets_dir")]
    pub assets_dir: String,
    /// Relative path to the scenes directory.
    #[serde(default = "default_scenes_dir")]
    pub scenes_dir: String,
    /// Relative path to the scripts directory.
    #[serde(default = "default_scripts_dir")]
    pub scripts_dir: String,
    /// Optional path to a `.input.ron` mapping relative to the assets directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_mapping: Option<String>,
}

impl Default for ProjectMetadata {
    fn default() -> Self {
        Self {
            name: "Untitled Project".to_owned(),
            id: Uuid::new_v4(),
            version: default_project_version(),
            default_mode: WorldMode::default(),
            startup_scene: default_startup_scene(),
            assets_dir: default_assets_dir(),
            scenes_dir: default_scenes_dir(),
            scripts_dir: default_scripts_dir(),
            input_mapping: None,
        }
    }
}

/// Default window configuration stored under `[window]` in `project.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowConfig {
    /// Window title displayed by the OS window manager.
    #[serde(default = "default_window_title")]
    pub title: String,
    /// Initial window width in physical pixels.
    #[serde(default = "default_window_width")]
    pub width: u32,
    /// Initial window height in physical pixels.
    #[serde(default = "default_window_height")]
    pub height: u32,
    /// Whether the user can resize the window.
    #[serde(default = "default_true")]
    pub resizable: bool,
    /// Whether vertical sync is enabled by default.
    #[serde(default = "default_true")]
    pub vsync: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: default_window_title(),
            width: default_window_width(),
            height: default_window_height(),
            resizable: true,
            vsync: true,
        }
    }
}

/// Parsed representation of `project.toml`, stored as an ECS [`Resource`].
#[derive(Resource, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectManifest {
    /// Manifest format version (`CURRENT_MANIFEST_VERSION`).
    pub schema_version: u32,
    /// General project metadata and directory layout.
    pub project: ProjectMetadata,
    /// Default runtime window configuration.
    #[serde(default)]
    pub window: WindowConfig,
}

/// Resolved filesystem paths for an active engine project.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct ProjectPaths {
    /// Root directory containing `project.toml`.
    pub root_dir: PathBuf,
    /// Full path to the loaded manifest file.
    pub manifest_path: PathBuf,
    /// Resolved path to the project's assets directory.
    pub assets_dir: PathBuf,
    /// Resolved path to the project's scenes directory.
    pub scenes_dir: PathBuf,
    /// Resolved path to the project's scripts directory.
    pub scripts_dir: PathBuf,
}

impl ProjectManifest {
    /// Creates a new validated manifest with sensible defaults for `name` and `default_mode`.
    #[must_use]
    pub fn new(name: impl Into<String>, default_mode: WorldMode) -> Self {
        let name = name.into();
        let window_title = if name.trim().is_empty() {
            default_window_title()
        } else {
            name.clone()
        };
        Self {
            schema_version: CURRENT_MANIFEST_VERSION,
            project: ProjectMetadata {
                name,
                default_mode,
                ..ProjectMetadata::default()
            },
            window: WindowConfig {
                title: window_title,
                ..WindowConfig::default()
            },
        }
    }

    /// Validates schema version, metadata fields, relative project paths, and window dimensions.
    pub fn validate(&self) -> Result<(), ProjectManifestError> {
        if self.schema_version != CURRENT_MANIFEST_VERSION {
            return Err(ProjectManifestError::Validation(format!(
                "unsupported schema_version {} (expected {CURRENT_MANIFEST_VERSION})",
                self.schema_version
            )));
        }

        if self.project.name.trim().is_empty() {
            return Err(ProjectManifestError::Validation(
                "project.name must not be empty".to_owned(),
            ));
        }

        if self.project.version.trim().is_empty() {
            return Err(ProjectManifestError::Validation(
                "project.version must not be empty".to_owned(),
            ));
        }

        validate_relative_path("project.assets_dir", &self.project.assets_dir)?;
        validate_relative_path("project.scenes_dir", &self.project.scenes_dir)?;
        validate_relative_path("project.scripts_dir", &self.project.scripts_dir)?;
        validate_relative_path("project.startup_scene", &self.project.startup_scene)?;
        if let Some(mapping) = &self.project.input_mapping {
            validate_relative_path("project.input_mapping", mapping)?;
            if !mapping.ends_with(".input.ron") || mapping.contains([':', '#']) {
                return Err(ProjectManifestError::Validation(
                    "project.input_mapping must be a relative .input.ron path without a source or label".into(),
                ));
            }
        }

        if self.window.width == 0 || self.window.height == 0 {
            return Err(ProjectManifestError::Validation(
                "window.width and window.height must both be greater than 0".to_owned(),
            ));
        }

        Ok(())
    }

    /// Resolves relative project directories against `root_dir`.
    #[must_use]
    pub fn resolve_paths(&self, root_dir: impl AsRef<Path>) -> ProjectPaths {
        let root_dir = root_dir.as_ref().to_path_buf();
        ProjectPaths {
            manifest_path: root_dir.join(MANIFEST_FILE_NAME),
            assets_dir: root_dir.join(&self.project.assets_dir),
            scenes_dir: root_dir.join(&self.project.scenes_dir),
            scripts_dir: root_dir.join(&self.project.scripts_dir),
            root_dir,
        }
    }

    /// Loads and validates `project.toml` from inside `dir`.
    pub fn load_from_dir(
        dir: impl AsRef<Path>,
    ) -> Result<(Self, ProjectPaths), ProjectManifestError> {
        let manifest_path = dir.as_ref().join(MANIFEST_FILE_NAME);
        Self::load_from_file(manifest_path)
    }

    /// Loads and validates a project manifest from an explicit TOML file path.
    pub fn load_from_file(
        path: impl AsRef<Path>,
    ) -> Result<(Self, ProjectPaths), ProjectManifestError> {
        let manifest_path = path.as_ref().to_path_buf();
        let contents =
            fs::read_to_string(&manifest_path).map_err(|source| ProjectManifestError::Io {
                path: manifest_path.clone(),
                source,
            })?;
        let manifest: Self =
            toml::from_str(&contents).map_err(|source| ProjectManifestError::TomlParse {
                path: manifest_path.clone(),
                source,
            })?;
        manifest.validate()?;

        let root_dir = manifest_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut paths = manifest.resolve_paths(root_dir);
        paths.manifest_path = manifest_path;
        Ok((manifest, paths))
    }

    /// Loads a manifest whether `path` points to a project directory or directly to a `.toml` file.
    pub fn load_from_path(
        path: impl AsRef<Path>,
    ) -> Result<(Self, ProjectPaths), ProjectManifestError> {
        let path = path.as_ref();
        if path.is_dir() {
            Self::load_from_dir(path)
        } else {
            Self::load_from_file(path)
        }
    }

    /// Validates and writes the manifest as formatted TOML to `path`.
    pub fn save_to_file(&self, path: impl AsRef<Path>) -> Result<(), ProjectManifestError> {
        self.validate()?;
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| ProjectManifestError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let serialized = toml::to_string_pretty(self)
            .map_err(|source| ProjectManifestError::TomlSerialize { source })?;
        fs::write(path, serialized).map_err(|source| ProjectManifestError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

fn validate_relative_path(field_name: &str, raw: &str) -> Result<(), ProjectManifestError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ProjectManifestError::Validation(format!(
            "{field_name} must not be empty"
        )));
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return Err(ProjectManifestError::Validation(format!(
            "{field_name} must be a relative path inside the project: '{raw}'"
        )));
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(ProjectManifestError::Validation(format!(
            "{field_name} must be a relative path inside the project: '{raw}'"
        )));
    }

    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ProjectManifestError::Validation(format!(
                    "{field_name} contains disallowed path component: '{raw}'"
                )));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }

    Ok(())
}

fn default_project_version() -> String {
    "0.1.0".to_owned()
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

fn default_window_title() -> String {
    "Bevy9 Engine".to_owned()
}

const fn default_window_width() -> u32 {
    1280
}

const fn default_window_height() -> u32 {
    720
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{prefix}_{}", Uuid::new_v4()));
            fs::create_dir_all(&path).expect("temporary test directory should be created");
            Self { path }
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn manifest_toml_roundtrip_and_path_resolution() {
        let temp = TestTempDir::new("engine_core_manifest");
        let manifest_path = temp.path.join(MANIFEST_FILE_NAME);

        let manifest = ProjectManifest::new("Test Game", WorldMode::Hybrid);
        manifest
            .save_to_file(&manifest_path)
            .expect("valid manifest should save cleanly");

        let (loaded, paths) = ProjectManifest::load_from_dir(&temp.path)
            .expect("saved manifest should load from directory");
        assert_eq!(loaded, manifest);
        assert_eq!(paths.root_dir, temp.path);
        assert_eq!(paths.manifest_path, manifest_path);
        assert_eq!(paths.assets_dir, temp.path.join("assets"));
        assert_eq!(paths.scenes_dir, temp.path.join("scenes"));
        assert_eq!(paths.scripts_dir, temp.path.join("scripts"));

        let (loaded_via_path, _) = ProjectManifest::load_from_path(&manifest_path)
            .expect("manifest should also load via explicit file path");
        assert_eq!(loaded_via_path, manifest);
    }

    #[test]
    fn manifest_validation_rejects_invalid_inputs() {
        let mut wrong_version = ProjectManifest::new("Valid", WorldMode::Mode3D);
        wrong_version.schema_version = 999;
        assert!(matches!(
            wrong_version.validate(),
            Err(ProjectManifestError::Validation(_))
        ));

        let empty_name = ProjectManifest::new("   ", WorldMode::Mode2D);
        assert!(matches!(
            empty_name.validate(),
            Err(ProjectManifestError::Validation(_))
        ));

        let mut parent_escape = ProjectManifest::new("Valid", WorldMode::Mode3D);
        parent_escape.project.assets_dir = "../outside".to_owned();
        assert!(matches!(
            parent_escape.validate(),
            Err(ProjectManifestError::Validation(_))
        ));

        let mut absolute_scene = ProjectManifest::new("Valid", WorldMode::Mode3D);
        absolute_scene.project.startup_scene = "/etc/passwd".to_owned();
        assert!(matches!(
            absolute_scene.validate(),
            Err(ProjectManifestError::Validation(_))
        ));

        let mut zero_window = ProjectManifest::new("Valid", WorldMode::Mode3D);
        zero_window.window.width = 0;
        assert!(matches!(
            zero_window.validate(),
            Err(ProjectManifestError::Validation(_))
        ));
    }
}
