//! Procedural 2D and 3D editor viewport reference grids.

use core::f32::consts::FRAC_PI_2;

use bevy_color::Color;
use bevy_ecs::system::Res;
use bevy_gizmos::gizmos::Gizmos;
use bevy_math::{Isometry2d, Isometry3d, Quat, UVec2, Vec2, Vec3};
use engine_protocol::ViewportCameraMode;

use crate::viewport::ViewportConfig;

pub(crate) fn draw_editor_grid(config: Res<ViewportConfig>, mut gizmos: Gizmos) {
    if !config.show_grid {
        return;
    }

    match config.camera_mode {
        ViewportCameraMode::Mode3D => {
            gizmos.grid(
                Isometry3d::from_rotation(Quat::from_rotation_x(FRAC_PI_2)),
                UVec2::splat(20),
                Vec2::splat(1.0),
                Color::srgba(0.32, 0.36, 0.44, 0.65),
            );
            gizmos.line(Vec3::ZERO, Vec3::X * 2.0, Color::srgb(0.90, 0.28, 0.28));
            gizmos.line(Vec3::ZERO, Vec3::Z * 2.0, Color::srgb(0.28, 0.56, 0.92));
        }
        ViewportCameraMode::Mode2D => {
            gizmos.grid_2d(
                Isometry2d::IDENTITY,
                UVec2::splat(24),
                Vec2::splat(64.0),
                Color::srgba(0.32, 0.36, 0.44, 0.55),
            );
            gizmos.line_2d(Vec2::ZERO, Vec2::X * 128.0, Color::srgb(0.90, 0.28, 0.28));
            gizmos.line_2d(Vec2::ZERO, Vec2::Y * 128.0, Color::srgb(0.32, 0.85, 0.44));
        }
    }
}
