//! Dimensional workspace mode (`2D`, `3D`, or `Hybrid`) for engine worlds.

use core::{fmt, str::FromStr};

use bevy_ecs::{reflect::ReflectResource, resource::Resource};
use bevy_reflect::{std_traits::ReflectDefault, Reflect, ReflectDeserialize, ReflectSerialize};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Active spatial mode of an engine world or project.
#[derive(
    Resource, Reflect, Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize,
)]
#[reflect(Resource, Default, Serialize, Deserialize, PartialEq)]
pub enum WorldMode {
    /// 2D-focused world (2D cameras, sprites, tilemaps, 2D physics).
    #[serde(rename = "2d")]
    Mode2D,
    /// 3D-focused world (3D cameras, PBR meshes, lights, 3D physics).
    #[default]
    #[serde(rename = "3d")]
    Mode3D,
    /// Combined 2D and 3D workflows enabled simultaneously.
    #[serde(rename = "hybrid")]
    Hybrid,
}

/// Error returned when parsing a [`WorldMode`] from a string fails.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("invalid world mode '{0}'; expected '2d', '3d', or 'hybrid'")]
pub struct ParseWorldModeError(pub String);

impl WorldMode {
    /// Returns `true` when 2D subsystems are active in this mode.
    #[must_use]
    pub const fn supports_2d(&self) -> bool {
        matches!(self, Self::Mode2D | Self::Hybrid)
    }

    /// Returns `true` when 3D subsystems are active in this mode.
    #[must_use]
    pub const fn supports_3d(&self) -> bool {
        matches!(self, Self::Mode3D | Self::Hybrid)
    }

    /// Returns the canonical lowercase string identifier (`"2d"`, `"3d"`, or `"hybrid"`).
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Mode2D => "2d",
            Self::Mode3D => "3d",
            Self::Hybrid => "hybrid",
        }
    }
}

impl fmt::Display for WorldMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WorldMode {
    type Err = ParseWorldModeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "2d" | "mode2d" => Ok(Self::Mode2D),
            "3d" | "mode3d" => Ok(Self::Mode3D),
            "hybrid" => Ok(Self::Hybrid),
            _ => Err(ParseWorldModeError(value.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_flags_match_variants() {
        assert!(WorldMode::Mode2D.supports_2d());
        assert!(!WorldMode::Mode2D.supports_3d());

        assert!(!WorldMode::Mode3D.supports_2d());
        assert!(WorldMode::Mode3D.supports_3d());

        assert!(WorldMode::Hybrid.supports_2d());
        assert!(WorldMode::Hybrid.supports_3d());
    }

    #[test]
    fn parse_display_and_serde_roundtrip() {
        for (mode, expected) in [
            (WorldMode::Mode2D, "2d"),
            (WorldMode::Mode3D, "3d"),
            (WorldMode::Hybrid, "hybrid"),
        ] {
            assert_eq!(mode.as_str(), expected);
            assert_eq!(mode.to_string(), expected);
            assert_eq!(WorldMode::from_str(expected), Ok(mode));
            assert_eq!(
                WorldMode::from_str(&format!("  {} ", expected.to_ascii_uppercase())),
                Ok(mode)
            );

            let json = serde_json::to_string(&mode).expect("WorldMode should serialize");
            assert_eq!(json, format!("\"{expected}\""));
            let back: WorldMode =
                serde_json::from_str(&json).expect("WorldMode should deserialize");
            assert_eq!(back, mode);
        }

        assert!(WorldMode::from_str("4d").is_err());
    }
}
