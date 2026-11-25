//! Systems that hydrate serializable scene descriptors into live Bevy render components.

use bevy_asset::Assets;
use bevy_camera::{
    Camera, Camera2d, Camera3d, OrthographicProjection, PerspectiveProjection, Projection,
};
use bevy_ecs::{
    entity::Entity,
    query::Changed,
    system::{Commands, Query, Res, ResMut},
};
use bevy_light::{DirectionalLight, PointLight};
use bevy_mesh::{Mesh, Mesh2d, Mesh3d, Meshable};
use bevy_pbr::{MeshMaterial3d, StandardMaterial};
use bevy_shape::{Circle, Cuboid, Cylinder, Plane3d, Rectangle, Sphere};
use bevy_sprite::Sprite;
use bevy_sprite_render::{ColorMaterial, MeshMaterial2d};

use crate::components::{
    ColorMaterialDesc, MeshPrimitive2d, MeshPrimitive3d, SceneCamera2d, SceneCamera3d,
    SceneCameraActivationPolicy, SceneDirectionalLight, ScenePointLight, SpriteDesc,
    StandardMaterialDesc,
};

pub(crate) fn hydrate_3d_meshes_and_materials(
    mut commands: Commands,
    mut meshes: Option<ResMut<Assets<Mesh>>>,
    mut standard_materials: Option<ResMut<Assets<StandardMaterial>>>,
    mesh_3d_query: Query<(Entity, &MeshPrimitive3d), Changed<MeshPrimitive3d>>,
    mat_3d_query: Query<(Entity, &StandardMaterialDesc), Changed<StandardMaterialDesc>>,
) {
    if let Some(meshes) = meshes.as_deref_mut() {
        for (entity, primitive) in &mesh_3d_query {
            let mesh = match *primitive {
                MeshPrimitive3d::Cube { size } => Mesh::from(Cuboid::from_size(size)),
                MeshPrimitive3d::Sphere { radius } => Mesh::from(Sphere::new(radius.max(0.001))),
                MeshPrimitive3d::Plane { size } => {
                    Mesh::from(Plane3d::default().mesh().size(size.x, size.y))
                }
                MeshPrimitive3d::Cylinder { radius, height } => {
                    Mesh::from(Cylinder::new(radius.max(0.001), height.max(0.001)))
                }
            };
            commands.entity(entity).insert(Mesh3d(meshes.add(mesh)));
        }
    }

    if let Some(materials) = standard_materials.as_deref_mut() {
        for (entity, desc) in &mat_3d_query {
            let material = StandardMaterial {
                base_color: desc.base_color,
                metallic: desc.metallic,
                perceptual_roughness: desc.perceptual_roughness,
                emissive: desc.emissive,
                ..Default::default()
            };
            commands
                .entity(entity)
                .insert(MeshMaterial3d(materials.add(material)));
        }
    }
}

pub(crate) fn hydrate_2d_meshes_and_materials(
    mut commands: Commands,
    mut meshes: Option<ResMut<Assets<Mesh>>>,
    mut color_materials: Option<ResMut<Assets<ColorMaterial>>>,
    mesh_2d_query: Query<(Entity, &MeshPrimitive2d), Changed<MeshPrimitive2d>>,
    mat_2d_query: Query<(Entity, &ColorMaterialDesc), Changed<ColorMaterialDesc>>,
) {
    if let Some(meshes) = meshes.as_deref_mut() {
        for (entity, primitive) in &mesh_2d_query {
            let mesh = match *primitive {
                MeshPrimitive2d::Rectangle { size } => Mesh::from(Rectangle::from_size(size)),
                MeshPrimitive2d::Circle { radius } => Mesh::from(Circle::new(radius.max(0.001))),
            };
            commands.entity(entity).insert(Mesh2d(meshes.add(mesh)));
        }
    }

    if let Some(materials) = color_materials.as_deref_mut() {
        for (entity, desc) in &mat_2d_query {
            commands.entity(entity).insert(MeshMaterial2d(
                materials.add(ColorMaterial::from(desc.color)),
            ));
        }
    }
}

pub(crate) fn hydrate_sprites(
    mut commands: Commands,
    sprite_query: Query<(Entity, &SpriteDesc), Changed<SpriteDesc>>,
) {
    for (entity, desc) in &sprite_query {
        commands.entity(entity).insert(Sprite {
            color: desc.color,
            custom_size: desc.custom_size,
            flip_x: desc.flip_x,
            flip_y: desc.flip_y,
            ..Default::default()
        });
    }
}

pub(crate) fn hydrate_lights(
    mut commands: Commands,
    directional_query: Query<(Entity, &SceneDirectionalLight), Changed<SceneDirectionalLight>>,
    point_query: Query<(Entity, &ScenePointLight), Changed<ScenePointLight>>,
) {
    for (entity, desc) in &directional_query {
        commands.entity(entity).insert(DirectionalLight {
            color: desc.color,
            illuminance: desc.illuminance,
            shadow_maps_enabled: desc.shadows_enabled,
            ..Default::default()
        });
    }

    for (entity, desc) in &point_query {
        commands.entity(entity).insert(PointLight {
            color: desc.color,
            intensity: desc.intensity,
            range: desc.range,
            shadow_maps_enabled: desc.shadows_enabled,
            ..Default::default()
        });
    }
}

pub(crate) fn hydrate_cameras(
    mut commands: Commands,
    policy: Res<SceneCameraActivationPolicy>,
    cam3d_changed: Query<(Entity, &SceneCamera3d), Changed<SceneCamera3d>>,
    cam3d_all: Query<(Entity, &SceneCamera3d)>,
    cam2d_changed: Query<(Entity, &SceneCamera2d), Changed<SceneCamera2d>>,
    cam2d_all: Query<(Entity, &SceneCamera2d)>,
) {
    let policy_changed = bevy_ecs::change_detection::DetectChanges::is_changed(&policy);

    if policy_changed {
        for (entity, desc) in &cam3d_all {
            insert_hydrated_camera_3d(&mut commands, entity, desc, policy.allow_scene_cameras);
        }
        for (entity, desc) in &cam2d_all {
            insert_hydrated_camera_2d(&mut commands, entity, desc, policy.allow_scene_cameras);
        }
        return;
    }

    for (entity, desc) in &cam3d_changed {
        insert_hydrated_camera_3d(&mut commands, entity, desc, policy.allow_scene_cameras);
    }
    for (entity, desc) in &cam2d_changed {
        insert_hydrated_camera_2d(&mut commands, entity, desc, policy.allow_scene_cameras);
    }
}

fn insert_hydrated_camera_3d(
    commands: &mut Commands,
    entity: Entity,
    desc: &SceneCamera3d,
    allow_scene_cameras: bool,
) {
    commands.entity(entity).insert((
        Camera3d::default(),
        Camera {
            is_active: allow_scene_cameras && desc.active,
            ..Default::default()
        },
        Projection::Perspective(PerspectiveProjection {
            fov: desc.fov_degrees.clamp(5.0, 175.0).to_radians(),
            ..Default::default()
        }),
    ));
}

fn insert_hydrated_camera_2d(
    commands: &mut Commands,
    entity: Entity,
    desc: &SceneCamera2d,
    allow_scene_cameras: bool,
) {
    commands.entity(entity).insert((
        Camera2d,
        Camera {
            is_active: allow_scene_cameras && desc.active,
            ..Default::default()
        },
        Projection::Orthographic(OrthographicProjection {
            scale: desc.scale.max(0.01),
            ..OrthographicProjection::default_2d()
        }),
    ));
}
