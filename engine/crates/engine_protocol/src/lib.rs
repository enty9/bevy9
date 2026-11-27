//! Shared IPC, BRP, viewport input, and binary frame wire types between editor and runtime.

pub mod assets;
pub mod ipc;
pub mod pie;
pub mod rpc;
pub mod viewport;

/// Current wire protocol version shared by `engine_editor_bridge` and the Tauri/React editor.
pub const PROTOCOL_VERSION: u32 = 1;

/// Commonly used protocol types and constants.
pub mod prelude {
    pub use crate::{
        assets::{
            AssetFolderEntry, AssetImportSettings, AssetKind, AssetRecord, AssetRegistrySnapshot,
            AtlasGridSettings, GltfImportSettings, ImageImportSettings, SubAssetInfo,
            TextureFilterMode, TilemapImportSettings,
        },
        ipc::{
            ClientIpcMessage, EditorLogEntry, FrameMetrics, HistoryStatePayload, LogLevel,
            SceneTreeNode, ServerIpcMessage,
        },
        pie::{PieState, PieStatePayload},
        rpc::{
            AddComponentParams, AssetCreateFolderParams, AssetCreateFolderResult,
            AssetDeleteParams, AssetDeleteResult, AssetImportParams, AssetImportResult,
            AssetListParams, AssetListResult, AssetMoveParams, AssetSpawnInViewportParams,
            AssetSpawnInViewportResult, AssetThumbnailParams, AssetThumbnailResult,
            AssetUpdateSettingsParams, ComponentMutationResult, DeleteEntityParams,
            DeleteEntityResult, EntityPresetKind, HistoryStepResult, PieStateResult,
            PieStepParams, PrefabApplyResult, PrefabFileEntry, PrefabInstanceActionParams,
            PrefabListResult, PrefabRevertParams, PrefabRevertResult, PrefabSaveParams,
            PrefabSaveResult, PrefabSpawnParams, PrefabSpawnResult, RemoveComponentParams,
            ReparentEntityParams, ReparentEntityResult, SceneLoadParams, SceneNewParams,
            SceneOperationResult, SceneSaveParams, SetComponentParams, SetViewportModeParams,
            SetViewportModeResult, SpawnPresetParams, SpawnPresetResult, WorldTreeResult,
            METHOD_ASSET_CREATE_FOLDER, METHOD_ASSET_DELETE, METHOD_ASSET_IMPORT,
            METHOD_ASSET_LIST, METHOD_ASSET_MOVE, METHOD_ASSET_RESCAN,
            METHOD_ASSET_SPAWN_IN_VIEWPORT, METHOD_ASSET_THUMBNAIL, METHOD_ASSET_UPDATE_SETTINGS,
            METHOD_COMPONENT_ADD, METHOD_COMPONENT_REMOVE, METHOD_COMPONENT_SET,
            METHOD_ENTITY_DELETE, METHOD_ENTITY_REPARENT, METHOD_ENTITY_SPAWN_PRESET,
            METHOD_HISTORY_REDO, METHOD_HISTORY_STATE, METHOD_HISTORY_UNDO, METHOD_PIE_PAUSE,
            METHOD_PIE_PLAY, METHOD_PIE_STATE, METHOD_PIE_STEP, METHOD_PIE_STOP,
            METHOD_PREFAB_APPLY, METHOD_PREFAB_LIST, METHOD_PREFAB_REVERT, METHOD_PREFAB_SAVE,
            METHOD_PREFAB_SPAWN, METHOD_SCENE_LOAD, METHOD_SCENE_NEW, METHOD_SCENE_SAVE,
            METHOD_VIEWPORT_SET_MODE, METHOD_WORLD_TREE,
        },
        viewport::{
            decode_viewport_frame, encode_viewport_frame, GizmoMode, GizmoSnapConfig, GizmoSpace,
            InputModifiers, PixelFormat, PointerButtonCode, ProtocolError, ViewportCameraMode,
            ViewportFrameHeader, ViewportInputEvent, ViewportMode, FRAME_HEADER_BYTES, FRAME_MAGIC,
        },
        PROTOCOL_VERSION,
    };
}

pub use prelude::*;

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::{AssetUuid, EntityGuid, WorldMode};

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
            ClientIpcMessage::SetGizmoState {
                mode: GizmoMode::Rotate,
                space: GizmoSpace::Local,
                snap: GizmoSnapConfig {
                    enabled: true,
                    translate_step: 1.0,
                    translate_step_2d: 32.0,
                    rotate_step_degrees: 30.0,
                    scale_step: 0.25,
                },
            },
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
            prefab_path: Some("prefabs/cube.prefab.ron".to_owned()),
            is_prefab_member: true,
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
            ServerIpcMessage::GizmoStateChanged {
                mode: GizmoMode::Scale,
                space: GizmoSpace::Local,
                snap: GizmoSnapConfig::default(),
            },
            ServerIpcMessage::InspectorRefresh { guids: vec![guid] },
            ServerIpcMessage::HistoryStateChanged {
                state: HistoryStatePayload {
                    can_undo: true,
                    can_redo: false,
                    undo_label: Some("Move Entity".to_owned()),
                    redo_label: None,
                    undo_len: 1,
                    redo_len: 0,
                },
            },
            ServerIpcMessage::PieStateChanged {
                state: PieStatePayload {
                    state: PieState::Playing,
                    fixed_steps: 12,
                    frame_count: 14,
                    elapsed_secs: 0.2,
                    using_scene_camera: true,
                },
            },
            ServerIpcMessage::AssetsChanged { revision: 3 },
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

        let set_comp = SetComponentParams {
            guid,
            type_path: "bevy_ecs::name::Name".to_owned(),
            value: serde_json::json!("Renamed"),
            merge_key: Some("merge-1".to_owned()),
            label: Some("Rename Entity".to_owned()),
        };
        let set_json = serde_json::to_string(&set_comp).expect("set params should serialize");
        let set_back: SetComponentParams =
            serde_json::from_str(&set_json).expect("set params should deserialize");
        assert_eq!(set_back, set_comp);

        let step_res = HistoryStepResult {
            applied: true,
            label: Some("Rename Entity".to_owned()),
            state: HistoryStatePayload {
                can_undo: false,
                can_redo: true,
                undo_label: None,
                redo_label: Some("Rename Entity".to_owned()),
                undo_len: 0,
                redo_len: 1,
            },
        };
        let step_json = serde_json::to_string(&step_res).expect("history result should serialize");
        let step_back: HistoryStepResult =
            serde_json::from_str(&step_json).expect("history result should deserialize");
        assert_eq!(step_back, step_res);

        let prefab_save = PrefabSaveParams {
            root_guid: guid,
            path: "prefabs/hero.prefab.ron".to_owned(),
            name: Some("Hero".to_owned()),
        };
        let prefab_json = serde_json::to_string(&prefab_save).expect("prefab params should serialize");
        let prefab_back: PrefabSaveParams =
            serde_json::from_str(&prefab_json).expect("prefab params should deserialize");
        assert_eq!(prefab_back, prefab_save);

        let asset_id = AssetUuid::new();
        let asset_rec = AssetRecord {
            uuid: asset_id,
            relative_path: "textures/crate.png".to_owned(),
            name: "crate.png".to_owned(),
            kind: AssetKind::Image,
            file_size: 1024,
            modified_ms: 1_700_000_000,
            source_hash: "deadbeef".to_owned(),
            tags: vec!["prop".to_owned()],
            dependencies: vec![],
            dependents: vec![],
            sub_assets: vec![],
            import_settings: AssetImportSettings::Image(ImageImportSettings {
                filter_mode: TextureFilterMode::Nearest,
                srgb: true,
                atlas_grid: Some(AtlasGridSettings {
                    tile_size: [32, 32],
                    columns: 4,
                    rows: 4,
                    padding: [0, 0],
                    offset: [0, 0],
                }),
            }),
            has_thumbnail: false,
        };
        let asset_json = serde_json::to_string(&asset_rec).expect("asset record should serialize");
        let asset_back: AssetRecord =
            serde_json::from_str(&asset_json).expect("asset record should deserialize");
        assert_eq!(asset_back, asset_rec);

        assert_eq!(
            serde_json::to_string(&ViewportCameraMode::Mode3D).unwrap(),
            "\"3d\""
        );
        assert_eq!(
            serde_json::to_string(&ViewportCameraMode::Mode2D).unwrap(),
            "\"2d\""
        );
    }

    #[test]
    fn pie_protocol_types_and_methods_roundtrip() {
        assert_eq!(
            serde_json::to_string(&PieState::Editing).unwrap(),
            "\"editing\""
        );
        assert_eq!(
            serde_json::to_string(&PieState::Playing).unwrap(),
            "\"playing\""
        );
        assert_eq!(
            serde_json::to_string(&PieState::Paused).unwrap(),
            "\"paused\""
        );
        assert!(PieState::Editing.is_editing());
        assert!(!PieState::Editing.is_active());
        assert!(PieState::Playing.is_active() && PieState::Playing.is_playing());
        assert!(PieState::Paused.is_active() && PieState::Paused.is_paused());

        let default_step: PieStepParams = serde_json::from_str("{}").unwrap();
        assert_eq!(default_step, PieStepParams { steps: 1 });
        let custom_step: PieStepParams = serde_json::from_str(r#"{"steps":4}"#).unwrap();
        assert_eq!(custom_step, PieStepParams { steps: 4 });

        let payload = PieStatePayload {
            state: PieState::Paused,
            fixed_steps: 7,
            frame_count: 9,
            elapsed_secs: 0.125,
            using_scene_camera: false,
        };
        let encoded = serde_json::to_string(&payload).unwrap();
        let decoded: PieStateResult = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, payload);

        assert_eq!(METHOD_PIE_PLAY, "engine.pie.play");
        assert_eq!(METHOD_PIE_PAUSE, "engine.pie.pause");
        assert_eq!(METHOD_PIE_STEP, "engine.pie.step");
        assert_eq!(METHOD_PIE_STOP, "engine.pie.stop");
        assert_eq!(METHOD_PIE_STATE, "engine.pie.state");
    }
}
