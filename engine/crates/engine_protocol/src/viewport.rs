//! Viewport transport modes, forwarded input events, and binary frame codec.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::PROTOCOL_VERSION;

/// Magic 4-byte signature identifying binary viewport frames (`"BV9F"`).
pub const FRAME_MAGIC: [u8; 4] = *b"BV9F";

/// Byte size of the fixed little-endian viewport frame header.
pub const FRAME_HEADER_BYTES: usize = 28;

/// Active presentation transport for the editor viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewportMode {
    /// Renders offscreen into an image buffer and streams frames over IPC to the UI canvas.
    #[default]
    Offscreen,
    /// Displays the detached native Bevy window directly on the OS compositor.
    NativeWindow,
}

/// Active camera navigation mode in the editor viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ViewportCameraMode {
    /// Perspective 3D orbit/fly camera.
    #[default]
    #[serde(rename = "3d")]
    Mode3D,
    /// Orthographic 2D pan/zoom camera.
    #[serde(rename = "2d")]
    Mode2D,
}

/// Transform gizmo manipulation mode (`W` = Translate, `E` = Rotate, `R` = Scale).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GizmoMode {
    /// Move selected entities along axes or planes (`W`).
    #[default]
    Translate,
    /// Rotate selected entities around axes (`E`).
    Rotate,
    /// Scale selected entities along axes or uniformly (`R`).
    Scale,
}

/// Transform coordinate space used by gizmo handles (`World` vs `Local`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GizmoSpace {
    /// Gizmo axes align with global world axes (`X`, `Y`, `Z`).
    #[default]
    World,
    /// Gizmo axes align with the primary selected entity's orientation.
    Local,
}

/// Snapping increments for 3D and 2D transform gizmo operations.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GizmoSnapConfig {
    /// Whether step snapping is enabled (`Ctrl` temporarily toggles while dragging).
    pub enabled: bool,
    /// 3D translation snap step in world units.
    pub translate_step: f32,
    /// 2D translation snap step in world/pixel units.
    pub translate_step_2d: f32,
    /// Rotation snap increment in degrees.
    pub rotate_step_degrees: f32,
    /// Scale snap increment.
    pub scale_step: f32,
}

impl Default for GizmoSnapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            translate_step: 0.5,
            translate_step_2d: 16.0,
            rotate_step_degrees: 15.0,
            scale_step: 0.1,
        }
    }
}

/// Pixel format identifier stored in binary viewport frame headers.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    /// Tightly packed 32-bit `RGBA8` (`R`, `G`, `B`, `A` bytes in linear/sRGB presentation order).
    #[default]
    Rgba8UnormSrgb = 1,
}

impl PixelFormat {
    /// Returns the number of bytes per pixel for this format.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8UnormSrgb => 4,
        }
    }

    /// Converts a raw protocol byte into a [`PixelFormat`], if known.
    #[must_use]
    pub const fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Rgba8UnormSrgb),
            _ => None,
        }
    }
}

/// Fixed header prepended to every binary viewport frame packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewportFrameHeader {
    /// Wire protocol version (must match [`PROTOCOL_VERSION`]).
    pub protocol_version: u32,
    /// Monotonically increasing frame sequence counter.
    pub frame_id: u64,
    /// Width of the frame in pixels.
    pub width: u32,
    /// Height of the frame in pixels.
    pub height: u32,
    /// Pixel encoding format of the payload.
    pub format: PixelFormat,
    /// Byte length of the pixel payload following the header.
    pub payload_len: u32,
}

/// Errors produced when encoding or decoding binary viewport frame packets.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Packet buffer is smaller than [`FRAME_HEADER_BYTES`].
    #[error("viewport frame buffer too short: expected at least {FRAME_HEADER_BYTES} bytes, got {actual}")]
    BufferTooShort {
        /// Actual byte length received.
        actual: usize,
    },
    /// Packet header does not begin with [`FRAME_MAGIC`].
    #[error("invalid viewport frame magic bytes: {actual:?}")]
    InvalidMagic {
        /// Four magic bytes read from the packet.
        actual: [u8; 4],
    },
    /// Packet protocol version does not match [`PROTOCOL_VERSION`].
    #[error("unsupported viewport protocol version {actual} (expected {PROTOCOL_VERSION})")]
    UnsupportedVersion {
        /// Protocol version read from the packet.
        actual: u32,
    },
    /// Pixel format byte in the header is unknown.
    #[error("unknown viewport pixel format code {raw}")]
    UnknownPixelFormat {
        /// Raw pixel format tag.
        raw: u8,
    },
    /// Pixel slice or trailing payload size does not match `width * height * bytes_per_pixel`.
    #[error("viewport payload length mismatch: expected {expected} bytes, got {actual}")]
    PayloadLengthMismatch {
        /// Expected byte count.
        expected: usize,
        /// Actual byte count.
        actual: usize,
    },
}

/// Encodes a viewport frame header and tightly packed pixel slice into a binary packet.
///
/// Layout (28 bytes little-endian header + `pixels`):
/// - `0..4`: magic `"BV9F"`
/// - `4..8`: `protocol_version` (`u32`)
/// - `8..16`: `frame_id` (`u64`)
/// - `16..20`: `width` (`u32`)
/// - `20..24`: `height` (`u32`)
/// - `24`: `format` (`u8`)
/// - `25..28`: reserved zero bytes
/// - `28..`: raw pixel payload
pub fn encode_viewport_frame(
    header: &ViewportFrameHeader,
    pixels: &[u8],
) -> Result<Vec<u8>, ProtocolError> {
    if header.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion {
            actual: header.protocol_version,
        });
    }

    let expected_pixels = (header.width as usize)
        .checked_mul(header.height as usize)
        .and_then(|px| px.checked_mul(header.format.bytes_per_pixel()))
        .ok_or(ProtocolError::PayloadLengthMismatch {
            expected: usize::MAX,
            actual: pixels.len(),
        })?;

    if pixels.len() != expected_pixels || header.payload_len as usize != expected_pixels {
        return Err(ProtocolError::PayloadLengthMismatch {
            expected: expected_pixels,
            actual: pixels.len(),
        });
    }

    let mut out = Vec::with_capacity(FRAME_HEADER_BYTES + pixels.len());
    out.extend_from_slice(&FRAME_MAGIC);
    out.extend_from_slice(&header.protocol_version.to_le_bytes());
    out.extend_from_slice(&header.frame_id.to_le_bytes());
    out.extend_from_slice(&header.width.to_le_bytes());
    out.extend_from_slice(&header.height.to_le_bytes());
    out.push(header.format as u8);
    out.extend_from_slice(&[0u8; 3]);
    out.extend_from_slice(pixels);
    Ok(out)
}

/// Decodes a binary viewport frame packet into its [`ViewportFrameHeader`] and pixel slice.
pub fn decode_viewport_frame(bytes: &[u8]) -> Result<(ViewportFrameHeader, &[u8]), ProtocolError> {
    if bytes.len() < FRAME_HEADER_BYTES {
        return Err(ProtocolError::BufferTooShort {
            actual: bytes.len(),
        });
    }

    let magic: [u8; 4] = bytes[0..4]
        .try_into()
        .map_err(|_| ProtocolError::BufferTooShort {
            actual: bytes.len(),
        })?;
    if magic != FRAME_MAGIC {
        return Err(ProtocolError::InvalidMagic { actual: magic });
    }

    let protocol_version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion {
            actual: protocol_version,
        });
    }

    let frame_id = u64::from_le_bytes([
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    ]);
    let width = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    let format = PixelFormat::from_u8(bytes[24])
        .ok_or(ProtocolError::UnknownPixelFormat { raw: bytes[24] })?;

    let expected_pixels = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(format.bytes_per_pixel()))
        .ok_or(ProtocolError::PayloadLengthMismatch {
            expected: usize::MAX,
            actual: bytes.len() - FRAME_HEADER_BYTES,
        })?;

    let payload = &bytes[FRAME_HEADER_BYTES..];
    if payload.len() != expected_pixels {
        return Err(ProtocolError::PayloadLengthMismatch {
            expected: expected_pixels,
            actual: payload.len(),
        });
    }

    let payload_len =
        u32::try_from(payload.len()).map_err(|_| ProtocolError::PayloadLengthMismatch {
            expected: expected_pixels,
            actual: payload.len(),
        })?;

    Ok((
        ViewportFrameHeader {
            protocol_version,
            frame_id,
            width,
            height,
            format,
            payload_len,
        },
        payload,
    ))
}

/// Mouse button identifiers forwarded from the UI canvas to the Engine Host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButtonCode {
    /// Primary (left) mouse button.
    Left,
    /// Secondary (right) mouse button.
    Right,
    /// Auxiliary (middle) mouse button.
    Middle,
}

/// Keyboard modifier flags attached to forwarded viewport input events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct InputModifiers {
    /// Shift key is held.
    pub shift: bool,
    /// Control key is held.
    pub ctrl: bool,
    /// Alt / Option key is held.
    pub alt: bool,
    /// Super / Meta / Command key is held.
    pub meta: bool,
}

/// Input and resize events forwarded from the embedded `<canvas>` viewport to the runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewportInputEvent {
    /// Pointer cursor moved over the viewport canvas.
    PointerMove {
        /// Horizontal coordinate relative to canvas top-left in logical pixels.
        x: f32,
        /// Vertical coordinate relative to canvas top-left in logical pixels.
        y: f32,
        /// Horizontal delta since the previous event.
        delta_x: f32,
        /// Vertical delta since the previous event.
        delta_y: f32,
    },
    /// Mouse button pressed or released over the viewport canvas.
    PointerButton {
        /// Button identifier.
        button: PointerButtonCode,
        /// `true` on press, `false` on release.
        pressed: bool,
        /// Cursor X position at the time of the event.
        x: f32,
        /// Cursor Y position at the time of the event.
        y: f32,
        /// Active keyboard modifiers.
        modifiers: InputModifiers,
    },
    /// Mouse wheel scrolled over the viewport canvas.
    Wheel {
        /// Horizontal scroll delta.
        delta_x: f32,
        /// Vertical scroll delta (positive typically zooms in).
        delta_y: f32,
        /// Active keyboard modifiers.
        modifiers: InputModifiers,
    },
    /// Keyboard key pressed or released while the viewport is focused.
    Key {
        /// Web `KeyboardEvent.code` string (e.g. `"KeyW"`, `"Space"`, `"ShiftLeft"`).
        code: String,
        /// `true` on key down, `false` on key up.
        pressed: bool,
        /// Active keyboard modifiers.
        modifiers: InputModifiers,
    },
    /// Viewport panel changed physical render dimensions.
    Resize {
        /// Target render width in pixels.
        width: u32,
        /// Target render height in pixels.
        height: u32,
        /// Display DPI scale factor.
        scale_factor: f32,
    },
    /// Viewport canvas gained or lost keyboard focus.
    FocusChanged {
        /// `true` if the viewport currently has focus.
        focused: bool,
    },
}
