//! Authored [`ScriptComponent`] and inspector-editable [`ScriptProperties`] types.

use std::collections::BTreeMap;

use bevy_ecs::{component::Component, reflect::ReflectComponent};
use bevy_math::{Vec2, Vec3};
use bevy_reflect::{std_traits::ReflectDefault, Reflect, ReflectDeserialize, ReflectSerialize};
use engine_core::AssetUuid;
use serde::{Deserialize, Serialize};

/// Single inspector-editable property value attached to a [`ScriptComponent`].
#[derive(Reflect, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[reflect(Default, Serialize, Deserialize, PartialEq)]
pub enum ScriptPropertyValue {
    /// Boolean flag exposed to Rhai as `bool`.
    Bool(bool),
    /// 64-bit signed integer exposed to Rhai as `INT` (`i64`).
    Int(i64),
    /// 64-bit floating-point number exposed to Rhai as `FLOAT` (`f64`).
    Float(f64),
    /// UTF-8 string exposed to Rhai as `ImmutableString`.
    String(String),
    /// 2D vector exposed to Rhai as `Vec2`.
    Vec2(Vec2),
    /// 3D vector exposed to Rhai as `Vec3`.
    Vec3(Vec3),
}

impl Default for ScriptPropertyValue {
    fn default() -> Self {
        Self::Float(0.0)
    }
}

impl ScriptPropertyValue {
    /// Returns the value as `bool` if it is [`Self::Bool`].
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match *self {
            Self::Bool(v) => Some(v),
            _ => None,
        }
    }

    /// Returns the value as `i64` if it is [`Self::Int`].
    #[must_use]
    pub const fn as_int(&self) -> Option<i64> {
        match *self {
            Self::Int(v) => Some(v),
            _ => None,
        }
    }

    /// Returns the value as `f64`, promoting [`Self::Int`] if needed.
    #[must_use]
    pub const fn as_float(&self) -> Option<f64> {
        match *self {
            Self::Float(v) => Some(v),
            Self::Int(v) => Some(v as f64),
            _ => None,
        }
    }

    /// Returns the value as `&str` if it is [`Self::String`].
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(v) => Some(v.as_str()),
            _ => None,
        }
    }

    /// Returns the value as [`Vec2`] if it is [`Self::Vec2`].
    #[must_use]
    pub const fn as_vec2(&self) -> Option<Vec2> {
        match *self {
            Self::Vec2(v) => Some(v),
            _ => None,
        }
    }

    /// Returns the value as [`Vec3`] if it is [`Self::Vec3`].
    #[must_use]
    pub const fn as_vec3(&self) -> Option<Vec3> {
        match *self {
            Self::Vec3(v) => Some(v),
            _ => None,
        }
    }
}

impl From<bool> for ScriptPropertyValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for ScriptPropertyValue {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<i32> for ScriptPropertyValue {
    fn from(value: i32) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<f64> for ScriptPropertyValue {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<f32> for ScriptPropertyValue {
    fn from(value: f32) -> Self {
        Self::Float(f64::from(value))
    }
}

impl From<String> for ScriptPropertyValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for ScriptPropertyValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<Vec2> for ScriptPropertyValue {
    fn from(value: Vec2) -> Self {
        Self::Vec2(value)
    }
}

impl From<Vec3> for ScriptPropertyValue {
    fn from(value: Vec3) -> Self {
        Self::Vec3(value)
    }
}

/// Ordered serializable property map exposed to scripts as `props` and editable in the Inspector.
#[derive(Reflect, Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[reflect(Default, Serialize, Deserialize, PartialEq)]
pub struct ScriptProperties {
    /// Deterministic map of property name to typed value.
    #[serde(default)]
    pub values: BTreeMap<String, ScriptPropertyValue>,
}

impl ScriptProperties {
    /// Creates an empty property map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces a property and returns `self` for chaining.
    #[must_use]
    pub fn with(
        mut self,
        name: impl Into<String>,
        value: impl Into<ScriptPropertyValue>,
    ) -> Self {
        self.set(name, value);
        self
    }

    /// Inserts or replaces a property value.
    pub fn set(
        &mut self,
        name: impl Into<String>,
        value: impl Into<ScriptPropertyValue>,
    ) -> Option<ScriptPropertyValue> {
        self.values.insert(name.into(), value.into())
    }

    /// Looks up a property value by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ScriptPropertyValue> {
        self.values.get(name)
    }

    /// Removes a property by name, returning its previous value if present.
    pub fn remove(&mut self, name: &str) -> Option<ScriptPropertyValue> {
        self.values.remove(name)
    }

    /// Returns `true` if the property map contains `name`.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    /// Returns the number of stored properties.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns `true` when no properties are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Clears all stored properties.
    pub fn clear(&mut self) {
        self.values.clear();
    }

    /// Iterates over `(name, value)` pairs in deterministic key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ScriptPropertyValue)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Reads a boolean property.
    #[must_use]
    pub fn get_bool(&self, name: &str) -> Option<bool> {
        self.get(name).and_then(ScriptPropertyValue::as_bool)
    }

    /// Reads an integer property.
    #[must_use]
    pub fn get_int(&self, name: &str) -> Option<i64> {
        self.get(name).and_then(ScriptPropertyValue::as_int)
    }

    /// Reads a floating-point property (promoting integers if needed).
    #[must_use]
    pub fn get_float(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(ScriptPropertyValue::as_float)
    }

    /// Reads a string property.
    #[must_use]
    pub fn get_string(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(ScriptPropertyValue::as_str)
    }

    /// Reads a [`Vec2`] property.
    #[must_use]
    pub fn get_vec2(&self, name: &str) -> Option<Vec2> {
        self.get(name).and_then(ScriptPropertyValue::as_vec2)
    }

    /// Reads a [`Vec3`] property.
    #[must_use]
    pub fn get_vec3(&self, name: &str) -> Option<Vec3> {
        self.get(name).and_then(ScriptPropertyValue::as_vec3)
    }
}

/// Authored script component attached to scene entities.
///
/// References a `.rhai` asset via persistent [`AssetUuid`] (`script`) resolved
/// through `AssetRegistry`, alongside inspector-editable [`ScriptProperties`].
#[derive(Component, Reflect, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[reflect(Component, Default, Serialize, Deserialize, PartialEq)]
pub struct ScriptComponent {
    /// Persistent [`AssetUuid`] of a `.rhai` asset indexed in `AssetRegistry`.
    #[serde(default)]
    pub script: Option<AssetUuid>,
    /// Whether this entity's script lifecycle hooks execute when scripting is active.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Inspector-editable per-entity properties seeded into the script's `props` map.
    #[serde(default)]
    pub properties: ScriptProperties,
}

const fn default_enabled() -> bool {
    true
}

impl Default for ScriptComponent {
    fn default() -> Self {
        Self {
            script: None,
            enabled: true,
            properties: ScriptProperties::new(),
        }
    }
}

impl ScriptComponent {
    /// Creates an enabled [`ScriptComponent`] referencing a script asset by [`AssetUuid`].
    #[must_use]
    pub fn from_uuid(uuid: AssetUuid) -> Self {
        Self {
            script: Some(uuid),
            enabled: true,
            properties: ScriptProperties::new(),
        }
    }

    /// Sets the `enabled` flag and returns `self` for chaining.
    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Adds a single property to [`Self::properties`] and returns `self` for chaining.
    #[must_use]
    pub fn with_property(
        mut self,
        name: impl Into<String>,
        value: impl Into<ScriptPropertyValue>,
    ) -> Self {
        self.properties.set(name, value);
        self
    }

    /// Replaces [`Self::properties`] and returns `self` for chaining.
    #[must_use]
    pub fn with_properties(mut self, properties: ScriptProperties) -> Self {
        self.properties = properties;
        self
    }

    /// Returns `true` when [`Self::script`] is unassigned.
    #[must_use]
    pub const fn is_unassigned(&self) -> bool {
        self.script.is_none()
    }
}
