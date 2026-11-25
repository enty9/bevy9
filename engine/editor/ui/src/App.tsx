import { useEffect, useState } from "react";
import { FolderOpen, Play, RotateCw, Square, Terminal, Trash2 } from "lucide-react";
import {
  clearEngineLogs,
  DEFAULT_ENGINE_STATUS,
  getEngineLogs,
  getEngineStatus,
  isRunningInTauri,
  listSampleProjects,
  loadProjectMetadata,
  pickProjectFolder,
  restartEngineHost,
  startEngineHost,
  stopEngineHost,
  subscribeEngineLogs,
  subscribeEngineStatus,
  type EngineHostStatus,
  type EngineLogMessage,
  type ProjectSummary,
} from "./lib/tauriBridge";

export function App() {
  const [samples, setSamples] = useState<ProjectSummary[]>([]);
  const [selectedProject, setSelectedProject] = useState<ProjectSummary | null>(null);
  const [status, setStatus] = useState<EngineHostStatus>(DEFAULT_ENGINE_STATUS);
  const [logs, setLogs] = useState<EngineLogMessage[]>([]);
  const [busy, setBusy] = useState(false);
  const [errorBanner, setErrorBanner] = useState<string | null>(null);

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
      setStatus(initialStatus);
      setLogs(initialLogs);

      if (foundSamples.length > 0) {
        setSelectedProject(foundSamples[0]);
      }

      unlisteners.push(
        await subscribeEngineStatus((next) => {
          if (active) {
            setStatus(next);
          }
        }),
      );
      unlisteners.push(
        await subscribeEngineLogs((entry) => {
          if (active) {
            setLogs((prev) => [...prev.slice(-499), entry]);
          }
        }),
      );
    }

    void bootstrap();

    return () => {
      active = false;
      for (const unlisten of unlisteners) {
        unlisten();
      }
    };
  }, []);

  async function handleStart(project: ProjectSummary) {
    setBusy(true);
    setErrorBanner(null);
    setSelectedProject(project);
    try {
      const next = await startEngineHost(project.root_dir);
      setStatus(next);
    } catch (err) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function handleStop() {
    setBusy(true);
    setErrorBanner(null);
    try {
      const next = await stopEngineHost();
      setStatus(next);
    } finally {
      setBusy(false);
    }
  }

  async function handleRestart() {
    setBusy(true);
    setErrorBanner(null);
    try {
      const next = await restartEngineHost();
      setStatus(next);
    } catch (err) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function handleBrowseProject() {
    const dir = await pickProjectFolder();
    if (!dir) {
      return;
    }
    setErrorBanner(null);
    try {
      const loaded = await loadProjectMetadata(dir);
      setSelectedProject(loaded);
      await handleStart(loaded);
    } catch (err) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    }
  }

  return (
    <div className="bootstrap-shell">
      <header className="topbar">
        <div className="brand">
          <span>Bevy9 Editor</span>
          <span className={`badge ${status.state}`}>{status.state}</span>
          {!isRunningInTauri() && <span className="muted">(browser preview)</span>}
        </div>
        <div className="toolbar-group">
          <button type="button" onClick={() => void handleBrowseProject()} disabled={busy}>
            <FolderOpen size={14} />
            Open Project
          </button>
          <button
            type="button"
            className="primary"
            disabled={busy || !selectedProject}
            onClick={() => selectedProject && void handleStart(selectedProject)}
          >
            <Play size={14} />
            Start Host
          </button>
          <button type="button" disabled={busy || status.state === "stopped"} onClick={() => void handleRestart()}>
            <RotateCw size={14} />
            Restart
          </button>
          <button type="button" disabled={busy || status.state === "stopped"} onClick={() => void handleStop()}>
            <Square size={14} />
            Stop
          </button>
        </div>
      </header>

      <main className="bootstrap-main">
        <section className="card">
          <div className="card-header">
            <span>Projects &amp; Host Status</span>
          </div>
          <div className="card-body">
            {errorBanner && <div className="badge crashed">{errorBanner}</div>}

            <div className="muted">Sample Projects</div>
            {samples.length === 0 ? (
              <div className="muted">No sample projects loaded (open inside Tauri or browse directory).</div>
            ) : (
              samples.map((project) => (
                <button
                  key={project.root_dir}
                  type="button"
                  className={`project-item ${
                    selectedProject?.root_dir === project.root_dir ? "selected" : ""
                  }`}
                  onClick={() => setSelectedProject(project)}
                >
                  <div>
                    <div>{project.name}</div>
                    <div className="muted">{project.startup_scene}</div>
                  </div>
                  <span className="badge">{project.default_mode}</span>
                </button>
              ))
            )}

            <div className="muted" style={{ marginTop: 8 }}>
              Active Endpoints
            </div>
            <div className="muted">BRP: {status.brp_url ?? "—"}</div>
            <div className="muted">IPC: {status.ipc_url ?? "—"}</div>
            <div className="muted">PID: {status.pid ?? "—"}</div>
            {status.last_error && <div className="muted">Last error: {status.last_error}</div>}
          </div>
        </section>

        <section className="card">
          <div className="card-header">
            <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
              <Terminal size={14} />
              Engine Host Logs
            </span>
            <button
              type="button"
              onClick={() => {
                setLogs([]);
                void clearEngineLogs();
              }}
            >
              <Trash2 size={13} />
              Clear
            </button>
          </div>
          <div className="card-body">
            <div className="log-list">
              {logs.map((entry, idx) => (
                <div key={`${entry.timestamp_ms}-${idx}`} className="log-row">
                  <span className={`log-level ${entry.level}`}>{entry.level}</span>
                  <span className="muted">{entry.target}</span>
                  <span>{entry.message}</span>
                </div>
              ))}
            </div>
          </div>
        </section>
      </main>
    </div>
  );
}
