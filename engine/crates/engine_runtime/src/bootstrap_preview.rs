//! Default 2D/3D bootstrap preview scene and HUD for empty windowed launches.

use bevy::{prelude::*, text::FontSize, ui::px};
use engine_core::{ProjectManifest, WorldMode};
use engine_scene::{SceneEntity, SceneHydrationSystems};

/// Marker component for the animated object in the Stage 0 bootstrap preview.
#[derive(Component, Debug, Clone, Copy)]
pub struct BootstrapSpinner {
    /// Rotates around Z in 2D mode, or around X/Y in 3D mode.
    pub is_2d: bool,
}

/// Spawns a lightweight 2D or 3D fallback scene and status HUD in windowed mode
/// whenever no scene or camera has been created yet.
#[derive(Debug, Default, Clone, Copy)]
pub struct BootstrapPreviewPlugin;

impl Plugin for BootstrapPreviewPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostStartup,
            spawn_bootstrap_preview_if_empty.after(SceneHydrationSystems),
        )
        .add_systems(Update, animate_bootstrap_spinner);
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Queries fallback state and inserts meshes, materials, lights, and UI"
)]
fn spawn_bootstrap_preview_if_empty(
    mut commands: Commands,
    cameras: Query<(), With<Camera>>,
    scene_entities: Query<(), With<SceneEntity>>,
    manifest: Option<Res<ProjectManifest>>,
    world_mode: Option<Res<WorldMode>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut standard_materials: ResMut<Assets<StandardMaterial>>,
    mut color_materials: ResMut<Assets<ColorMaterial>>,
) {
    if !cameras.is_empty() || !scene_entities.is_empty() {
        return;
    }

    let active_mode = world_mode.map_or(WorldMode::Mode3D, |mode| *mode);
    let project_name = manifest
        .as_deref()
        .map_or("Untitled Project", |m| m.project.name.as_str());

    commands.insert_resource(ClearColor(Color::srgb(0.08, 0.10, 0.14)));

    if active_mode.supports_3d() {
        commands.spawn((
            Name::new("Bootstrap 3D Camera"),
            Camera3d::default(),
            Transform::from_xyz(3.2, 2.4, 4.8).looking_at(Vec3::new(0.0, 0.4, 0.0), Vec3::Y),
        ));

        commands.spawn((
            Name::new("Bootstrap Key Light"),
            DirectionalLight {
                illuminance: 12_000.0,
                shadow_maps_enabled: true,
                ..Default::default()
            },
            Transform::from_xyz(4.0, 8.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
        ));

        commands.spawn((
            Name::new("Bootstrap Floor"),
            Mesh3d(meshes.add(Plane3d::default().mesh().size(8.0, 8.0))),
            MeshMaterial3d(standard_materials.add(StandardMaterial {
                base_color: Color::srgb(0.16, 0.19, 0.25),
                perceptual_roughness: 0.85,
                ..Default::default()
            })),
        ));

        commands.spawn((
            Name::new("Bootstrap 3D Cube"),
            Mesh3d(meshes.add(Cuboid::new(1.2, 1.2, 1.2))),
            MeshMaterial3d(standard_materials.add(StandardMaterial {
                base_color: Color::srgb(0.24, 0.62, 0.96),
                metallic: 0.15,
                perceptual_roughness: 0.35,
                ..Default::default()
            })),
            Transform::from_xyz(0.0, 0.75, 0.0),
            BootstrapSpinner { is_2d: false },
        ));
    } else {
        commands.spawn((Name::new("Bootstrap 2D Camera"), Camera2d));

        commands.spawn((
            Name::new("Bootstrap 2D Quad"),
            Mesh2d(meshes.add(Rectangle::new(180.0, 180.0))),
            MeshMaterial2d(color_materials.add(Color::srgb(0.24, 0.62, 0.96))),
            Transform::default(),
            BootstrapSpinner { is_2d: true },
        ));
    }

    let hud_text = format!(
        "Bevy9 Engine | Project: {project_name} | Mode: {active_mode}\nStage 0 bootstrap preview (no scene loaded yet)"
    );

    commands
        .spawn((
            Name::new("Bootstrap HUD"),
            Node {
                position_type: PositionType::Absolute,
                top: px(16),
                left: px(16),
                padding: UiRect::axes(px(12), px(8)),
                ..Default::default()
            },
            BackgroundColor(Color::srgba(0.04, 0.06, 0.09, 0.82)),
        ))
        .with_child((
            Text::new(hud_text),
            TextFont {
                font_size: FontSize::Px(16.0),
                ..Default::default()
            },
            TextColor(Color::srgb(0.92, 0.95, 1.0)),
        ));
}

fn animate_bootstrap_spinner(
    time: Res<Time>,
    mut spinners: Query<(&BootstrapSpinner, &mut Transform)>,
) {
    let dt = time.delta_secs();
    for (spinner, mut transform) in &mut spinners {
        if spinner.is_2d {
            transform.rotate_z(0.8 * dt);
        } else {
            transform.rotate_y(0.7 * dt);
            transform.rotate_x(0.35 * dt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;

    fn test_preview_app(mode: WorldMode) -> App {
        let mut app = App::new();
        app.insert_resource(mode)
            .insert_resource(ProjectManifest::new("Preview Test", mode))
            .add_plugins((
                MinimalPlugins,
                AssetPlugin {
                    use_asset_processor_override: Some(false),
                    ..Default::default()
                },
            ))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .init_asset::<ColorMaterial>()
            .add_plugins(BootstrapPreviewPlugin);
        app
    }

    #[test]
    fn spawns_3d_preview_when_no_camera_exists() {
        let mut app = test_preview_app(WorldMode::Hybrid);
        app.update();

        let world = app.world_mut();
        assert_eq!(world.query::<&Camera3d>().iter(world).count(), 1);
        assert_eq!(world.query::<&DirectionalLight>().iter(world).count(), 1);
        let spinner = world
            .query::<&BootstrapSpinner>()
            .single(world)
            .expect("3D preview should spawn one spinner");
        assert!(!spinner.is_2d);
        assert!(world.contains_resource::<ClearColor>());
    }

    #[test]
    fn spawns_2d_preview_in_mode_2d() {
        let mut app = test_preview_app(WorldMode::Mode2D);
        app.update();

        let world = app.world_mut();
        assert_eq!(world.query::<&Camera2d>().iter(world).count(), 1);
        let spinner = world
            .query::<&BootstrapSpinner>()
            .single(world)
            .expect("2D preview should spawn one spinner");
        assert!(spinner.is_2d);
    }

    #[test]
    fn skips_preview_when_camera_already_exists() {
        let mut app = test_preview_app(WorldMode::Mode3D);
        app.world_mut().spawn(Camera3d::default());
        app.update();

        let world = app.world_mut();
        assert_eq!(world.query::<&Camera3d>().iter(world).count(), 1);
        assert_eq!(world.query::<&BootstrapSpinner>().iter(world).count(), 0);
    }
}
