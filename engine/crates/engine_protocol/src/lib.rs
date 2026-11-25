//! Shared IPC, BRP, viewport input, and binary frame wire types between editor and runtime.

pub mod ipc;
pub mod rpc;
pub mod viewport;

/// Current wire protocol version shared by `engine_editor_bridge` and the Tauri/React editor.
pub const PROTOCOL_VERSION: u32 = 1;

/// Commonly used protocol types and constants.
pub mod prelude {
    pub use crate::{
        ipc::{
            ClientIpcMessage, EditorLogEntry, FrameMetrics, LogLevel, SceneTreeNode,
            ServerIpcMessage,
        },
        rpc::{
            EntityPresetKind, SceneLoadParams, SceneNewParams, SceneOperationResult,
            SceneSaveParams, SetViewportModeParams, SetViewportModeResult, SpawnPresetParams,
            SpawnPresetResult, WorldTreeResult, METHOD_ENTITY_SPAWN_PRESET, METHOD_SCENE_LOAD,
            METHOD_SCENE_NEW, METHOD_SCENE_SAVE, METHOD_VIEWPORT_SET_MODE, METHOD_WORLD_TREE,
        },
        viewport::{
            decode_viewport_frame, encode_viewport_frame, InputModifiers, PixelFormat,
            PointerButtonCode, ProtocolError, ViewportCameraMode, ViewportFrameHeader,
            ViewportInputEvent, ViewportMode, FRAME_HEADER_BYTES, FRAME_MAGIC,
        },
        PROTOCOL_VERSION,
    };
}

pub use prelude::*;

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::{EntityGuid, WorldMode};

    #[test]
    fn viewport_frame_roundtrip_and_error_cases() {
        let width = 4;
        let height = 3;
        let pixels: Vec<u8> = (0..(width * height * 4)).map(|i| (i % 251) as u8).collect();
        let header = ViewportFrameHeader {
            protocol_version: PROTOCOL_VERSION,
            frame_id: 42,
            width,
            height,
            format: PixelFormat::Rgba8UnormSrgb,
            payload_len: pixels.len() as u32,
        };

        let encoded = encode_viewport_frame(&header, &pixels).expect("valid frame should encode");
        assert_eq!(encoded.len(), FRAME_HEADER_BYTES + pixels.len());

        let (decoded_header, decoded_pixels) =
            decode_viewport_frame(&encoded).expect("valid frame should decode");
        assert_eq!(decoded_header, header);
        assert_eq!(decoded_pixels, pixels.as_slice());

        // Short buffer
        assert_eq!(
            decode_viewport_frame(&encoded[..10]),
            Err(ProtocolError::BufferTooShort { actual: 10 })
        );

        // Bad magic
        let mut bad_magic = encoded.clone();
        bad_magic[0] = b'X';
        assert_eq!(
            decode_viewport_frame(&bad_magic),
            Err(ProtocolError::InvalidMagic { actual: *b"XV9F" })
        );

        // Unsupported version
        let mut bad_version = encoded.clone();
        bad_version[4..8].copy_from_slice(&99u32.to_le_bytes());
        assert_eq!(
            decode_viewport_frame(&bad_version),
            Err(ProtocolError::UnsupportedVersion { actual: 99 })
        );

        // Unknown pixel format
        let mut bad_format = encoded.clone();
        bad_format[24] = 9;
        assert_eq!(
            decode_viewport_frame(&bad_format),
            Err(ProtocolError::UnknownPixelFormat { raw: 9 })
        );

        // Truncated payload
        let truncated = &encoded[..encoded.len() - 4];
        assert_eq!(
            decode_viewport_frame(truncated),
            Err(ProtocolError::PayloadLengthMismatch {
                expected: pixels.len(),
                actual: pixels.len() - 4,
            })
        );
    }

    #[test]
    fn ipc_and_rpc_messages_roundtrip_json() {
        let guid = EntityGuid::new();
        let client_msgs = [
            ClientIpcMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                client_name: "editor-ui".to_owned(),
            },
            ClientIpcMessage::ViewportInput {
                event: ViewportInputEvent::PointerButton {
                    button: PointerButtonCode::Right,
                    pressed: true,
                    x: 120.5,
                    y: 240.0,
                    modifiers: InputModifiers {
                        shift: true,
                        ctrl: false,
                        alt: false,
                        meta: false,
                    },
                },
            },
            ClientIpcMessage::SetViewportMode {
                mode: ViewportMode::NativeWindow,
            },
            ClientIpcMessage::SetCameraMode {
                camera_mode: ViewportCameraMode::Mode2D,
            },
            ClientIpcMessage::SelectEntities { guids: vec![guid] },
            ClientIpcMessage::Ping { nonce: 777 },
        ];

        for msg in &client_msgs {
            let json = serde_json::to_string(msg).expect("client message should serialize");
            let back: ClientIpcMessage =
                serde_json::from_str(&json).expect("client message should deserialize");
            assert_eq!(&back, msg);
        }

        let tree_node = SceneTreeNode {
            guid,
            entity_bits: 1234,
            name: "Cube".to_owned(),
            parent_guid: None,
            visible: true,
            children: vec![],
            components: vec!["engine_scene::components::MeshPrimitive3d".to_owned()],
        };

        let server_msgs = [
            ServerIpcMessage::Welcome {
                protocol_version: PROTOCOL_VERSION,
                project_name: "Sandbox".to_owned(),
                world_mode: WorldMode::Hybrid,
                scene_name: "Main".to_owned(),
                scene_path: Some("scenes/main.level.ron".to_owned()),
                viewport_mode: ViewportMode::Offscreen,
                camera_mode: ViewportCameraMode::Mode3D,
            },
            ServerIpcMessage::HierarchySnapshot {
                roots: vec![tree_node.clone()],
            },
            ServerIpcMessage::SelectionChanged { guids: vec![guid] },
            ServerIpcMessage::ViewportStateChanged {
                viewport_mode: ViewportMode::Offscreen,
                camera_mode: ViewportCameraMode::Mode3D,
                width: 1280,
                height: 720,
            },
            ServerIpcMessage::Log {
                entry: EditorLogEntry {
                    level: LogLevel::Info,
                    target: "engine".to_owned(),
                    message: "ready".to_owned(),
                },
            },
            ServerIpcMessage::Metrics {
                metrics: FrameMetrics {
                    fps: 60.0,
                    frame_time_ms: 16.6,
                    entity_count: 10,
                    scene_entity_count: 4,
                },
            },
            ServerIpcMessage::Pong { nonce: 777 },
            ServerIpcMessage::Error {
                message: "bad request".to_owned(),
            },
        ];

        for msg in &server_msgs {
            let json = serde_json::to_string(msg).expect("server message should serialize");
            let back: ServerIpcMessage =
                serde_json::from_str(&json).expect("server message should deserialize");
            assert_eq!(&back, msg);
        }

        let rpc_req = SpawnPresetParams {
            preset: EntityPresetKind::Cube3d,
            name: Some("Hero".to_owned()),
            parent_guid: Some(guid),
            position: Some([1.0, 2.0, 3.0]),
        };
        let rpc_json = serde_json::to_string(&rpc_req).expect("rpc params should serialize");
        let rpc_back: SpawnPresetParams =
            serde_json::from_str(&rpc_json).expect("rpc params should deserialize");
        assert_eq!(rpc_back, rpc_req);

        let tree_res = WorldTreeResult {
            scene_name: "Main".to_owned(),
            scene_path: None,
            world_mode: WorldMode::Mode3D,
            roots: vec![tree_node],
        };
        let tree_json = serde_json::to_string(&tree_res).expect("tree result should serialize");
        let tree_back: WorldTreeResult =
            serde_json::from_str(&tree_json).expect("tree result should deserialize");
        assert_eq!(tree_back, tree_res);
    }
}
