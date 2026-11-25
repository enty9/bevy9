//! Built-in 2D and 3D entity preset spawners.

use bevy_camera::visibility::Visibility;
use bevy_color::Color;
use bevy_ecs::{entity::Entity, hierarchy::ChildOf, name::Name, world::World};
use bevy_math::{Vec2, Vec3};
use bevy_transform::components::Transform;
use engine_core::EntityGuid;
use engine_protocol::EntityPresetKind;

use crate::components::{
    ColorMaterialDesc, MeshPrimitive2d, MeshPrimitive3d, SceneCamera2d, SceneCamera3d,
    SceneDirectionalLight, SceneEntity, ScenePointLight, SpriteDesc, StandardMaterialDesc,
};

/// Returns the default display name for an [`EntityPresetKind`].
#[must_use]
pub const fn default_preset_name(preset: EntityPresetKind) -> &'static str {
    match preset {
        EntityPresetKind::Empty => "Entity",
        EntityPresetKind::Cube3d => "Cube",
        EntityPresetKind::Sphere3d => "Sphere",
        EntityPresetKind::Plane3d => "Plane",
        EntityPresetKind::DirectionalLight => "Directional Light",
        EntityPresetKind::PointLight => "Point Light",
        EntityPresetKind::Camera3d => "Camera 3D",
        EntityPresetKind::Sprite2d => "Sprite 2D",
        EntityPresetKind::Quad2d => "Quad 2D",
        EntityPresetKind::Circle2d => "Circle 2D",
        EntityPresetKind::Camera2d => "Camera 2D",
    }
}

/// Spawns a built-in [`EntityPresetKind`] into `world` with [`SceneEntity`], [`EntityGuid`],
/// [`Name`], [`Transform`], and [`Visibility`].
pub fn spawn_preset(
    world: &mut World,
    preset: EntityPresetKind,
    name: Option<String>,
    parent: Option<Entity>,
    transform: Option<Transform>,
) -> (Entity, EntityGuid) {
    let guid = EntityGuid::new();
    let display_name = name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| default_preset_name(preset).to_owned());

    let resolved_transform = transform.unwrap_or_else(|| default_transform_for_preset(preset));
    let mut entity_mut = world.spawn((
        SceneEntity,
        guid,
        Name::new(display_name),
        resolved_transform,
        Visibility::default(),
    ));

    if let Some(parent_entity) = parent {
        entity_mut.insert(ChildOf(parent_entity));
    }

    match preset {
        EntityPresetKind::Empty => {}
        EntityPresetKind::Cube3d => {
            entity_mut.insert((
                MeshPrimitive3d::Cube { size: Vec3::ONE },
                StandardMaterialDesc {
                    base_color: Color::srgb(0.24, 0.62, 0.96),
                    ..Default::default()
                },
            ));
        }
        EntityPresetKind::Sphere3d => {
            entity_mut.insert((
                MeshPrimitive3d::Sphere { radius: 0.5 },
                StandardMaterialDesc {
                    base_color: Color::srgb(0.95, 0.56, 0.24),
                    ..Default::default()
                },
            ));
        }
        EntityPresetKind::Plane3d => {
            entity_mut.insert((
                MeshPrimitive3d::Plane {
                    size: Vec2::splat(10.0),
                },
                StandardMaterialDesc {
                    base_color: Color::srgb(0.22, 0.25, 0.30),
                    perceptual_roughness: 0.85,
                    ..Default::default()
                },
            ));
        }
        EntityPresetKind::DirectionalLight => {
            entity_mut.insert(SceneDirectionalLight::default());
        }
        EntityPresetKind::PointLight => {
            entity_mut.insert(ScenePointLight::default());
        }
        EntityPresetKind::Camera3d => {
            entity_mut.insert(SceneCamera3d::default());
        }
        EntityPresetKind::Sprite2d => {
            entity_mut.insert(SpriteDesc::default());
        }
        EntityPresetKind::Quad2d => {
            entity_mut.insert((
                MeshPrimitive2d::Rectangle {
                    size: Vec2::splat(96.0),
                },
                ColorMaterialDesc::default(),
            ));
        }
        EntityPresetKind::Circle2d => {
            entity_mut.insert((
                MeshPrimitive2d::Circle { radius: 48.0 },
                ColorMaterialDesc {
                    color: Color::srgb(0.32, 0.82, 0.56),
                },
            ));
        }
        EntityPresetKind::Camera2d => {
            entity_mut.insert(SceneCamera2d::default());
        }
    }

    (entity_mut.id(), guid)
}

fn default_transform_for_preset(preset: EntityPresetKind) -> Transform {
    match preset {
        EntityPresetKind::DirectionalLight => {
            Transform::from_xyz(4.0, 8.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y)
        }
        EntityPresetKind::PointLight => Transform::from_xyz(0.0, 3.0, 0.0),
        EntityPresetKind::Camera3d => {
            Transform::from_xyz(3.5, 2.5, 5.5).looking_at(Vec3::ZERO, Vec3::Y)
        }
        _ => Transform::default(),
    }
}
