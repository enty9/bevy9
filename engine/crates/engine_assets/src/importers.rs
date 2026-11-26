//! High-level 2D image/atlas and 3D `glTF` asset loading helpers parameterized by `.b9meta` import settings.

use bevy_asset::{AssetServer, Handle, RenderAssetUsages};
use bevy_gltf::{GltfAssetLabel, GltfLoaderSettings};
use bevy_image::{Image, ImageLoaderSettings, ImageSampler, TextureAtlasLayout};
use bevy_math::UVec2;
use bevy_world_serialization::WorldAsset;
use engine_core::AssetUuid;
use engine_protocol::TextureFilterMode;

use crate::registry::SharedAssetRegistry;

/// Resolves an image [`AssetUuid`] via `registry` and loads it through `asset_server`
/// applying its persisted [`engine_protocol::ImageImportSettings`].
#[must_use]
pub fn load_image_by_uuid(
    uuid: AssetUuid,
    registry: &SharedAssetRegistry,
    asset_server: &AssetServer,
) -> Option<Handle<Image>> {
    let rel_path = registry.resolve_path(uuid)?;
    let settings = registry.image_settings(uuid).unwrap_or_default();
    let is_srgb = settings.srgb;
    let sampler = match settings.filter_mode {
        TextureFilterMode::Linear => ImageSampler::linear(),
        TextureFilterMode::Nearest => ImageSampler::nearest(),
    };

    Some(
        asset_server
            .load_builder()
            .with_settings::<ImageLoaderSettings>(move |loader_settings| {
                loader_settings.is_srgb = is_srgb;
                loader_settings.sampler = sampler.clone();
            })
            .load(rel_path),
    )
}

/// Builds a [`TextureAtlasLayout`] from an image asset's `.b9meta` sprite-sheet grid settings, if configured.
#[must_use]
pub fn build_atlas_layout_for_uuid(
    uuid: AssetUuid,
    registry: &SharedAssetRegistry,
) -> Option<TextureAtlasLayout> {
    let grid = registry.image_settings(uuid)?.atlas_grid?;
    let padding = (grid.padding[0] > 0 || grid.padding[1] > 0)
        .then_some(UVec2::new(grid.padding[0], grid.padding[1]));
    let offset = (grid.offset[0] > 0 || grid.offset[1] > 0)
        .then_some(UVec2::new(grid.offset[0], grid.offset[1]));
    Some(TextureAtlasLayout::from_grid(
        UVec2::new(grid.tile_size[0].max(1), grid.tile_size[1].max(1)),
        grid.columns.max(1),
        grid.rows.max(1),
        padding,
        offset,
    ))
}

/// Resolves a `glTF`/`.glb` [`AssetUuid`] via `registry` and loads the requested scene sub-asset,
/// returning `(world_asset_handle, uniform_scale)`.
#[must_use]
pub fn load_gltf_scene_by_uuid(
    uuid: AssetUuid,
    scene_index_override: Option<usize>,
    registry: &SharedAssetRegistry,
    asset_server: &AssetServer,
) -> Option<(Handle<WorldAsset>, f32)> {
    let rel_path = registry.resolve_path(uuid)?;
    let settings = registry.gltf_settings(uuid).unwrap_or_default();
    let scene_idx = scene_index_override.unwrap_or(0);
    let asset_path = GltfAssetLabel::Scene(scene_idx).from_asset(rel_path);

    let mesh_usage = if settings.load_meshes {
        RenderAssetUsages::default()
    } else {
        RenderAssetUsages::empty()
    };
    let material_usage = if settings.load_materials {
        RenderAssetUsages::default()
    } else {
        RenderAssetUsages::empty()
    };
    let load_cameras = settings.load_cameras;
    let load_lights = settings.load_lights;
    let load_animations = settings.load_animations;

    let handle = asset_server
        .load_builder()
        .with_settings::<GltfLoaderSettings>(move |loader_settings| {
            loader_settings.load_meshes = mesh_usage;
            loader_settings.load_materials = material_usage;
            loader_settings.load_cameras = load_cameras;
            loader_settings.load_lights = load_lights;
            loader_settings.load_animations = load_animations;
        })
        .load(asset_path);

    Some((handle, settings.scale.max(0.0001)))
}
