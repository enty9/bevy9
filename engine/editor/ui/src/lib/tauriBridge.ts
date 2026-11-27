import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";

export interface SceneFileEntry {
  name: string;
  relative_path: string;
  absolute_path: string;
}

export interface ProjectSummary {
  name: string;
  id: string;
  version: string;
  default_mode: string;
  startup_scene: string;
  root_dir: string;
  manifest_path: string;
  assets_dir: string;
  scenes_dir: string;
  scripts_dir: string;
  scenes: SceneFileEntry[];
}

export type EngineHostState = "stopped" | "starting" | "running" | "crashed";

export interface EngineHostStatus {
  state: EngineHostState;
  project_dir: string | null;
  project_name: string | null;
  brp_url: string | null;
  ipc_url: string | null;
  brp_port: number | null;
  ipc_port: number | null;
  pid: number | null;
  last_error: string | null;
}

export interface EngineLogMessage {
  timestamp_ms: number;
  level: string;
  target: string;
  message: string;
}

export const DEFAULT_ENGINE_STATUS: EngineHostStatus = {
  state: "stopped",
  project_dir: null,
  project_name: null,
  brp_url: null,
  ipc_url: null,
  brp_port: null,
  ipc_port: null,
  pid: null,
  last_error: null,
};

export function isRunningInTauri(): boolean {
  return isTauri();
}

let openNativeDialogs = 0;
const nativeDialogListeners = new Set<(open: boolean) => void>();

function notifyNativeDialogListeners(): void {
  const isOpen = openNativeDialogs > 0;
  for (const listener of nativeDialogListeners) {
    listener(isOpen);
  }
}

export function subscribeNativeDialogOpen(
  listener: (open: boolean) => void,
): () => void {
  nativeDialogListeners.add(listener);
  listener(openNativeDialogs > 0);
  return () => {
    nativeDialogListeners.delete(listener);
  };
}

async function withNativeDialog<T>(task: () => Promise<T>): Promise<T> {
  openNativeDialogs += 1;
  if (openNativeDialogs === 1) {
    notifyNativeDialogListeners();
  }
  try {
    return await task();
  } finally {
    openNativeDialogs = Math.max(0, openNativeDialogs - 1);
    if (openNativeDialogs === 0) {
      notifyNativeDialogListeners();
    }
  }
}

export async function listSampleProjects(): Promise<ProjectSummary[]> {
  if (!isTauri()) {
    return [];
  }
  return invoke<ProjectSummary[]>("list_sample_projects");
}

export async function loadProjectMetadata(path: string): Promise<ProjectSummary> {
  return invoke<ProjectSummary>("load_project_metadata", { path });
}

export async function getEngineStatus(): Promise<EngineHostStatus> {
  if (!isTauri()) {
    return DEFAULT_ENGINE_STATUS;
  }
  return invoke<EngineHostStatus>("get_engine_status");
}

export async function getEngineLogs(): Promise<EngineLogMessage[]> {
  if (!isTauri()) {
    return [];
  }
  return invoke<EngineLogMessage[]>("get_engine_logs");
}

export async function clearEngineLogs(): Promise<void> {
  if (!isTauri()) {
    return;
  }
  await invoke<void>("clear_engine_logs");
}

export async function startEngineHost(projectPath: string): Promise<EngineHostStatus> {
  if (!isTauri()) {
    return {
      ...DEFAULT_ENGINE_STATUS,
      state: "running",
      project_dir: projectPath,
      brp_url: "http://127.0.0.1:15702",
      ipc_url: "ws://127.0.0.1:15704",
      brp_port: 15702,
      ipc_port: 15704,
    };
  }
  return invoke<EngineHostStatus>("start_engine_host", { projectPath });
}

export async function stopEngineHost(): Promise<EngineHostStatus> {
  if (!isTauri()) {
    return DEFAULT_ENGINE_STATUS;
  }
  return invoke<EngineHostStatus>("stop_engine_host");
}

export async function restartEngineHost(): Promise<EngineHostStatus> {
  if (!isTauri()) {
    return DEFAULT_ENGINE_STATUS;
  }
  return invoke<EngineHostStatus>("restart_engine_host");
}

export async function subscribeEngineStatus(
  handler: (status: EngineHostStatus) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) {
    return () => {};
  }
  return listen<EngineHostStatus>("engine-status", (event) => {
    handler(event.payload);
  });
}

export async function subscribeEngineLogs(
  handler: (entry: EngineLogMessage) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) {
    return () => {};
  }
  return listen<EngineLogMessage>("engine-log", (event) => {
    handler(event.payload);
  });
}

export async function pickProjectFolder(): Promise<string | null> {
  if (!isTauri()) {
    return null;
  }
  const selected = await withNativeDialog(() =>
    open({
      directory: true,
      multiple: false,
      title: "Select Bevy9 Project Directory",
    }),
  );
  return typeof selected === "string" ? selected : null;
}

export async function pickOpenSceneFile(defaultPath?: string): Promise<string | null> {
  if (!isTauri()) {
    return null;
  }
  const selected = await withNativeDialog(() =>
    open({
      multiple: false,
      defaultPath,
      title: "Open Scene (.level.ron)",
      filters: [{ name: "Bevy9 Scene", extensions: ["ron"] }],
    }),
  );
  return typeof selected === "string" ? selected : null;
}

export async function pickSaveSceneFile(defaultPath?: string): Promise<string | null> {
  if (!isTauri()) {
    return null;
  }
  const selected = await withNativeDialog(() =>
    save({
      defaultPath,
      title: "Save Scene (.level.ron)",
      filters: [{ name: "Bevy9 Scene", extensions: ["ron"] }],
    }),
  );
  return typeof selected === "string" ? selected : null;
}

export async function pickSavePrefabFile(defaultPath?: string): Promise<string | null> {
  if (!isTauri()) {
    return null;
  }
  const selected = await withNativeDialog(() =>
    save({
      defaultPath,
      title: "Save Prefab (.prefab.ron)",
      filters: [{ name: "Bevy9 Prefab", extensions: ["ron"] }],
    }),
  );
  return typeof selected === "string" ? selected : null;
}

export async function pickOpenPrefabFile(defaultPath?: string): Promise<string | null> {
  if (!isTauri()) {
    return null;
  }
  const selected = await withNativeDialog(() =>
    open({
      multiple: false,
      defaultPath,
      title: "Instantiate Prefab (.prefab.ron)",
      filters: [{ name: "Bevy9 Prefab", extensions: ["ron"] }],
    }),
  );
  return typeof selected === "string" ? selected : null;
}

export async function pickImportAssetFiles(): Promise<string[]> {
  if (!isTauri()) {
    return [];
  }
  const selected = await withNativeDialog(() =>
    open({
      multiple: true,
      title: "Import Assets",
      filters: [
        {
          name: "Supported Assets",
          extensions: ["png", "bmp", "tga", "glb", "gltf", "ron", "rhai", "wav", "ogg", "mp3", "flac"],
        },
      ],
    }),
  );
  if (Array.isArray(selected)) {
    return selected.filter((p): p is string => typeof p === "string");
  }
  return typeof selected === "string" ? [selected] : [];
}

export async function subscribeOsFileDrop(
  handler: (paths: string[]) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) {
    return () => {};
  }
  return getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === "drop" && event.payload.paths.length > 0) {
      handler(event.payload.paths);
    }
  });
}
