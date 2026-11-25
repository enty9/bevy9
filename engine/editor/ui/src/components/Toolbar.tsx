import { useState, type FormEvent } from "react";
import {
  FilePlus2,
  FolderOpen,
  LayoutGrid,
  Play,
  RotateCw,
  Save,
  SaveAll,
} from "lucide-react";
import type { EngineHostStatus, ProjectSummary } from "../lib/tauriBridge";
import type { IpcConnectionState } from "../protocol/ipcClient";
import type { FrameMetrics, ViewportCameraMode, WorldMode } from "../protocol/types";

interface ToolbarProps {
  samples: ProjectSummary[];
  activeProject: ProjectSummary | null;
  hostStatus: EngineHostStatus;
  ipcState: IpcConnectionState;
  sceneName: string;
  sceneDirty: boolean;
  cameraMode: ViewportCameraMode;
  metrics: FrameMetrics;
  busy: boolean;
  onSelectProject: (projectDir: string) => void;
  onBrowseProject: () => void;
  onNewScene: (name: string, worldMode: WorldMode) => void;
  onOpenScene: (explicitPath?: string) => void;
  onSaveScene: () => void;
  onSaveSceneAs: () => void;
  onSetCameraMode: (mode: ViewportCameraMode) => void;
  onStartHost: () => void;
  onRestartHost: () => void;
  onResetLayout: () => void;
}

export function Toolbar({
  samples,
  activeProject,
  hostStatus,
  ipcState,
  sceneName,
  sceneDirty,
  cameraMode,
  metrics,
  busy,
  onSelectProject,
  onBrowseProject,
  onNewScene,
  onOpenScene,
  onSaveScene,
  onSaveSceneAs,
  onSetCameraMode,
  onStartHost,
  onRestartHost,
  onResetLayout,
}: ToolbarProps) {
  const [newPopoverOpen, setNewPopoverOpen] = useState(false);
  const [newSceneName, setNewSceneName] = useState("Untitled Scene");
  const [newWorldMode, setNewWorldMode] = useState<WorldMode>("3d");

  const projectOptions = [...samples];
  if (
    activeProject &&
    !projectOptions.some((item) => item.root_dir === activeProject.root_dir)
  ) {
    projectOptions.unshift(activeProject);
  }

  const hostRunning = hostStatus.state === "running" || ipcState === "connected";
  const connectionLabel =
    ipcState === "connected"
      ? "Connected"
      : hostStatus.state === "starting" || ipcState === "connecting"
        ? "Connecting"
        : hostStatus.state === "crashed"
          ? "Crashed"
          : "Stopped";

  const connectionClass =
    ipcState === "connected"
      ? "running"
      : hostStatus.state === "crashed"
        ? "crashed"
        : hostStatus.state === "starting" || ipcState === "connecting"
          ? "starting"
          : "stopped";

  function handleCreateSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const trimmed = newSceneName.trim() || "Untitled Scene";
    setNewPopoverOpen(false);
    onNewScene(trimmed, newWorldMode);
  }

  return (
    <header className="toolbar">
      <div className="toolbar-section">
        <div className="brand">
          <span className="brand-dot" />
          <span>Bevy9</span>
        </div>

        <label className="toolbar-field" title="Active project">
          <select
            className="select-input"
            disabled={busy || projectOptions.length === 0}
            value={activeProject?.root_dir ?? ""}
            onChange={(e) => {
              if (e.target.value) {
                onSelectProject(e.target.value);
              }
            }}
          >
            {projectOptions.length === 0 && (
              <option value="">No project loaded</option>
            )}
            {projectOptions.map((project) => (
              <option key={project.root_dir} value={project.root_dir}>
                {project.name} ({project.default_mode.toUpperCase()})
              </option>
            ))}
          </select>
        </label>

        <button
          type="button"
          disabled={busy}
          title="Browse project directory"
          onClick={onBrowseProject}
        >
          <FolderOpen size={14} />
          Project...
        </button>

        <div className="toolbar-divider" />

        <div className="popover-anchor">
          <button
            type="button"
            disabled={busy || !hostRunning}
            onClick={() => {
              setNewWorldMode(cameraMode === "2d" ? "2d" : "3d");
              setNewPopoverOpen((open) => !open);
            }}
          >
            <FilePlus2 size={14} />
            New
          </button>

          {newPopoverOpen && (
            <form className="popover-card" onSubmit={handleCreateSubmit}>
              <div className="popover-title">New Scene</div>
              <input
                type="text"
                className="text-input"
                value={newSceneName}
                onChange={(e) => setNewSceneName(e.target.value)}
                placeholder="Scene name"
                autoFocus
              />
              <div className="segmented">
                {(["3d", "2d", "hybrid"] as const).map((mode) => (
                  <button
                    key={mode}
                    type="button"
                    className={`seg-btn ${newWorldMode === mode ? "active" : ""}`}
                    onClick={() => setNewWorldMode(mode)}
                  >
                    {mode.toUpperCase()}
                  </button>
                ))}
              </div>
              <div className="popover-actions">
                <button
                  type="button"
                  onClick={() => setNewPopoverOpen(false)}
                >
                  Cancel
                </button>
                <button type="submit" className="primary">
                  Create
                </button>
              </div>
            </form>
          )}
        </div>

        <button
          type="button"
          disabled={busy || !hostRunning}
          title="Open .level.ron scene file"
          onClick={() => onOpenScene()}
        >
          <FolderOpen size={14} />
          Open Scene
        </button>

        <button
          type="button"
          className="primary"
          disabled={busy || !hostRunning}
          title="Save active scene (Ctrl+S)"
          onClick={onSaveScene}
        >
          <Save size={14} />
          Save
        </button>

        <button
          type="button"
          disabled={busy || !hostRunning}
          title="Save active scene to a new .level.ron file"
          onClick={onSaveSceneAs}
        >
          <SaveAll size={14} />
          Save As
        </button>
      </div>

      <div className="toolbar-section">
        <span className="scene-label" title={sceneName}>
          {sceneName}
          {sceneDirty ? " *" : ""}
        </span>

        <div className="segmented" title="Editor viewport camera mode">
          <button
            type="button"
            className={`seg-btn ${cameraMode === "3d" ? "active" : ""}`}
            disabled={!hostRunning}
            onClick={() => onSetCameraMode("3d")}
          >
            3D
          </button>
          <button
            type="button"
            className={`seg-btn ${cameraMode === "2d" ? "active" : ""}`}
            disabled={!hostRunning}
            onClick={() => onSetCameraMode("2d")}
          >
            2D
          </button>
        </div>

        <div className="toolbar-divider" />

        <span className={`badge ${connectionClass}`}>{connectionLabel}</span>
        <span className="metric-pill" title="Viewport FPS / Scene entities">
          {metrics.fps > 0 ? `${metrics.fps.toFixed(0)} FPS` : "-- FPS"} ·{" "}
          {metrics.scene_entity_count} entities
        </span>

        {hostStatus.state === "stopped" || hostStatus.state === "crashed" ? (
          <button
            type="button"
            className="primary"
            disabled={busy || !activeProject}
            onClick={onStartHost}
          >
            <Play size={13} />
            Start Host
          </button>
        ) : (
          <button
            type="button"
            disabled={busy}
            title="Restart Engine Host"
            onClick={onRestartHost}
          >
            <RotateCw size={13} />
            Restart
          </button>
        )}

        <button
          type="button"
          title="Reset docking panels to default sizes"
          onClick={onResetLayout}
        >
          <LayoutGrid size={13} />
        </button>
      </div>
    </header>
  );
}
