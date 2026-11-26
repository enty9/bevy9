//! Stable persistent asset identifiers ([`AssetUuid`]) independent of disk paths.

use core::{fmt, str::FromStr};

use bevy_reflect::{std_traits::ReflectDefault, Reflect, ReflectDeserialize, ReflectSerialize};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable persistent UUID identifying a project asset across renames and moves.
///
/// Stored inside sidecar `*.b9meta` files as well as scene (`.level.ron`), prefab
/// (`.prefab.ron`), and tilemap (`.tilemap.ron`) references.
#[derive(
    Reflect,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[reflect(Default, Serialize, Deserialize, PartialEq, Hash)]
pub struct AssetUuid(Uuid);

impl AssetUuid {
    /// Generates a new random v4 [`AssetUuid`].
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wraps an existing [`Uuid`] as an [`AssetUuid`].
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Parses an [`AssetUuid`] from a canonical UUID string.
    pub fn parse_str(input: &str) -> Result<Self, uuid::Error> {
        Uuid::parse_str(input).map(Self)
    }

    /// Returns the underlying [`Uuid`] value.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for AssetUuid {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AssetUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for AssetUuid {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_str(s)
    }
}

impl From<Uuid> for AssetUuid {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl From<AssetUuid> for Uuid {
    fn from(value: AssetUuid) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_display_and_json_roundtrip() {
        let id = AssetUuid::new();
        let text = id.to_string();
        assert_eq!(text.len(), 36);
        let parsed: AssetUuid = text.parse().expect("UUID string should parse");
        assert_eq!(parsed, id);

        let json = serde_json::to_string(&id).expect("AssetUuid should serialize to JSON");
        let back: AssetUuid = serde_json::from_str(&json).expect("AssetUuid should deserialize");
        assert_eq!(back, id);
    }
}
