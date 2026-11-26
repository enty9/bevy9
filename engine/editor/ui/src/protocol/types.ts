export const PROTOCOL_VERSION = 1;
export const FRAME_MAGIC = "BV9F";
export const FRAME_HEADER_BYTES = 28;

export type WorldMode = "2d" | "3d" | "hybrid";
export type ViewportMode = "offscreen" | "native_window";
export type ViewportCameraMode = "3d" | "2d";
export type GizmoMode = "translate" | "rotate" | "scale";
export type GizmoSpace = "world" | "local";

export interface GizmoSnapConfig {
  enabled: boolean;
  translate_step: number;
  translate_step_2d: number;
  rotate_step_degrees: number;
  scale_step: number;
}

export interface GizmoStatePayload {
  mode: GizmoMode;
  space: GizmoSpace;
  snap: GizmoSnapConfig;
}

export type EntityPresetKind =
  | "empty"
  | "cube3d"
  | "sphere3d"
  | "plane3d"
  | "directional_light"
  | "point_light"
  | "camera3d"
  | "sprite2d"
  | "quad2d"
  | "circle2d"
  | "camera2d";

export type LogLevel = "trace" | "debug" | "info" | "warn" | "error";

export interface EditorLogEntry {
  level: LogLevel;
  target: string;
  message: string;
}

export interface FrameMetrics {
  fps: number;
  frame_time_ms: number;
  entity_count: number;
  scene_entity_count: number;
}

export interface SceneTreeNode {
  guid: string;
  entity_bits: number;
  name: string;
  parent_guid: string | null;
  visible: boolean;
  children: SceneTreeNode[];
  components: string[];
  prefab_path?: string | null;
  is_prefab_member?: boolean;
}

export type PointerButtonCode = "left" | "right" | "middle";

export interface InputModifiers {
  shift: boolean;
  ctrl: boolean;
  alt: boolean;
  meta: boolean;
}

export type ViewportInputEvent =
  | {
      kind: "pointer_move";
      x: number;
      y: number;
      delta_x: number;
      delta_y: number;
    }
  | {
      kind: "pointer_button";
      button: PointerButtonCode;
      pressed: boolean;
      x: number;
      y: number;
      modifiers: InputModifiers;
    }
  | {
      kind: "wheel";
      delta_x: number;
      delta_y: number;
      modifiers: InputModifiers;
    }
  | {
      kind: "key";
      code: string;
      pressed: boolean;
      modifiers: InputModifiers;
    }
  | {
      kind: "resize";
      width: number;
      height: number;
      scale_factor: number;
    }
  | {
      kind: "focus_changed";
      focused: boolean;
    };

export type ClientIpcMessage =
  | {
      type: "hello";
      protocol_version: number;
      client_name: string;
    }
  | {
      type: "viewport_input";
      event: ViewportInputEvent;
    }
  | {
      type: "set_viewport_mode";
      mode: ViewportMode;
    }
  | {
      type: "set_camera_mode";
      camera_mode: ViewportCameraMode;
    }
  | {
      type: "select_entities";
      guids: string[];
    }
  | {
      type: "set_gizmo_state";
      mode: GizmoMode;
      space: GizmoSpace;
      snap: GizmoSnapConfig;
    }
  | {
      type: "ping";
      nonce: number;
    };

export interface WelcomePayload {
  protocol_version: number;
  project_name: string;
  world_mode: WorldMode;
  scene_name: string;
  scene_path: string | null;
  viewport_mode: ViewportMode;
  camera_mode: ViewportCameraMode;
}

export interface ViewportStatePayload {
  viewport_mode: ViewportMode;
  camera_mode: ViewportCameraMode;
  width: number;
  height: number;
}

export interface HistoryStatePayload {
  can_undo: boolean;
  can_redo: boolean;
  undo_label: string | null;
  redo_label: string | null;
  undo_len: number;
  redo_len: number;
}

export interface HistoryStepResult {
  applied: boolean;
  label: string | null;
  state: HistoryStatePayload;
}

export type ServerIpcMessage =
  | ({ type: "welcome" } & WelcomePayload)
  | { type: "hierarchy_snapshot"; roots: SceneTreeNode[] }
  | { type: "selection_changed"; guids: string[] }
  | ({ type: "viewport_state_changed" } & ViewportStatePayload)
  | ({ type: "gizmo_state_changed" } & GizmoStatePayload)
  | { type: "inspector_refresh"; guids: string[] }
  | { type: "history_state_changed"; state: HistoryStatePayload }
  | { type: "log"; entry: EditorLogEntry }
  | { type: "metrics"; metrics: FrameMetrics }
  | { type: "pong"; nonce: number }
  | { type: "error"; message: string };

export interface ViewportFrameHeader {
  protocol_version: number;
  frame_id: number;
  width: number;
  height: number;
  format: number;
  payload_len: number;
}

export interface DecodedViewportFrame {
  header: ViewportFrameHeader;
  pixels: Uint8ClampedArray<ArrayBuffer>;
}

export interface SceneNewParams {
  name: string;
  world_mode: WorldMode;
}

export interface SceneLoadParams {
  path: string;
}

export interface SceneSaveParams {
  path?: string | null;
  name?: string | null;
}

export interface SceneOperationResult {
  name: string;
  path: string | null;
  world_mode: WorldMode;
  entity_count: number;
}

export interface WorldTreeResult {
  scene_name: string;
  scene_path: string | null;
  world_mode: WorldMode;
  roots: SceneTreeNode[];
}

export interface SpawnPresetParams {
  preset: EntityPresetKind;
  name?: string | null;
  parent_guid?: string | null;
  position?: [number, number, number] | null;
}

export interface SpawnPresetResult {
  guid: string;
  entity_bits: number;
  name: string;
}

export interface SetViewportModeResult {
  mode: ViewportMode;
}

export interface DeleteEntityResult {
  deleted_guids: string[];
}

export interface ReparentEntityResult {
  guid: string;
  parent_guid: string | null;
}

export interface ComponentMutationResult {
  guid: string;
  type_path: string;
}

export interface PrefabInstanceData {
  prefab_path: string;
  root_source_guid: string;
  overrides: Record<string, string>;
}

export interface PrefabMemberData {
  instance_root_guid: string;
  source_guid: string;
}

export interface PrefabSaveParams {
  root_guid: string;
  path: string;
  name?: string | null;
}

export interface PrefabSaveResult {
  prefab_path: string;
  root_source_guid: string;
  entity_count: number;
}

export interface PrefabSpawnParams {
  path: string;
  parent_guid?: string | null;
  position?: [number, number, number] | null;
}

export interface PrefabSpawnResult {
  guid: string;
  entity_bits: number;
  name: string;
  prefab_path: string;
}

export interface PrefabApplyResult {
  prefab_path: string;
  updated_guids: string[];
}

export interface PrefabRevertResult {
  prefab_path: string;
  reverted_guids: string[];
}

export interface PrefabFileEntry {
  name: string;
  relative_path: string;
  absolute_path: string;
}

export interface PrefabListResult {
  prefabs: PrefabFileEntry[];
}

export interface BrpSchemaRef {
  type?: {
    $ref?: string;
  };
  $ref?: string;
}

export interface BrpSchemaComponentInfo {
  mutable?: boolean;
  storageType?: string;
  isSendAndSync?: boolean;
  requiredComponentTypes?: string[];
  relationshipKind?: string;
}

export interface BrpSchemaType {
  type?: string | string[];
  kind?: string;
  shortPath?: string;
  typePath?: string;
  crateName?: string;
  modulePath?: string;
  reflectTypes?: string[];
  componentInfo?: BrpSchemaComponentInfo;
  properties?: Record<string, BrpSchemaRef>;
  required?: string[];
  prefixItems?: BrpSchemaRef[];
  items?: BrpSchemaRef | boolean;
  oneOf?: Array<string | BrpSchemaType>;
  additionalProperties?: boolean | BrpSchemaRef;
}

export type BrpRegistrySchema = Record<string, BrpSchemaType>;

export interface BrpGetComponentsResponse {
  components: Record<string, unknown>;
  errors?: Record<string, { code: number; message: string }>;
}
