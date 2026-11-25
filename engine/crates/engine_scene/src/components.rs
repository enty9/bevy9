//! Serializable 2D/3D scene descriptors and active scene state resources.

use std::path::PathBuf;

use bevy_color::{Color, LinearRgba};
use bevy_ecs::{
    component::Component,
    reflect::{ReflectComponent, ReflectResource},
    resource::Resource,
};
use bevy_math::{Vec2, Vec3};
use bevy_reflect::{std_traits::ReflectDefault, Reflect};
use engine_core::WorldMode;
use serde::{Deserialize, Serialize};

/// Marker component tagging entities that belong to the authored level scene.
///
/// Editor-only entities (editor cameras, grid helpers, viewport readback nodes)
/// omit this marker so they are excluded from `.level.ron` serialization and
/// the editor Outliner.
#[derive(
    Component, Reflect, Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component, Default)]
pub struct SceneEntity;

/// Serializable 3D primitive mesh descriptor hydrated into `Mesh3d`.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[reflect(Component, Default)]
pub enum MeshPrimitive3d {
    /// Axis-aligned box with full extents `[x, y, z]`.
    Cube {
        /// Full dimensions along X, Y, and Z.
        size: Vec3,
    },
    /// UV sphere with the specified radius.
    Sphere {
        /// Sphere radius in world units.
        radius: f32,
    },
    /// Horizontal ground plane in XZ with the specified `[width, depth]`.
    Plane {
        /// Plane extents along X and Z.
        size: Vec2,
    },
    /// Vertical cylinder aligned along Y.
    Cylinder {
        /// Cylinder radius.
        radius: f32,
        /// Full cylinder height.
        height: f32,
    },
}

impl Default for MeshPrimitive3d {
    fn default() -> Self {
        Self::Cube { size: Vec3::ONE }
    }
}

/// Serializable PBR material descriptor hydrated into `MeshMaterial3d<StandardMaterial>`.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[reflect(Component, Default)]
pub struct StandardMaterialDesc {
    /// Base albedo color.
    pub base_color: Color,
    /// Metallic factor in `[0.0, 1.0]`.
    pub metallic: f32,
    /// Perceptual roughness in `[0.0, 1.0]`.
    pub perceptual_roughness: f32,
    /// Linear emissive color.
    pub emissive: LinearRgba,
}

impl Default for StandardMaterialDesc {
    fn default() -> Self {
        Self {
            base_color: Color::srgb(0.8, 0.82, 0.86),
            metallic: 0.0,
            perceptual_roughness: 0.5,
            emissive: LinearRgba::BLACK,
        }
    }
}

/// Serializable 2D primitive mesh descriptor hydrated into `Mesh2d`.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[reflect(Component, Default)]
pub enum MeshPrimitive2d {
    /// 2D rectangle with full `[width, height]` in world/pixel units.
    Rectangle {
        /// Rectangle width and height.
        size: Vec2,
    },
    /// 2D filled circle with the given radius.
    Circle {
        /// Circle radius.
        radius: f32,
    },
}

impl Default for MeshPrimitive2d {
    fn default() -> Self {
        Self::Rectangle {
            size: Vec2::splat(64.0),
        }
    }
}

/// Serializable 2D flat color material descriptor hydrated into `MeshMaterial2d<ColorMaterial>`.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[reflect(Component, Default)]
pub struct ColorMaterialDesc {
    /// Material color.
    pub color: Color,
}

impl Default for ColorMaterialDesc {
    fn default() -> Self {
        Self {
            color: Color::srgb(0.24, 0.62, 0.96),
        }
    }
}

/// Serializable 2D sprite descriptor hydrated into Bevy's `Sprite`.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[reflect(Component, Default)]
pub struct SpriteDesc {
    /// Tint color of the sprite.
    pub color: Color,
    /// Optional explicit sprite dimensions in world/pixel units.
    pub custom_size: Option<Vec2>,
    /// Mirror horizontally.
    pub flip_x: bool,
    /// Mirror vertically.
    pub flip_y: bool,
}

impl Default for SpriteDesc {
    fn default() -> Self {
        Self {
            color: Color::srgb(0.95, 0.55, 0.25),
            custom_size: Some(Vec2::splat(64.0)),
            flip_x: false,
            flip_y: false,
        }
    }
}

/// Serializable 3D perspective camera descriptor hydrated into `Camera3d`.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[reflect(Component, Default)]
pub struct SceneCamera3d {
    /// Vertical field of view in degrees.
    pub fov_degrees: f32,
    /// Whether this camera renders when scene cameras are enabled.
    pub active: bool,
}

impl Default for SceneCamera3d {
    fn default() -> Self {
        Self {
            fov_degrees: 45.0,
            active: true,
        }
    }
}

/// Serializable 2D orthographic camera descriptor hydrated into `Camera2d`.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[reflect(Component, Default)]
pub struct SceneCamera2d {
    /// Orthographic scale factor (`1.0` = 1:1 world units to pixels).
    pub scale: f32,
    /// Whether this camera renders when scene cameras are enabled.
    pub active: bool,
}

impl Default for SceneCamera2d {
    fn default() -> Self {
        Self {
            scale: 1.0,
            active: true,
        }
    }
}

/// Serializable directional light descriptor hydrated into `DirectionalLight`.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[reflect(Component, Default)]
pub struct SceneDirectionalLight {
    /// Light color.
    pub color: Color,
    /// Illuminance in lux.
    pub illuminance: f32,
    /// Whether shadow mapping is enabled.
    pub shadows_enabled: bool,
}

impl Default for SceneDirectionalLight {
    fn default() -> Self {
        Self {
            color: Color::WHITE,
            illuminance: 10_000.0,
            shadows_enabled: true,
        }
    }
}

/// Serializable point light descriptor hydrated into `PointLight`.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[reflect(Component, Default)]
pub struct ScenePointLight {
    /// Light color.
    pub color: Color,
    /// Luminous power in lumens.
    pub intensity: f32,
    /// Attenuation cut-off radius.
    pub range: f32,
    /// Whether shadow mapping is enabled.
    pub shadows_enabled: bool,
}

impl Default for ScenePointLight {
    fn default() -> Self {
        Self {
            color: Color::WHITE,
            intensity: 100_000.0,
            range: 20.0,
            shadows_enabled: false,
        }
    }
}

/// Metadata tracking the currently active scene in the world.
#[derive(Resource, Reflect, Debug, Clone, PartialEq, Eq)]
#[reflect(Resource, Default)]
pub struct ActiveScene {
    /// Backing `.level.ron` path on disk, if saved or loaded from disk.
    pub path: Option<PathBuf>,
    /// Human-readable scene title.
    pub name: String,
    /// Dimensional mode associated with the active scene.
    pub world_mode: WorldMode,
    /// Whether the scene has unsaved modifications.
    pub dirty: bool,
}

impl Default for ActiveScene {
    fn default() -> Self {
        Self {
            path: None,
            name: "Untitled Scene".to_owned(),
            world_mode: WorldMode::Hybrid,
            dirty: false,
        }
    }
}

/// Controls whether hydrated scene cameras (`SceneCamera3d` / `SceneCamera2d`) activate
/// their underlying Bevy [`bevy_camera::Camera`].
///
/// Standalone runtime builds leave this `true`, whereas the Editor Host sets it to `false`
/// while editing so the editor viewport camera owns rendering.
#[derive(Resource, Reflect, Debug, Clone, Copy, PartialEq, Eq)]
#[reflect(Resource, Default)]
pub struct SceneCameraActivationPolicy {
    /// When `false`, hydrated scene cameras keep `Camera::is_active = false`.
    pub allow_scene_cameras: bool,
}

impl Default for SceneCameraActivationPolicy {
    fn default() -> Self {
        Self {
            allow_scene_cameras: true,
        }
    }
}
