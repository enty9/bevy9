import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  clearEngineLogs,
  DEFAULT_ENGINE_STATUS,
  getEngineLogs,
  getEngineStatus,
  isRunningInTauri,
  listSampleProjects,
  loadProjectMetadata,
  pickOpenPrefabFile,
  pickOpenSceneFile,
  pickProjectFolder,
  pickSavePrefabFile,
  pickSaveSceneFile,
  restartEngineHost,
  startEngineHost,
  subscribeEngineLogs,
  subscribeEngineStatus,
  subscribeOsFileDrop,
  type EngineHostStatus,
  type EngineLogMessage,
  type ProjectSummary,
} from "./lib/tauriBridge";
import { BrpClient } from "./protocol/brpClient";
import { EditorIpcClient, type IpcConnectionState } from "./protocol/ipcClient";
import type {
  AssetRecord,
  AssetRegistrySnapshot,
  BrpRegistrySchema,
  DecodedViewportFrame,
  EntityPresetKind,
  FrameMetrics,
  GizmoStatePayload,
  HistoryStatePayload,
  PrefabFileEntry,
  SceneTreeNode,
  ViewportCameraMode,
  ViewportInputEvent,
  ViewportStatePayload,
  WorldMode,
} from "./protocol/types";
import { ConsolePanel } from "./components/ConsolePanel";
import { ContentBrowserPanel } from "./components/ContentBrowserPanel";
import {
  DEFAULT_LAYOUT_STATE,
  DockLayout,
  loadSavedLayout,
  persistLayout,
  type BottomDockTab,
  type DockLayoutState,
} from "./components/DockLayout";
import { InspectorPanel } from "./components/InspectorPanel";
import { OutlinerPanel } from "./components/OutlinerPanel";
import { Toolbar } from "./components/Toolbar";
import { ViewportPanel } from "./components/ViewportPanel";

const DEFAULT_METRICS: FrameMetrics = {
  fps: 0,
  frame_time_ms: 0,
  entity_count: 0,
  scene_entity_count: 0,
};

const DEFAULT_VIEWPORT_STATE: ViewportStatePayload = {
  viewport_mode: "offscreen",
  camera_mode: "3d",
  width: 1280,
  height: 720,
};

const DEFAULT_GIZMO_STATE: GizmoStatePayload = {
  mode: "translate",
  space: "world",
  snap: {
    enabled: false,
    translate_step: 0.5,
    translate_step_2d: 16,
    rotate_step_degrees: 15,
    scale_step: 0.1,
  },
};

const DEFAULT_HISTORY_STATE: HistoryStatePayload = {
  can_undo: false,
  can_redo: false,
  undo_label: null,
  redo_label: null,
  undo_len: 0,
  redo_len: 0,
};

function findNodeByGuid(
  nodes: SceneTreeNode[],
  guid: string,
): SceneTreeNode | null {
  for (const node of nodes) {
    if (node.guid === guid) {
      return node;
    }
    const found = findNodeByGuid(node.children, guid);
    if (found) {
      return found;
    }
  }
  return null;
}

function findPrefabRootForGuid(
  nodes: SceneTreeNode[],
  guid: string,
  nearestRoot: SceneTreeNode | null = null,
): SceneTreeNode | null {
  for (const node of nodes) {
    const activeRoot = node.prefab_path ? node : nearestRoot;
    if (node.guid === guid) {
      return activeRoot;
    }
    const found = findPrefabRootForGuid(node.children, guid, activeRoot);
    if (found !== undefined && found !== null) {
      return found;
    }
  }
  return null;
}

export type FrameSubscriber = (frame: DecodedViewportFrame) => void;

export function App() {
  const [samples, setSamples] = useState<ProjectSummary[]>([]);
  const [activeProject, setActiveProject] = useState<ProjectSummary | null>(null);
  const [hostStatus, setHostStatus] = useState<EngineHostStatus>(DEFAULT_ENGINE_STATUS);
  const [ipcState, setIpcState] = useState<IpcConnectionState>("disconnected");
  const [layout, setLayout] = useState<DockLayoutState>(() => loadSavedLayout());

  const [registrySchema, setRegistrySchema] = useState<BrpRegistrySchema | null>(null);
  const [hierarchy, setHierarchy] = useState<SceneTreeNode[]>([]);
  const [prefabs, setPrefabs] = useState<PrefabFileEntry[]>([]);
  const [assetSnapshot, setAssetSnapshot] = useState<AssetRegistrySnapshot | null>(null);
  const [activeAssetFolder, setActiveAssetFolder] = useState("");
  const [activeBottomTab, setActiveBottomTab] = useState<BottomDockTab>("content");
  const [selection, setSelection] = useState<string[]>([]);
  const [viewportState, setViewportState] = useState<ViewportStatePayload>(
    DEFAULT_VIEWPORT_STATE,
  );
  const [gizmoState, setGizmoState] = useState<GizmoStatePayload>(
    DEFAULT_GIZMO_STATE,
  );
  const [historyState, setHistoryState] = useState<HistoryStatePayload>(
    DEFAULT_HISTORY_STATE,
  );
  const [inspectorRevision, setInspectorRevision] = useState(0);
  const [metrics, setMetrics] = useState<FrameMetrics>(DEFAULT_METRICS);
  const [sceneName, setSceneName] = useState("Untitled Scene");
  const [scenePath, setScenePath] = useState<string | null>(null);
  const [sceneDirty, setSceneDirty] = useState(false);

  const [logs, setLogs] = useState<EngineLogMessage[]>([]);
  const [busy, setBusy] = useState(false);
  const [bannerError, setBannerError] = useState<string | null>(null);

  const frameListenersRef = useRef(new Set<FrameSubscriber>());
  const autoStartedRef = useRef(false);

  const subscribeFrames = useCallback((listener: FrameSubscriber): (() => void) => {
    frameListenersRef.current.add(listener);
    return () => {
      frameListenersRef.current.delete(listener);
    };
  }, []);

  const appendLog = useCallback((entry: EngineLogMessage) => {
    setLogs((prev) => [...prev.slice(-499), entry]);
  }, []);

  function updateLayout(next: DockLayoutState) {
    setLayout(next);
    persistLayout(next);
  }

  useEffect(() => {
    let active = true;
    const unlisteners: Array<() => void> = [];

    async function bootstrap() {
      const [foundSamples, initialStatus, initialLogs] = await Promise.all([
        listSampleProjects(),
        getEngineStatus(),
        getEngineLogs(),
      ]);

      if (!active) {
        return;
      }

      setSamples(foundSamples);
      setHostStatus(initialStatus);
      setLogs(initialLogs);

      if (initialStatus.project_dir) {
        const matched = foundSamples.find(
          (item) => item.root_dir === initialStatus.project_dir,
        );
        if (matched) {
          setActiveProject(matched);
        } else {
          try {
            const loaded = await loadProjectMetadata(initialStatus.project_dir);
            if (active) {
              setActiveProject(loaded);
            }
          } catch {
            // Keep existing status.
          }
        }
      } else if (foundSamples.length > 0) {
        setActiveProject(foundSamples[0]);
      }

      unlisteners.push(
        await subscribeEngineStatus((next) => {
          if (active) {
            setHostStatus(next);
          }
        }),
      );
      unlisteners.push(
        await subscribeEngineLogs((entry) => {
          if (active) {
            appendLog(entry);
          }
        }),
      );

      if (
        isRunningInTauri() &&
        !autoStartedRef.current &&
        initialStatus.state === "stopped" &&
        foundSamples.length > 0
      ) {
        autoStartedRef.current = true;
        const starter = foundSamples[0];
        setBusy(true);
        try {
          const started = await startEngineHost(starter.root_dir);
          if (active) {
            setHostStatus(started);
          }
        } catch (err) {
          if (active) {
            setBannerError(err instanceof Error ? err.message : String(err));
          }
        } finally {
          if (active) {
            setBusy(false);
          }
        }
      }
    }

    void bootstrap();

    return () => {
      active = false;
      for (const unlisten of unlisteners) {
        unlisten();
      }
    };
  }, [appendLog]);

  const effectiveBrpUrl = useMemo(() => {
    if (hostStatus.state === "running" && hostStatus.brp_url) {
      return hostStatus.brp_url;
    }
    if (!isRunningInTauri()) {
      return "http://127.0.0.1:15702";
    }
    return null;
  }, [hostStatus.state, hostStatus.brp_url]);

  const effectiveIpcUrl = useMemo(() => {
    if (hostStatus.state === "running" && hostStatus.ipc_url) {
      return hostStatus.ipc_url;
    }
    if (!isRunningInTauri()) {
      return "ws://127.0.0.1:15704";
    }
    return null;
  }, [hostStatus.state, hostStatus.ipc_url]);

  const brpClient = useMemo(
    () => (effectiveBrpUrl ? new BrpClient(effectiveBrpUrl) : null),
    [effectiveBrpUrl],
  );

  const ipcClient = useMemo(
    () => (effectiveIpcUrl ? new EditorIpcClient(effectiveIpcUrl) : null),
    [effectiveIpcUrl],
  );

  useEffect(() => {
    if (!ipcClient) {
      setIpcState("disconnected");
      return;
    }

    const unsubs = [
      ipcClient.onConnectionState(setIpcState),
      ipcClient.onWelcome((welcome) => {
        setSceneName(welcome.scene_name);
        setScenePath(welcome.scene_path);
        setSceneDirty(false);
        setViewportState((prev) => ({
          ...prev,
          viewport_mode: welcome.viewport_mode,
          camera_mode: welcome.camera_mode,
        }));
      }),
      ipcClient.onHierarchy(setHierarchy),
      ipcClient.onSelection(setSelection),
      ipcClient.onViewportState(setViewportState),
      ipcClient.onGizmoState(setGizmoState),
      ipcClient.onHistoryState(setHistoryState),
      ipcClient.onInspectorRefresh(() => {
        setSceneDirty(true);
        setInspectorRevision((rev) => rev + 1);
      }),
      ipcClient.onMetrics(setMetrics),
      ipcClient.onLog((entry) => {
        appendLog({
          timestamp_ms: Date.now(),
          level: entry.level,
          target: entry.target,
          message: entry.message,
        });
      }),
      ipcClient.onError((message) => {
        setBannerError(message);
        appendLog({
          timestamp_ms: Date.now(),
          level: "error",
          target: "ipc",
          message,
        });
      }),
      ipcClient.onFrame((frame) => {
        for (const listener of frameListenersRef.current) {
          listener(frame);
        }
      }),
    ];

    ipcClient.connect();

    return () => {
      for (const unsub of unsubs) {
        unsub();
      }
      ipcClient.disconnect();
    };
  }, [ipcClient, appendLog]);

  useEffect(() => {
    if (!brpClient || ipcState !== "connected") {
      return;
    }

    let cancelled = false;
    async function syncSchemaAndTree() {
      if (!brpClient) {
        return;
      }
      try {
        const [schema, tree, prefabList, assetsSnap] = await Promise.all([
          brpClient.getRegistrySchema(),
          brpClient.getWorldTree(),
          brpClient.listPrefabs(),
          brpClient.listAssets(),
        ]);
        if (cancelled) {
          return;
        }
        setRegistrySchema(schema);
        setHierarchy(tree.roots);
        setPrefabs(prefabList.prefabs);
        setAssetSnapshot(assetsSnap);
        setSceneName(tree.scene_name);
        setScenePath(tree.scene_path);
      } catch (err) {
        if (!cancelled) {
          appendLog({
            timestamp_ms: Date.now(),
            level: "warn",
            target: "brp",
            message: err instanceof Error ? err.message : String(err),
          });
        }
      }
    }

    void syncSchemaAndTree();

    return () => {
      cancelled = true;
    };
  }, [brpClient, ipcState, appendLog]);

  const refreshAssets = useCallback(async () => {
    if (!brpClient) {
      return;
    }
    try {
      const snap = await brpClient.listAssets();
      setAssetSnapshot(snap);
    } catch {
      // Ignored; next IPC welcome or reconnect syncs state.
    }
  }, [brpClient]);

  useEffect(() => {
    if (!ipcClient || !brpClient) {
      return;
    }
    return ipcClient.onAssetsChanged(() => {
      void refreshAssets();
    });
  }, [ipcClient, brpClient, refreshAssets]);

  const handleImportAssetPaths = useCallback(
    async (paths: string[], targetFolder: string) => {
      if (!brpClient || paths.length === 0) {
        return;
      }
      try {
        const res = await brpClient.importAssets(paths, targetFolder);
        await refreshAssets();
        appendLog({
          timestamp_ms: Date.now(),
          level: "info",
          target: "assets",
          message: `Imported ${res.imported.length} asset(s) into assets/${targetFolder}`,
        });
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient, refreshAssets, appendLog],
  );

  useEffect(() => {
    if (!brpClient || ipcState !== "connected") {
      return;
    }
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void subscribeOsFileDrop((paths) => {
      void handleImportAssetPaths(paths, activeAssetFolder);
    }).then((fn) => {
      if (cancelled) {
        fn();
      } else {
        unlisten = fn;
      }
    });
    return () => {
      cancelled = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, [brpClient, ipcState, activeAssetFolder, handleImportAssetPaths]);

  const handleSpawnAssetInViewport = useCallback(
    async (asset: AssetRecord, vx?: number, vy?: number) => {
      if (!brpClient) {
        return;
      }
      try {
        const res = await brpClient.spawnAssetInViewport(
          asset.uuid,
          vx ?? viewportState.width * 0.5,
          vy ?? viewportState.height * 0.5,
        );
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
        setSelection([res.guid]);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient, viewportState.width, viewportState.height],
  );

  const consoleAlertCount = useMemo(
    () => logs.filter((l) => l.level === "error" || l.level === "warn").length,
    [logs],
  );

  const handleStartForProject = useCallback(async (projectDir: string) => {
    setBusy(true);
    setBannerError(null);
    setSelection([]);
    setHierarchy([]);
    setAssetSnapshot(null);
    setActiveAssetFolder("");
    setRegistrySchema(null);
    try {
      const loaded = await loadProjectMetadata(projectDir);
      setActiveProject(loaded);
      const next = await startEngineHost(loaded.root_dir);
      setHostStatus(next);
    } catch (err) {
      setBannerError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  const handleBrowseProject = useCallback(async () => {
    const folder = await pickProjectFolder();
    if (!folder) {
      return;
    }
    await handleStartForProject(folder);
  }, [handleStartForProject]);

  const handleNewScene = useCallback(
    async (name: string, worldMode: WorldMode) => {
      if (!brpClient) {
        return;
      }
      setBusy(true);
      setBannerError(null);
      try {
        const res = await brpClient.newScene({ name, world_mode: worldMode });
        setSceneName(res.name);
        setScenePath(res.path);
        setSceneDirty(false);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      } finally {
        setBusy(false);
      }
    },
    [brpClient],
  );

  const handleOpenScene = useCallback(
    async (explicitPath?: string) => {
      if (!brpClient) {
        return;
      }
      const picked =
        explicitPath ?? (await pickOpenSceneFile(activeProject?.scenes_dir));
      if (!picked) {
        return;
      }
      setBusy(true);
      setBannerError(null);
      try {
        const res = await brpClient.loadScene({ path: picked });
        setSceneName(res.name);
        setScenePath(res.path);
        setSceneDirty(false);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      } finally {
        setBusy(false);
      }
    },
    [brpClient, activeProject?.scenes_dir],
  );

  const handleSaveSceneAs = useCallback(async () => {
    if (!brpClient) {
      return;
    }
    const defaultTarget =
      scenePath ??
      (activeProject ? `${activeProject.scenes_dir}/main.level.ron` : undefined);
    const picked = await pickSaveSceneFile(defaultTarget);
    if (!picked) {
      return;
    }
    setBusy(true);
    setBannerError(null);
    try {
      const res = await brpClient.saveScene({ path: picked, name: sceneName });
      setSceneName(res.name);
      setScenePath(res.path);
      setSceneDirty(false);
    } catch (err) {
      setBannerError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [brpClient, scenePath, sceneName, activeProject]);

  const handleSaveScene = useCallback(async () => {
    if (!brpClient) {
      return;
    }
    if (!scenePath) {
      await handleSaveSceneAs();
      return;
    }
    setBusy(true);
    setBannerError(null);
    try {
      const res = await brpClient.saveScene({ path: scenePath, name: sceneName });
      setSceneName(res.name);
      setScenePath(res.path);
      setSceneDirty(false);
    } catch (err) {
      setBannerError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [brpClient, scenePath, sceneName, handleSaveSceneAs]);

  const handleSetCameraMode = useCallback(
    (mode: ViewportCameraMode) => {
      ipcClient?.setCameraMode(mode);
      setViewportState((prev) => ({ ...prev, camera_mode: mode }));
    },
    [ipcClient],
  );

  const handleViewportInput = useCallback(
    (event: ViewportInputEvent) => {
      ipcClient?.sendViewportInput(event);
    },
    [ipcClient],
  );

  const handleToggleViewportMode = useCallback(() => {
    const nextMode =
      viewportState.viewport_mode === "offscreen" ? "native_window" : "offscreen";
    ipcClient?.setViewportMode(nextMode);
    setViewportState((prev) => ({ ...prev, viewport_mode: nextMode }));
  }, [ipcClient, viewportState.viewport_mode]);

  const handleChangeGizmoState = useCallback(
    (next: GizmoStatePayload) => {
      setGizmoState(next);
      ipcClient?.setGizmoState(next);
    },
    [ipcClient],
  );

  const handleSelectEntities = useCallback(
    (guids: string[]) => {
      setSelection(guids);
      ipcClient?.selectEntities(guids);
    },
    [ipcClient],
  );

  const handleToggleVisibility = useCallback(
    async (node: SceneTreeNode) => {
      if (!brpClient) {
        return;
      }
      setBannerError(null);
      try {
        await brpClient.setComponent(
          node.guid,
          "bevy_camera::visibility::Visibility",
          node.visible ? "Hidden" : "Inherited",
          undefined,
          node.visible ? "Hide Entity" : "Show Entity",
        );
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient],
  );

  const handleSpawnPreset = useCallback(
    async (preset: EntityPresetKind, parentGuid: string | null) => {
      if (!brpClient) {
        return;
      }
      setBannerError(null);
      try {
        const res = await brpClient.spawnPreset({
          preset,
          parent_guid: parentGuid,
        });
        setSelection([res.guid]);
        ipcClient?.selectEntities([res.guid]);
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient, ipcClient],
  );

  const handleRenameEntity = useCallback(
    async (node: SceneTreeNode, newName: string) => {
      if (!brpClient) {
        return;
      }
      setBannerError(null);
      try {
        await brpClient.setComponent(
          node.guid,
          "bevy_ecs::name::Name",
          newName,
          undefined,
          "Rename Entity",
        );
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient],
  );

  const handleDeleteEntity = useCallback(
    async (node: SceneTreeNode) => {
      if (!brpClient) {
        return;
      }
      setBannerError(null);
      try {
        const deleted = await brpClient.deleteEntity(node.guid);
        const deletedSet = new Set(deleted.deleted_guids);
        const nextSelection = selection.filter((g) => !deletedSet.has(g));
        setSelection(nextSelection);
        ipcClient?.selectEntities(nextSelection);
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient, ipcClient, selection],
  );

  const handleReparentEntity = useCallback(
    async (guid: string, parentGuid: string | null) => {
      if (!brpClient) {
        return;
      }
      setBannerError(null);
      try {
        await brpClient.reparentEntity(guid, parentGuid);
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient],
  );

  const handleUndo = useCallback(async () => {
    if (!brpClient) {
      return;
    }
    setBannerError(null);
    try {
      const res = await brpClient.undo();
      setHistoryState(res.state);
      if (res.applied) {
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
        setInspectorRevision((rev) => rev + 1);
      }
    } catch (err) {
      setBannerError(err instanceof Error ? err.message : String(err));
    }
  }, [brpClient]);

  const handleRedo = useCallback(async () => {
    if (!brpClient) {
      return;
    }
    setBannerError(null);
    try {
      const res = await brpClient.redo();
      setHistoryState(res.state);
      if (res.applied) {
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
        setInspectorRevision((rev) => rev + 1);
      }
    } catch (err) {
      setBannerError(err instanceof Error ? err.message : String(err));
    }
  }, [brpClient]);

  const handleSaveAsPrefab = useCallback(
    async (node: SceneTreeNode) => {
      if (!brpClient) {
        return;
      }
      const slug = node.name
        .trim()
        .toLowerCase()
        .replace(/[^a-z0-9_-]+/g, "_") || "entity";
      const defaultPath = activeProject
        ? `${activeProject.root_dir}/prefabs/${slug}.prefab.ron`
        : `prefabs/${slug}.prefab.ron`;

      let targetPath = isRunningInTauri()
        ? await pickSavePrefabFile(defaultPath)
        : window.prompt("Save prefab path (.prefab.ron):", defaultPath);
      if (!targetPath) {
        return;
      }

      setBannerError(null);
      try {
        await brpClient.savePrefab({
          root_guid: node.guid,
          path: targetPath,
          name: node.name,
        });
        setSceneDirty(true);
        const [tree, prefabList] = await Promise.all([
          brpClient.getWorldTree(),
          brpClient.listPrefabs(),
        ]);
        setHierarchy(tree.roots);
        setPrefabs(prefabList.prefabs);
        setInspectorRevision((rev) => rev + 1);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient, activeProject],
  );

  const handleSpawnPrefab = useCallback(
    async (path: string, parentGuid: string | null) => {
      if (!brpClient) {
        return;
      }
      setBannerError(null);
      try {
        const res = await brpClient.spawnPrefab({
          path,
          parent_guid: parentGuid,
        });
        setSelection([res.guid]);
        ipcClient?.selectEntities([res.guid]);
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient, ipcClient],
  );

  const handleBrowseSpawnPrefab = useCallback(
    async (parentGuid: string | null) => {
      const defaultPath = activeProject?.root_dir;
      const picked = isRunningInTauri()
        ? await pickOpenPrefabFile(defaultPath)
        : window.prompt(
            "Open prefab path (.prefab.ron):",
            "prefabs/crate.prefab.ron",
          );
      if (picked) {
        await handleSpawnPrefab(picked, parentGuid);
      }
    },
    [activeProject, handleSpawnPrefab],
  );

  const handleApplyPrefab = useCallback(
    async (node: SceneTreeNode) => {
      if (!brpClient) {
        return;
      }
      setBannerError(null);
      try {
        await brpClient.applyPrefab(node.guid);
        setSceneDirty(true);
        const [tree, prefabList] = await Promise.all([
          brpClient.getWorldTree(),
          brpClient.listPrefabs(),
        ]);
        setHierarchy(tree.roots);
        setPrefabs(prefabList.prefabs);
        setInspectorRevision((rev) => rev + 1);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient],
  );

  const handleRevertPrefab = useCallback(
    async (node: SceneTreeNode) => {
      if (!brpClient) {
        return;
      }
      setBannerError(null);
      try {
        await brpClient.revertPrefab(node.guid, null);
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
        setInspectorRevision((rev) => rev + 1);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient],
  );

  const handleRevertPrefabComponent = useCallback(
    async (node: SceneTreeNode, typePath: string) => {
      if (!brpClient) {
        return;
      }
      setBannerError(null);
      try {
        await brpClient.revertPrefab(node.guid, typePath);
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
        setInspectorRevision((rev) => rev + 1);
      } catch (err) {
        setBannerError(err instanceof Error ? err.message : String(err));
      }
    },
    [brpClient],
  );

  const handleRestartHost = useCallback(async () => {
    setBusy(true);
    setBannerError(null);
    try {
      const next = await restartEngineHost();
      setHostStatus(next);
    } catch (err) {
      setBannerError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    function onWindowKeyDown(event: KeyboardEvent) {
      const target = event.target as HTMLElement | null;
      const tag = target?.tagName;
      const isTextInput =
        tag === "INPUT" ||
        tag === "TEXTAREA" ||
        tag === "SELECT" ||
        target?.isContentEditable === true;

      if ((event.ctrlKey || event.metaKey) && event.code === "KeyS") {
        event.preventDefault();
        void handleSaveScene();
        return;
      }

      if ((event.ctrlKey || event.metaKey) && !event.altKey && !isTextInput) {
        if (event.code === "KeyZ") {
          event.preventDefault();
          if (event.shiftKey) {
            void handleRedo();
          } else {
            void handleUndo();
          }
          return;
        }
        if (event.code === "KeyY") {
          event.preventDefault();
          void handleRedo();
          return;
        }
      }

      if (
        isTextInput ||
        tag === "CANVAS" ||
        event.ctrlKey ||
        event.metaKey ||
        event.altKey
      ) {
        return;
      }

      const nextMode =
        event.code === "KeyW"
          ? "translate"
          : event.code === "KeyE"
            ? "rotate"
            : event.code === "KeyR"
              ? "scale"
              : null;
      if (nextMode) {
        event.preventDefault();
        handleChangeGizmoState({ ...gizmoState, mode: nextMode });
      }
    }
    window.addEventListener("keydown", onWindowKeyDown);
    return () => {
      window.removeEventListener("keydown", onWindowKeyDown);
    };
  }, [
    handleSaveScene,
    handleUndo,
    handleRedo,
    handleChangeGizmoState,
    gizmoState,
  ]);

  const selectedNode = useMemo(
    () =>
      selection.length > 0 ? findNodeByGuid(hierarchy, selection[0]) : null,
    [hierarchy, selection],
  );

  const selectedPrefabRootNode = useMemo(
    () =>
      selectedNode?.is_prefab_member
        ? findPrefabRootForGuid(hierarchy, selectedNode.guid)
        : null,
    [hierarchy, selectedNode],
  );

  const handleComponentChanged = useCallback(
    async (refreshTree: boolean) => {
      setSceneDirty(true);
      if (refreshTree && brpClient) {
        try {
          const tree = await brpClient.getWorldTree();
          setHierarchy(tree.roots);
        } catch {
          // Hierarchy snapshot over WS will sync on next tick.
        }
      }
    },
    [brpClient],
  );

  return (
    <div className="editor-shell">
      <Toolbar
        brpClient={brpClient}
        projectKey={`${hostStatus.project_dir ?? ""}:${hostStatus.pid ?? ""}`}
        samples={samples}
        activeProject={activeProject}
        hostStatus={hostStatus}
        ipcState={ipcState}
        sceneName={sceneName}
        sceneDirty={sceneDirty}
        cameraMode={viewportState.camera_mode}
        historyState={historyState}
        metrics={metrics}
        busy={busy}
        onSelectProject={(dir) => void handleStartForProject(dir)}
        onBrowseProject={() => void handleBrowseProject()}
        onNewScene={(name, worldMode) => void handleNewScene(name, worldMode)}
        onOpenScene={(path) => void handleOpenScene(path)}
        onSaveScene={() => void handleSaveScene()}
        onSaveSceneAs={() => void handleSaveSceneAs()}
        onUndo={() => void handleUndo()}
        onRedo={() => void handleRedo()}
        onSetCameraMode={handleSetCameraMode}
        onStartHost={() =>
          activeProject && void handleStartForProject(activeProject.root_dir)
        }
        onRestartHost={() => void handleRestartHost()}
        onResetLayout={() => updateLayout(DEFAULT_LAYOUT_STATE)}
      />

      {bannerError && (
        <div className="error-strip">
          <span>{bannerError}</span>
          <button
            type="button"
            className="ghost-btn"
            onClick={() => setBannerError(null)}
          >
            Dismiss
          </button>
        </div>
      )}

      <DockLayout
        layout={layout}
        onChange={updateLayout}
        left={
          <OutlinerPanel
            roots={hierarchy}
            selectedGuids={selection}
            prefabs={prefabs}
            connected={ipcState === "connected" || brpClient !== null}
            onSelect={handleSelectEntities}
            onToggleVisibility={(node) => void handleToggleVisibility(node)}
            onSpawnPreset={(preset, parentGuid) =>
              void handleSpawnPreset(preset, parentGuid)
            }
            onSpawnPrefab={(path, parentGuid) =>
              void handleSpawnPrefab(path, parentGuid)
            }
            onBrowseSpawnPrefab={(parentGuid) =>
              void handleBrowseSpawnPrefab(parentGuid)
            }
            onRename={(node, newName) => void handleRenameEntity(node, newName)}
            onDelete={(node) => void handleDeleteEntity(node)}
            onReparent={(guid, parentGuid) =>
              void handleReparentEntity(guid, parentGuid)
            }
            onCollapse={() => updateLayout({ ...layout, leftCollapsed: true })}
          />
        }
        center={
          <ViewportPanel
            sceneName={sceneName}
            viewportMode={viewportState.viewport_mode}
            cameraMode={viewportState.camera_mode}
            width={viewportState.width}
            height={viewportState.height}
            ipcState={ipcState}
            gizmoState={gizmoState}
            assets={assetSnapshot?.assets ?? []}
            subscribeFrames={subscribeFrames}
            onViewportInput={handleViewportInput}
            onToggleViewportMode={handleToggleViewportMode}
            onChangeGizmoState={handleChangeGizmoState}
            onDropAsset={(asset, vx, vy) =>
              void handleSpawnAssetInViewport(asset, vx, vy)
            }
          />
        }
        activeBottomTab={activeBottomTab}
        onChangeBottomTab={setActiveBottomTab}
        consoleBadgeCount={consoleAlertCount}
        bottomContentBrowser={
          <ContentBrowserPanel
            snapshot={assetSnapshot}
            currentFolder={activeAssetFolder}
            onChangeFolder={setActiveAssetFolder}
            brpClient={brpClient}
            onImportPaths={handleImportAssetPaths}
            onRefreshAssets={refreshAssets}
            onOpenScenePath={(path) => void handleOpenScene(path)}
            onSpawnAssetAtCenter={(asset) =>
              void handleSpawnAssetInViewport(asset)
            }
          />
        }
        bottomConsole={
          <ConsolePanel
            logs={logs}
            onClear={() => {
              setLogs([]);
              void clearEngineLogs();
            }}
            onCollapse={() => updateLayout({ ...layout, bottomCollapsed: true })}
          />
        }
        right={
          <InspectorPanel
            selectedNode={selectedNode}
            prefabRootNode={selectedPrefabRootNode}
            selectedCount={selection.length}
            brpClient={brpClient}
            registrySchema={registrySchema}
            assets={assetSnapshot?.assets ?? []}
            inspectorRevision={inspectorRevision}
            onComponentChanged={(refreshTree) =>
              void handleComponentChanged(refreshTree)
            }
            onDeleteEntity={(node) => void handleDeleteEntity(node)}
            onSaveAsPrefab={(node) => void handleSaveAsPrefab(node)}
            onApplyPrefab={(node) => void handleApplyPrefab(node)}
            onRevertPrefab={(node) => void handleRevertPrefab(node)}
            onRevertPrefabComponent={(node, typePath) =>
              void handleRevertPrefabComponent(node, typePath)
            }
            onCollapse={() => updateLayout({ ...layout, rightCollapsed: true })}
          />
        }
      />
    </div>
  );
}
