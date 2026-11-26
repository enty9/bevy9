//! Systems that hydrate serializable scene descriptors into live Bevy render components.

use std::collections::{HashMap, HashSet};

use bevy_asset::{AssetServer, Assets};
use bevy_camera::{
    visibility::Visibility, Camera, Camera2d, Camera3d, OrthographicProjection,
    PerspectiveProjection, Projection,
};
use bevy_color::Color;
use bevy_ecs::{
    component::Component,
    entity::Entity,
    hierarchy::ChildOf,
    query::{Added, Changed, With, Without},
    system::{Commands, Query, Res, ResMut},
};
use bevy_image::{TextureAtlas, TextureAtlasLayout};
use bevy_light::{DirectionalLight, PointLight};
use bevy_math::{UVec2, Vec2, Vec3};
use bevy_mesh::{Mesh, Mesh2d, Mesh3d, Meshable};
use bevy_pbr::{MeshMaterial3d, StandardMaterial};
use bevy_picking::Pickable;
use bevy_shape::{Circle, Cuboid, Cylinder, Plane3d, Rectangle, Sphere};
use bevy_sprite::Sprite;
use bevy_sprite_render::{ColorMaterial, MeshMaterial2d};
use bevy_transform::components::Transform;
use bevy_world_serialization::WorldAssetRoot;
use engine_assets::{
    build_atlas_layout_for_uuid, load_gltf_scene_by_uuid, load_image_by_uuid,
    load_tilemap_from_file, AssetRegistry, SharedAssetRegistry,
};
use engine_core::AssetUuid;

use crate::{
    components::{
        ColorMaterialDesc, MeshPrimitive2d, MeshPrimitive3d, SceneCamera2d, SceneCamera3d,
        SceneCameraActivationPolicy, SceneDirectionalLight, SceneGltfModel, ScenePointLight,
        SceneTilemap, SpriteDesc, StandardMaterialDesc,
    },
    prefab::PrefabInstance,
};

/// Internal marker component on the spawned child `WorldAssetRoot` entity for a hydrated [`SceneGltfModel`].
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct HydratedGltfModelRoot;

/// Internal marker component on each spawned child tile entity for a hydrated [`SceneTilemap`].
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct HydratedTilemapTile;

pub(crate) fn hydrate_3d_meshes_and_materials(
    mut commands: Commands,
    mut meshes: Option<ResMut<Assets<Mesh>>>,
    mut standard_materials: Option<ResMut<Assets<StandardMaterial>>>,
    registry: Option<Res<SharedAssetRegistry>>,
    asset_server: Option<Res<AssetServer>>,
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
            let base_color_texture = desc.base_color_texture.and_then(|uuid| {
                let reg = registry.as_deref()?;
                let server = asset_server.as_deref()?;
                load_image_by_uuid(uuid, reg, server)
            });
            let material = StandardMaterial {
                base_color: desc.base_color,
                base_color_texture,
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
    registry: Option<Res<SharedAssetRegistry>>,
    asset_server: Option<Res<AssetServer>>,
    mut atlas_layouts: Option<ResMut<Assets<TextureAtlasLayout>>>,
    sprite_query: Query<(Entity, &SpriteDesc), Changed<SpriteDesc>>,
) {
    for (entity, desc) in &sprite_query {
        let mut sprite = Sprite {
            color: desc.color,
            custom_size: desc.custom_size,
            flip_x: desc.flip_x,
            flip_y: desc.flip_y,
            ..Default::default()
        };
        if let (Some(uuid), Some(reg), Some(server)) =
            (desc.texture, registry.as_deref(), asset_server.as_deref())
            && let Some(handle) = load_image_by_uuid(uuid, reg, server)
        {
            sprite.image = handle;
            if let (Some(idx), Some(layouts)) = (desc.atlas_index, atlas_layouts.as_deref_mut())
                && let Some(layout) = build_atlas_layout_for_uuid(uuid, reg)
            {
                sprite.texture_atlas = Some(TextureAtlas {
                    layout: layouts.add(layout),
                    index: idx,
                });
            }
        }
        commands.entity(entity).insert((sprite, Pickable::default()));
    }
}

pub(crate) fn hydrate_gltf_models(
    mut commands: Commands,
    registry: Option<Res<SharedAssetRegistry>>,
    asset_server: Option<Res<AssetServer>>,
    model_query: Query<(Entity, &SceneGltfModel), Changed<SceneGltfModel>>,
    existing_roots: Query<(Entity, &ChildOf), With<HydratedGltfModelRoot>>,
) {
    for (entity, model) in &model_query {
        for (child_entity, &ChildOf(parent)) in &existing_roots {
            if parent == entity {
                commands.entity(child_entity).despawn();
            }
        }

        let Some(uuid) = model.asset else {
            continue;
        };
        let (Some(reg), Some(server)) = (registry.as_deref(), asset_server.as_deref()) else {
            continue;
        };
        let Some((scene_handle, asset_scale)) =
            load_gltf_scene_by_uuid(uuid, Some(model.scene_index), reg, server)
        else {
            continue;
        };

        let combined_scale = (asset_scale * model.scale_multiplier).max(0.0001);
        commands.entity(entity).insert(Pickable::default());
        commands.spawn((
            HydratedGltfModelRoot,
            WorldAssetRoot(scene_handle),
            Transform::from_scale(Vec3::splat(combined_scale)),
            Visibility::default(),
            ChildOf(entity),
        ));
    }
}

pub(crate) fn tag_gltf_meshes_pickable(
    mut commands: Commands,
    new_meshes: Query<Entity, (Added<Mesh3d>, Without<Pickable>)>,
    parents: Query<&ChildOf>,
    gltf_roots: Query<(), With<HydratedGltfModelRoot>>,
) {
    for mesh_entity in &new_meshes {
        let mut cursor = mesh_entity;
        for _ in 0..16 {
            let Ok(&ChildOf(parent)) = parents.get(cursor) else {
                break;
            };
            if gltf_roots.contains(parent) {
                commands.entity(mesh_entity).insert(Pickable::default());
                break;
            }
            cursor = parent;
        }
    }
}

pub(crate) fn hydrate_tilemaps(
    mut commands: Commands,
    registry: Option<Res<SharedAssetRegistry>>,
    asset_server: Option<Res<AssetServer>>,
    mut atlas_layouts: Option<ResMut<Assets<TextureAtlasLayout>>>,
    tilemap_query: Query<(Entity, &SceneTilemap), Changed<SceneTilemap>>,
    existing_tiles: Query<(Entity, &ChildOf), With<HydratedTilemapTile>>,
) {
    for (entity, tilemap_desc) in &tilemap_query {
        for (child_entity, &ChildOf(parent)) in &existing_tiles {
            if parent == entity {
                commands.entity(child_entity).despawn();
            }
        }

        let Some(uuid) = tilemap_desc.asset else {
            continue;
        };
        let Some(reg) = registry.as_deref() else {
            continue;
        };
        let Some(abs_path) = reg.resolve_abs_path(uuid) else {
            continue;
        };
        let tilemap = match load_tilemap_from_file(&abs_path) {
            Ok(loaded) => loaded,
            Err(err) => {
                tracing::warn!(
                    path = %abs_path.display(),
                    error = %err,
                    "failed to load SceneTilemap asset"
                );
                continue;
            }
        };

        let ppu = reg
            .tilemap_settings(uuid)
            .map_or(1.0, |s| s.pixels_per_unit.max(0.01));
        let scale = (tilemap_desc.tile_scale * ppu).max(0.01);
        let tile_w = (tilemap.tile_size[0].max(1) as f32) * scale;
        let tile_h = (tilemap.tile_size[1].max(1) as f32) * scale;

        let tileset_image = tilemap.tileset_texture.and_then(|tex_uuid| {
            let server = asset_server.as_deref()?;
            load_image_by_uuid(tex_uuid, reg, server)
        });
        let atlas_layout_handle = tileset_image.as_ref().and_then(|_| {
            let layouts = atlas_layouts.as_deref_mut()?;
            Some(layouts.add(TextureAtlasLayout::from_grid(
                UVec2::new(tilemap.tile_size[0].max(1), tilemap.tile_size[1].max(1)),
                tilemap.grid_columns.max(1),
                tilemap.grid_rows.max(1),
                None,
                None,
            )))
        });

        commands.entity(entity).insert(Pickable::default());

        for layer in &tilemap.layers {
            if !layer.visible || layer.width == 0 || layer.height == 0 {
                continue;
            }
            let half_cols = (layer.width as f32 - 1.0) * 0.5;
            let half_rows = (layer.height as f32 - 1.0) * 0.5;
            for row in 0..layer.height {
                for col in 0..layer.width {
                    let idx = (row * layer.width + col) as usize;
                    let Some(Some(tile_index)) = layer.tiles.get(idx).copied() else {
                        continue;
                    };

                    let mut sprite = Sprite {
                        custom_size: Some(Vec2::new(tile_w, tile_h)),
                        color: Color::WHITE,
                        ..Default::default()
                    };
                    if let Some(image_handle) = &tileset_image {
                        sprite.image = image_handle.clone();
                        if let Some(layout_handle) = &atlas_layout_handle {
                            sprite.texture_atlas = Some(TextureAtlas {
                                layout: layout_handle.clone(),
                                index: tile_index as usize,
                            });
                        }
                    } else {
                        // Tint fallback colored cells deterministically when no texture/server exists.
                        let hue = ((tile_index * 67) % 360) as f32;
                        sprite.color = Color::hsl(hue, 0.55, 0.55);
                    }

                    let x = (col as f32 - half_cols) * tile_w;
                    let y = (half_rows - row as f32) * tile_h;
                    commands.spawn((
                        HydratedTilemapTile,
                        sprite,
                        Transform::from_xyz(x, y, layer.z_offset),
                        Visibility::default(),
                        Pickable::default(),
                        ChildOf(entity),
                    ));
                }
            }
        }
    }
}

/// Applies pending relocate/reimport signals from [`AssetRegistry`] before hydration systems execute.
pub(crate) fn flush_asset_rehydration_signals(
    mut registry: Option<ResMut<AssetRegistry>>,
    mut mat_q: Query<&mut StandardMaterialDesc>,
    mut sprite_q: Query<&mut SpriteDesc>,
    mut gltf_q: Query<&mut SceneGltfModel>,
    mut tilemap_q: Query<&mut SceneTilemap>,
    mut prefab_q: Query<&mut PrefabInstance>,
) {
    let Some(reg) = registry.as_deref_mut() else {
        return;
    };
    if reg.relocated_uuids.is_empty() && reg.reimport_uuids.is_empty() {
        return;
    }

    let relocated = core::mem::take(&mut reg.relocated_uuids);
    let reimported = core::mem::take(&mut reg.reimport_uuids);

    let mut affected: HashSet<AssetUuid> = HashSet::new();
    let mut relocated_paths: HashMap<AssetUuid, String> = HashMap::new();
    let shared = reg.shared();

    for uuid in relocated {
        affected.insert(uuid);
        if let Some(rel) = shared.resolve_path(uuid) {
            relocated_paths.insert(uuid, format!("assets/{rel}"));
        }
    }
    for uuid in reimported {
        affected.insert(uuid);
    }

    for mut mat in &mut mat_q {
        if mat
            .base_color_texture
            .is_some_and(|uuid| affected.contains(&uuid))
        {
            bevy_ecs::change_detection::DetectChangesMut::set_changed(&mut mat);
        }
    }
    for mut sprite in &mut sprite_q {
        if sprite.texture.is_some_and(|uuid| affected.contains(&uuid)) {
            bevy_ecs::change_detection::DetectChangesMut::set_changed(&mut sprite);
        }
    }
    for mut gltf in &mut gltf_q {
        if gltf.asset.is_some_and(|uuid| affected.contains(&uuid)) {
            bevy_ecs::change_detection::DetectChangesMut::set_changed(&mut gltf);
        }
    }
    for mut tilemap in &mut tilemap_q {
        if tilemap.asset.is_some_and(|uuid| affected.contains(&uuid)) {
            bevy_ecs::change_detection::DetectChangesMut::set_changed(&mut tilemap);
        }
    }
    if !relocated_paths.is_empty() {
        for mut prefab in &mut prefab_q {
            if let Some(uuid) = prefab.prefab_uuid
                && let Some(new_path) = relocated_paths.get(&uuid)
                && &prefab.prefab_path != new_path
            {
                prefab.prefab_path = new_path.clone();
            }
        }
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
