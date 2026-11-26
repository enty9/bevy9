//! Offscreen and native-window viewport transport (`ViewportTransport`).

use bevy_asset::{Assets, Handle};
use bevy_camera::{ClearColor, Projection, RenderTarget};
use bevy_color::Color;
use bevy_ecs::{
    change_detection::DetectChangesMut,
    component::Component,
    entity::Entity,
    message::MessageReader,
    observer::On,
    query::{Or, With},
    resource::Resource,
    system::{Commands, Query, Res, ResMut},
};
use bevy_image::Image;
use bevy_math::UVec2;
use bevy_render::{
    gpu_readback::{Readback, ReadbackComplete},
    render_resource::{TextureFormat, TextureUsages},
};
use bevy_time::Time;
use bevy_window::{Window, WindowCloseRequested, WindowRef, WindowResolution};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use engine_protocol::{
    PixelFormat, ViewportCameraMode, ViewportFrameHeader, ViewportInputEvent, ViewportMode,
    PROTOCOL_VERSION,
};

use crate::camera::{EditorCamera2d, EditorCamera3d};

/// Minimum allowed offscreen viewport dimension in pixels.
pub const MIN_VIEWPORT_SIZE: u32 = 64;

/// Maximum allowed offscreen viewport dimension in pixels.
pub const MAX_VIEWPORT_SIZE: u32 = 4096;

/// GPU row alignment required by `wgpu` texture-to-buffer copies (256 bytes).
pub const WGPU_COPY_BYTES_PER_ROW_ALIGNMENT: u32 = 256;

/// Clamps requested viewport dimensions to safe GPU bounds.
#[must_use]
pub fn clamp_viewport_dimensions(width: u32, height: u32) -> (u32, u32) {
    (
        width.clamp(MIN_VIEWPORT_SIZE, MAX_VIEWPORT_SIZE),
        height.clamp(MIN_VIEWPORT_SIZE, MAX_VIEWPORT_SIZE),
    )
}

/// Aligns an unpadded row byte stride up to [`WGPU_COPY_BYTES_PER_ROW_ALIGNMENT`].
#[must_use]
pub const fn align_bytes_per_row(unpadded_bytes_per_row: u32) -> u32 {
    if unpadded_bytes_per_row == 0 {
        return 0;
    }
    unpadded_bytes_per_row.div_ceil(WGPU_COPY_BYTES_PER_ROW_ALIGNMENT)
        * WGPU_COPY_BYTES_PER_ROW_ALIGNMENT
}

/// Strips GPU row-alignment padding from a raw `RGBA8` readback buffer into tightly
/// packed `width * height * 4` bytes.
///
/// Returns `None` if `width` or `height` is zero or `padded.len()` does not match the
/// exact aligned buffer size for `(width, height)`.
#[must_use]
pub fn unpack_aligned_rgba8(padded: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }

    let unpadded_row =
        (width as usize).checked_mul(PixelFormat::Rgba8UnormSrgb.bytes_per_pixel())?;
    let unpadded_row_u32 = u32::try_from(unpadded_row).ok()?;
    let aligned_row = align_bytes_per_row(unpadded_row_u32) as usize;
    let height_usize = height as usize;
    let expected_padded_len = aligned_row.checked_mul(height_usize)?;

    if padded.len() != expected_padded_len {
        return None;
    }

    if aligned_row == unpadded_row {
        return Some(padded.to_vec());
    }

    let mut packed = Vec::with_capacity(unpadded_row.checked_mul(height_usize)?);
    for row in 0..height_usize {
        let start = row * aligned_row;
        let end = start + unpadded_row;
        packed.extend_from_slice(&padded[start..end]);
    }
    Some(packed)
}

/// Marker component attached to the dynamically spawned detached OS viewport window.
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct DetachedViewportWindow;

/// Active configuration for the editor viewport presentation and camera mode.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct ViewportConfig {
    /// Whether the viewport renders offscreen to IPC or into the detached native window.
    pub mode: ViewportMode,
    /// Active editor camera navigation mode (`Mode3D` or `Mode2D`).
    pub camera_mode: ViewportCameraMode,
    /// Target render width in physical pixels.
    pub width: u32,
    /// Target render height in physical pixels.
    pub height: u32,
    /// Maximum frame capture rate for offscreen readback (`0` disables throttling).
    pub target_fps: u32,
    /// Enables procedural 2D/3D editor grid rendering.
    pub show_grid: bool,
    /// Background clear color for editor viewports.
    pub clear_color: Color,
    /// Window title used when spawning the detached [`DetachedViewportWindow`].
    pub window_title: String,
}

impl Default for ViewportConfig {
    fn default() -> Self {
        Self {
            mode: ViewportMode::Offscreen,
            camera_mode: ViewportCameraMode::Mode3D,
            width: 1280,
            height: 720,
            target_fps: 30,
            show_grid: true,
            clear_color: Color::srgb(0.08, 0.10, 0.14),
            window_title: "Bevy9 Viewport".to_owned(),
        }
    }
}

/// Captured offscreen viewport frame ready to be encoded and sent over IPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportFramePacket {
    /// Encoded header describing dimensions, pixel format, and sequence counter.
    pub header: ViewportFrameHeader,
    /// Tightly packed `RGBA8` pixels (`width * height * 4` bytes).
    pub pixels: Vec<u8>,
}

/// Non-blocking bounded channel carrying captured offscreen frames from Bevy to IPC.
#[derive(Resource, Debug, Clone)]
pub struct ViewportFrameChannel {
    sender: Sender<ViewportFramePacket>,
    receiver: Receiver<ViewportFramePacket>,
}

impl Default for ViewportFrameChannel {
    fn default() -> Self {
        let (sender, receiver) = bounded(2);
        Self { sender, receiver }
    }
}

impl ViewportFrameChannel {
    /// Pushes `packet`, dropping the oldest queued frame if the bounded channel is full.
    pub fn try_send_latest(&self, packet: ViewportFramePacket) {
        match self.sender.try_send(packet) {
            Ok(()) => {}
            Err(TrySendError::Full(latest)) => {
                let _ = self.receiver.try_recv();
                let _ = self.sender.try_send(latest);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Returns a cloneable [`Receiver`] for draining captured frames on the IPC thread.
    #[must_use]
    pub fn receiver(&self) -> Receiver<ViewportFramePacket> {
        self.receiver.clone()
    }
}

/// Queue of forwarded UI `<canvas>` input and resize events drained each frame.
#[derive(Resource, Default, Debug)]
pub struct ViewportInputQueue {
    events: Vec<ViewportInputEvent>,
}

impl ViewportInputQueue {
    /// Enqueues a single forwarded input event.
    pub fn push(&mut self, event: ViewportInputEvent) {
        self.events.push(event);
    }

    /// Drains all queued input events in arrival order.
    pub fn drain_all(&mut self) -> Vec<ViewportInputEvent> {
        core::mem::take(&mut self.events)
    }

    /// Returns an immutable slice of currently queued events.
    #[must_use]
    pub fn as_slice(&self) -> &[ViewportInputEvent] {
        &self.events
    }
}

/// Internal tracking state for offscreen render target allocation, readback, and detached window lifecycle.
#[derive(Resource, Default, Debug)]
pub struct OffscreenRenderState {
    /// Handle of the offscreen render target image, when allocated.
    pub image_handle: Option<Handle<Image>>,
    /// Entity holding the active [`Readback`] component, when attached.
    pub readback_entity: Option<Entity>,
    /// Entity of the dynamically spawned detached viewport window in [`ViewportMode::NativeWindow`].
    pub detached_window: Option<Entity>,
    /// Current allocated image dimensions.
    pub allocated_size: UVec2,
    /// Monotonic frame sequence counter for emitted packets.
    pub frame_counter: u64,
    /// Timestamp (in elapsed seconds) of the last captured offscreen frame.
    pub last_capture_secs: f64,
}

/// Creates a 2D `RGBA8` offscreen texture usable both as a [`RenderTarget`] and a GPU readback source.
#[must_use]
pub fn create_offscreen_target_image(width: u32, height: u32) -> Image {
    let (width, height) = clamp_viewport_dimensions(width, height);
    let mut image = Image::new_target_texture(width, height, TextureFormat::Rgba8UnormSrgb, None);
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    image
}

pub(crate) fn process_viewport_resize_inputs(
    mut config: ResMut<ViewportConfig>,
    frame_input: Res<crate::picking::ViewportFrameInput>,
) {
    for event in &frame_input.events {
        if let ViewportInputEvent::Resize { width, height, .. } = *event {
            let (clamped_w, clamped_h) = clamp_viewport_dimensions(width, height);
            if config.width != clamped_w || config.height != clamped_h {
                config.width = clamped_w;
                config.height = clamped_h;
            }
        }
    }
}

type EditorCameraTargetFilter = Or<(With<EditorCamera3d>, With<EditorCamera2d>)>;

pub(crate) fn sync_viewport_transport(
    mut commands: Commands,
    mut config: ResMut<ViewportConfig>,
    mut state: ResMut<OffscreenRenderState>,
    mut images: Option<ResMut<Assets<Image>>>,
    mut close_requests: MessageReader<WindowCloseRequested>,
    mut editor_cameras: Query<(&mut RenderTarget, &mut Projection), EditorCameraTargetFilter>,
) {
    // Closing the detached native viewport window returns it to embedded Offscreen mode
    // instead of exiting the Editor Host.
    if config.mode == ViewportMode::NativeWindow {
        let detached = state.detached_window;
        let should_attach = close_requests
            .read()
            .any(|event| detached.is_none_or(|win| event.window == win));
        if should_attach {
            config.mode = ViewportMode::Offscreen;
        }
    } else {
        close_requests.clear();
    }

    commands.insert_resource(ClearColor(config.clear_color));

    let (target_w, target_h) = clamp_viewport_dimensions(config.width, config.height);
    let desired_size = UVec2::new(target_w, target_h);

    match config.mode {
        ViewportMode::Offscreen => {
            if let Some(detached) = state.detached_window.take()
                && commands.get_entity(detached).is_ok()
            {
                commands.entity(detached).despawn();
                tracing::info!(window = ?detached, "Detached viewport window closed; returning to Offscreen mode");
            }

            if let Some(images) = images.as_deref_mut() {
                let needs_realloc =
                    state.image_handle.is_none() || state.allocated_size != desired_size;
                if needs_realloc {
                    let image = create_offscreen_target_image(target_w, target_h);
                    let handle = images.add(image);

                    if let Some(old_entity) = state.readback_entity.take()
                        && commands.get_entity(old_entity).is_ok()
                    {
                        commands.entity(old_entity).despawn();
                    }

                    let readback_entity = commands
                        .spawn(Readback::texture(handle.clone()))
                        .observe(on_readback_complete)
                        .id();

                    state.image_handle = Some(handle);
                    state.readback_entity = Some(readback_entity);
                    state.allocated_size = desired_size;
                } else if state.readback_entity.is_none()
                    && let Some(handle) = state.image_handle.clone()
                {
                    let readback_entity = commands
                        .spawn(Readback::texture(handle))
                        .observe(on_readback_complete)
                        .id();
                    state.readback_entity = Some(readback_entity);
                }
            }

            if let Some(handle) = state.image_handle.as_ref() {
                for (mut target, mut projection) in &mut editor_cameras {
                    let already_matches =
                        matches!(&*target, RenderTarget::Image(img) if img.handle == *handle);
                    if !already_matches {
                        *target = RenderTarget::from(handle.clone());
                        // Force `camera_system` to recompute `target_info` after RenderTarget change.
                        projection.set_changed();
                    }
                }
            }
        }
        ViewportMode::NativeWindow => {
            if let Some(old_entity) = state.readback_entity.take()
                && commands.get_entity(old_entity).is_ok()
            {
                commands.entity(old_entity).despawn();
            }

            let window_entity = match state.detached_window {
                Some(existing) if commands.get_entity(existing).is_ok() => existing,
                _ => {
                    let spawned = commands
                        .spawn((
                            DetachedViewportWindow,
                            Window {
                                title: config.window_title.clone(),
                                resolution: WindowResolution::new(target_w, target_h),
                                resizable: true,
                                visible: true,
                                focused: false,
                                ..Default::default()
                            },
                        ))
                        .id();
                    state.detached_window = Some(spawned);
                    tracing::info!(
                        window = ?spawned,
                        width = target_w,
                        height = target_h,
                        "Spawned detached native viewport window"
                    );
                    spawned
                }
            };

            for (mut target, mut projection) in &mut editor_cameras {
                let already_matches = matches!(
                    &*target,
                    RenderTarget::Window(WindowRef::Entity(id)) if *id == window_entity
                );
                if !already_matches {
                    *target = RenderTarget::Window(WindowRef::Entity(window_entity));
                    // Force `camera_system` to recompute `target_info` after RenderTarget change.
                    projection.set_changed();
                }
            }
        }
    }
}

fn on_readback_complete(
    event: On<ReadbackComplete>,
    config: Res<ViewportConfig>,
    mut state: ResMut<OffscreenRenderState>,
    time: Res<Time>,
    channel: Res<ViewportFrameChannel>,
) {
    if config.mode != ViewportMode::Offscreen || state.readback_entity != Some(event.entity) {
        return;
    }

    let now = time.elapsed_secs_f64();
    if config.target_fps > 0 {
        let min_interval = 1.0 / f64::from(config.target_fps);
        if state.last_capture_secs > 0.0 && (now - state.last_capture_secs) < min_interval {
            return;
        }
    }

    let width = state.allocated_size.x;
    let height = state.allocated_size.y;
    let Some(pixels) = unpack_aligned_rgba8(&event.data, width, height) else {
        return;
    };

    let Ok(payload_len) = u32::try_from(pixels.len()) else {
        return;
    };

    state.last_capture_secs = now;
    state.frame_counter = state.frame_counter.wrapping_add(1);

    let header = ViewportFrameHeader {
        protocol_version: PROTOCOL_VERSION,
        frame_id: state.frame_counter,
        width,
        height,
        format: PixelFormat::Rgba8UnormSrgb,
        payload_len,
    };

    channel.try_send_latest(ViewportFramePacket { header, pixels });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpack_aligned_rgba8_handles_aligned_and_unaligned_widths() {
        for (width, height) in [(64, 4), (100, 3), (320, 2), (800, 3), (1280, 2)] {
            let unpadded_row = (width * 4) as usize;
            let aligned_row = align_bytes_per_row(width * 4) as usize;
            assert_eq!(aligned_row % WGPU_COPY_BYTES_PER_ROW_ALIGNMENT as usize, 0);

            let mut padded = vec![0xAAu8; aligned_row * height as usize];
            let mut expected = Vec::with_capacity(unpadded_row * height as usize);

            for y in 0..height as usize {
                for x in 0..unpadded_row {
                    let byte = ((x + y * 17) % 251) as u8;
                    padded[y * aligned_row + x] = byte;
                    expected.push(byte);
                }
            }

            let unpacked = unpack_aligned_rgba8(&padded, width, height)
                .expect("valid padded buffer must unpack");
            assert_eq!(unpacked.len(), unpadded_row * height as usize);
            assert_eq!(unpacked, expected);
        }

        assert!(unpack_aligned_rgba8(&[1, 2, 3, 4], 100, 4).is_none());
        assert!(unpack_aligned_rgba8(&[], 0, 10).is_none());
        assert!(unpack_aligned_rgba8(&[], 10, 0).is_none());
    }
}
