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
  pickOpenSceneFile,
  pickProjectFolder,
  pickSaveSceneFile,
  restartEngineHost,
  startEngineHost,
  subscribeEngineLogs,
  subscribeEngineStatus,
  type EngineHostStatus,
  type EngineLogMessage,
  type ProjectSummary,
} from "./lib/tauriBridge";
import { BrpClient } from "./protocol/brpClient";
import { EditorIpcClient, type IpcConnectionState } from "./protocol/ipcClient";
import type {
  BrpRegistrySchema,
  DecodedViewportFrame,
  EntityPresetKind,
  FrameMetrics,
  SceneTreeNode,
  ViewportCameraMode,
  ViewportInputEvent,
  ViewportStatePayload,
  WorldMode,
} from "./protocol/types";
import { ConsolePanel } from "./components/ConsolePanel";
import {
  DEFAULT_LAYOUT_STATE,
  DockLayout,
  loadSavedLayout,
  persistLayout,
  type DockLayoutState,
} from "./components/DockLayout";
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

export type FrameSubscriber = (frame: DecodedViewportFrame) => void;

export function App() {
  const [samples, setSamples] = useState<ProjectSummary[]>([]);
  const [activeProject, setActiveProject] = useState<ProjectSummary | null>(null);
  const [hostStatus, setHostStatus] = useState<EngineHostStatus>(DEFAULT_ENGINE_STATUS);
  const [ipcState, setIpcState] = useState<IpcConnectionState>("disconnected");
  const [layout, setLayout] = useState<DockLayoutState>(() => loadSavedLayout());

  const [registrySchema, setRegistrySchema] = useState<BrpRegistrySchema | null>(null);
  const [hierarchy, setHierarchy] = useState<SceneTreeNode[]>([]);
  const [selection, setSelection] = useState<string[]>([]);
  const [viewportState, setViewportState] = useState<ViewportStatePayload>(
    DEFAULT_VIEWPORT_STATE,
  );
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
        const [schema, tree] = await Promise.all([
          brpClient.getRegistrySchema(),
          brpClient.getWorldTree(),
        ]);
        if (cancelled) {
          return;
        }
        setRegistrySchema(schema);
        setHierarchy(tree.roots);
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

  const handleStartForProject = useCallback(async (projectDir: string) => {
    setBusy(true);
    setBannerError(null);
    setSelection([]);
    setHierarchy([]);
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
        await brpClient.insertComponents(node.entity_bits, {
          "bevy_camera::visibility::Visibility": node.visible
            ? "Hidden"
            : "Inherited",
        });
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
        await brpClient.insertComponents(node.entity_bits, {
          "bevy_ecs::name::Name": newName,
        });
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
        await brpClient.despawnEntity(node.entity_bits);
        const nextSelection = selection.filter((g) => g !== node.guid);
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
    async (entityBits: number, parentBits: number | null) => {
      if (!brpClient) {
        return;
      }
      setBannerError(null);
      try {
        await brpClient.reparentEntities([entityBits], parentBits);
        setSceneDirty(true);
        const tree = await brpClient.getWorldTree();
        setHierarchy(tree.roots);
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
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        void handleSaveScene();
      }
    }
    window.addEventListener("keydown", onWindowKeyDown);
    return () => {
      window.removeEventListener("keydown", onWindowKeyDown);
    };
  }, [handleSaveScene]);

  const schemaCount = registrySchema ? Object.keys(registrySchema).length : 0;

  return (
    <div className="editor-shell">
      <Toolbar
        samples={samples}
        activeProject={activeProject}
        hostStatus={hostStatus}
        ipcState={ipcState}
        sceneName={sceneName}
        sceneDirty={sceneDirty}
        cameraMode={viewportState.camera_mode}
        metrics={metrics}
        busy={busy}
        onSelectProject={(dir) => void handleStartForProject(dir)}
        onBrowseProject={() => void handleBrowseProject()}
        onNewScene={(name, worldMode) => void handleNewScene(name, worldMode)}
        onOpenScene={(path) => void handleOpenScene(path)}
        onSaveScene={() => void handleSaveScene()}
        onSaveSceneAs={() => void handleSaveSceneAs()}
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
            connected={ipcState === "connected" || brpClient !== null}
            onSelect={handleSelectEntities}
            onToggleVisibility={(node) => void handleToggleVisibility(node)}
            onSpawnPreset={(preset, parentGuid) =>
              void handleSpawnPreset(preset, parentGuid)
            }
            onRename={(node, newName) => void handleRenameEntity(node, newName)}
            onDelete={(node) => void handleDeleteEntity(node)}
            onReparent={(entityBits, parentBits) =>
              void handleReparentEntity(entityBits, parentBits)
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
            subscribeFrames={subscribeFrames}
            onViewportInput={handleViewportInput}
            onToggleViewportMode={handleToggleViewportMode}
          />
        }
        bottom={
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
          <div className="panel">
            <div className="panel-header">
              <span>Inspector</span>
              <button
                type="button"
                className="ghost-btn"
                onClick={() => updateLayout({ ...layout, rightCollapsed: true })}
              >
                ×
              </button>
            </div>
            <div className="panel-body">
              <div className="muted">Selected: {selection.length}</div>
              <div className="muted">Reflected schema types: {schemaCount}</div>
            </div>
          </div>
        }
      />
    </div>
  );
}
