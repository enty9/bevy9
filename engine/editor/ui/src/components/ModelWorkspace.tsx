import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { AlertTriangle, Box, Monitor, Save, X } from "lucide-react";
import type { ModelTab } from "../lib/modelTabs";

interface Status {
  ready: boolean;
  dirty: boolean;
  saved: number;
  reply?: string | null;
  error?: string | null;
}

interface TabBadge {
  dirty: boolean;
  error: boolean;
  ready: boolean;
}

interface Props {
  tabs: ModelTab[];
  active: string | null;
  occluded?: boolean;
  select: (token: string | null) => void;
  close: (token: string) => Promise<void>;
  onModelSaved?: (tab: ModelTab) => Promise<void> | void;
  children: ReactNode;
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function hasOverlappingDomOverlay(bounds: DOMRect): boolean {
  const overlays = document.querySelectorAll(
    ".popover-card, [role='dialog'], [aria-modal='true']",
  );
  for (const el of overlays) {
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;
    const overlaps =
      r.left < bounds.right + 2 &&
      r.right > bounds.left - 2 &&
      r.top < bounds.bottom + 2 &&
      r.bottom > bounds.top - 2;
    if (overlaps) {
      return true;
    }
  }
  return false;
}

export function ModelWorkspace({
  tabs,
  active,
  occluded = false,
  select,
  close,
  onModelSaved,
  children,
}: Props) {
  const [closing, setClosing] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [badges, setBadges] = useState<Record<string, TabBadge>>({});

  const seenSavedRef = useRef<Map<string, number>>(new Map());
  const refreshPromisesRef = useRef<Map<string, Promise<void>>>(new Map());
  const closingTokensRef = useRef<Set<string>>(new Set());
  const onModelSavedRef = useRef(onModelSaved);
  onModelSavedRef.current = onModelSaved;

  const activeTab = tabs.find((tab) => tab.token === active) ?? null;

  useEffect(() => {
    const valid = new Set(tabs.map((tab) => tab.token));
    setClosing((prev) => (prev && valid.has(prev) ? prev : null));
    setBadges((prev) => {
      const entries = Object.entries(prev).filter(([token]) => valid.has(token));
      return entries.length === Object.keys(prev).length
        ? prev
        : Object.fromEntries(entries);
    });
    for (const token of seenSavedRef.current.keys()) {
      if (!valid.has(token)) {
        seenSavedRef.current.delete(token);
      }
    }
    for (const token of refreshPromisesRef.current.keys()) {
      if (!valid.has(token)) {
        refreshPromisesRef.current.delete(token);
      }
    }
    for (const token of closingTokensRef.current) {
      if (!valid.has(token)) {
        closingTokensRef.current.delete(token);
      }
    }
  }, [tabs]);

  const reportSaved = useCallback(async (tab: ModelTab, savedCount: number) => {
    const seen = seenSavedRef.current.get(tab.token);
    if (seen === undefined) {
      seenSavedRef.current.set(tab.token, savedCount);
      return;
    }
    if (savedCount > seen) {
      seenSavedRef.current.set(tab.token, savedCount);
      const pending = Promise.resolve(onModelSavedRef.current?.(tab)).finally(() => {
        if (refreshPromisesRef.current.get(tab.token) === pending) {
          refreshPromisesRef.current.delete(tab.token);
        }
      });
      refreshPromisesRef.current.set(tab.token, pending);
      await pending;
      return;
    }
    const inflight = refreshPromisesRef.current.get(tab.token);
    if (inflight) {
      await inflight;
    }
  }, []);

  const switchTo = useCallback(
    (token: string | null) => {
      setClosing(null);
      select(token);
    },
    [select],
  );

  const handleBadgeChange = useCallback((token: string, next: TabBadge) => {
    setBadges((prev) => {
      const current = prev[token];
      if (
        current &&
        current.dirty === next.dirty &&
        current.error === next.error &&
        current.ready === next.ready
      ) {
        return prev;
      }
      return { ...prev, [token]: next };
    });
  }, []);

  const requestClose = useCallback(
    async (token: string) => {
      if (saving || closingTokensRef.current.has(token)) return;
      closingTokensRef.current.add(token);
      const tab = tabs.find((item) => item.token === token) ?? null;
      try {
        const latest = await invoke<Status>("model_status", { token });
        if (tab) {
          await reportSaved(tab, latest.saved);
        }
        if (!latest.ready || !latest.dirty) {
          setClosing((prev) => (prev === token ? null : prev));
          await close(token);
          return;
        }
        select(token);
        setClosing(token);
      } catch {
        // Process already exited or failed to start; close without prompting to save.
        setClosing((prev) => (prev === token ? null : prev));
        await close(token);
      } finally {
        closingTokensRef.current.delete(token);
      }
    },
    [saving, tabs, reportSaved, close, select],
  );

  return (
    <section className="model-workspace">
      {tabs.length > 0 && (
        <nav className="model-tabs" aria-label="Viewport documents">
          <button
            type="button"
            className={`model-tab-btn ${!activeTab ? "active" : ""}`}
            disabled={saving}
            onClick={() => switchTo(null)}
          >
            <Monitor size={12} />
            Viewport
          </button>
          {tabs.map(({ token, path }) => {
            const badge = badges[token];
            const isSelected = activeTab?.token === token;
            const fileName = path.split("/").pop() ?? path;
            return (
              <div
                key={token}
                className={`model-tab ${isSelected ? "active" : ""}`}
              >
                <button
                  type="button"
                  className={`model-tab-btn ${isSelected ? "active" : ""}`}
                  title={path}
                  disabled={saving}
                  onClick={() => switchTo(token)}
                >
                  <Box size={12} />
                  <span className="truncate">{fileName}</span>
                  {badge?.dirty && (
                    <span className="model-tab-dirty" title="Unsaved model changes">
                      *
                    </span>
                  )}
                  {badge?.error && (
                    <AlertTriangle
                      size={11}
                      className="model-tab-error-icon"
                      aria-label="Model error"
                    />
                  )}
                </button>
                <button
                  type="button"
                  className="model-tab-close"
                  aria-label={`Close ${path}`}
                  title="Close model tab"
                  disabled={saving}
                  onClick={() => void requestClose(token)}
                >
                  <X size={12} />
                </button>
              </div>
            );
          })}
        </nav>
      )}
      {!activeTab && children}
      {tabs.map((tab) => (
        <ModelSurface
          key={tab.token}
          tab={tab}
          active={tab.token === activeTab?.token}
          closing={closing === tab.token}
          occluded={occluded}
          onCancel={() => setClosing(null)}
          onClose={() => close(tab.token)}
          onSaving={setSaving}
          onBadgeChange={handleBadgeChange}
          onReportSaved={reportSaved}
        />
      ))}
    </section>
  );
}

interface ModelSurfaceProps {
  tab: ModelTab;
  active: boolean;
  closing: boolean;
  occluded: boolean;
  onCancel: () => void;
  onClose: () => Promise<void>;
  onSaving: (saving: boolean) => void;
  onBadgeChange: (token: string, badge: TabBadge) => void;
  onReportSaved: (tab: ModelTab, savedCount: number) => Promise<void>;
}

function ModelSurface({
  tab,
  active,
  closing,
  occluded,
  onCancel,
  onClose,
  onSaving,
  onBadgeChange,
  onReportSaved,
}: ModelSurfaceProps) {
  const { token, path } = tab;
  const slot = useRef<HTMLDivElement>(null);
  const [status, setStatus] = useState<Status>({ ready: false, dirty: false, saved: 0 });
  const [error, setError] = useState<string | null>(null);
  const [fatalError, setFatalError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [overlayPaused, setOverlayPaused] = useState(false);
  const savingRef = useRef(false);
  const mountedRef = useRef(true);
  const wasActiveRef = useRef(active);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (savingRef.current) {
        savingRef.current = false;
        onSaving(false);
      }
    };
  }, [onSaving]);

  // Sync tab badge indicators up to ModelWorkspace.
  useEffect(() => {
    onBadgeChange(token, {
      dirty: status.dirty,
      error: Boolean(error || status.error || fatalError),
      ready: status.ready,
    });
  }, [token, status.dirty, status.error, status.ready, error, fatalError, onBadgeChange]);

  // Poll Blender status while active; also run a one-shot check when switching
  // away so a Ctrl+S pressed inside Blender right before clicking Viewport still rescans.
  useEffect(() => {
    if (!active) {
      if (wasActiveRef.current && !fatalError) {
        void invoke<Status>("model_status", { token })
          .then((next) => {
            if (!mountedRef.current) return;
            setStatus(next);
            void onReportSaved(tab, next.saved);
          })
          .catch(() => {});
      }
      wasActiveRef.current = false;
      return;
    }
    wasActiveRef.current = true;
    if (fatalError) {
      return;
    }

    let live = true;
    let timer = 0;
    async function poll() {
      try {
        const next = await invoke<Status>("model_status", { token });
        if (!live || !mountedRef.current) return;
        setStatus(next);
        void onReportSaved(tab, next.saved);
        timer = window.setTimeout(() => void poll(), 500);
      } catch (err) {
        if (!live || !mountedRef.current) return;
        setFatalError(true);
        setError(errorMessage(err));
        setStatus((prev) => ({ ...prev, ready: false }));
      }
    }
    void poll();
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [active, token, tab, fatalError, onReportSaved]);

  // Keep native Blender child window synchronized with slot bounds and hidden whenever
  // inactive, closing, errored, minimized, or occluded by a popover or native dialog.
  useEffect(() => {
    const hide = () => {
      void invoke("model_layout", { token, rect: null }).catch(() => {});
    };
    if (!active || closing || occluded || fatalError) {
      setOverlayPaused(occluded && active && !closing && !fatalError);
      hide();
      return;
    }

    let frame = 0;
    let lastSignature = "";
    let lastNonNullSentAt = 0;
    let lastOccluded = false;

    const update = () => {
      const bounds = slot.current?.getBoundingClientRect();
      if (bounds) {
        const domOccluded = document.hidden || hasOverlappingDomOverlay(bounds);
        if (domOccluded !== lastOccluded) {
          lastOccluded = domOccluded;
          setOverlayPaused(domOccluded);
        }
        const px = (v: number) => Math.round(v * window.devicePixelRatio);
        const rect =
          !domOccluded && bounds.width > 1 && bounds.height > 1
            ? {
                x: px(bounds.left),
                y: px(bounds.top),
                width: px(bounds.width),
                height: px(bounds.height),
              }
            : null;
        const signature = JSON.stringify(rect);
        if (lastSignature !== signature) {
          const now = performance.now();
          // Always send hide (null) immediately; gently throttle continuous resize drags.
          if (rect === null || now - lastNonNullSentAt >= 33) {
            lastSignature = signature;
            if (rect !== null) {
              lastNonNullSentAt = now;
            }
            void invoke("model_layout", { token, rect }).catch((err) => {
              if (mountedRef.current) {
                setError(errorMessage(err));
              }
            });
          }
        }
      }
      frame = window.requestAnimationFrame(update);
    };

    const onVisibilityChange = () => {
      if (document.hidden) {
        lastSignature = "null";
        lastOccluded = true;
        setOverlayPaused(true);
        hide();
      }
    };

    document.addEventListener("visibilitychange", onVisibilityChange);
    update();
    return () => {
      document.removeEventListener("visibilitychange", onVisibilityChange);
      window.cancelAnimationFrame(frame);
      setOverlayPaused(false);
      hide();
    };
  }, [active, closing, occluded, fatalError, token]);

  const save = useCallback(
    async (thenClose = false) => {
      if (savingRef.current || !status.ready || fatalError) return;
      savingRef.current = true;
      onSaving(true);
      setBusy(true);
      setError(null);
      try {
        const request = await invoke<string>("save_model", { token });
        const until = Date.now() + 120_000;
        while (Date.now() < until) {
          if (!mountedRef.current) return;
          const next = await invoke<Status>("model_status", { token });
          if (next.reply === request) {
            if (mountedRef.current) {
              setStatus(next);
            }
            if (next.error) {
              throw new Error(next.error);
            }
            await onReportSaved(tab, next.saved);
            if (thenClose) {
              await onClose();
            }
            return;
          }
          await new Promise((resolve) => window.setTimeout(resolve, 200));
        }
        throw new Error("Blender has not confirmed the save. The tab is still open.");
      } catch (err) {
        if (mountedRef.current) {
          setError(errorMessage(err));
        }
      } finally {
        savingRef.current = false;
        onSaving(false);
        if (mountedRef.current) {
          setBusy(false);
        }
      }
    },
    [status.ready, fatalError, token, tab, onSaving, onClose, onReportSaved],
  );

  const saveRef = useRef(save);
  saveRef.current = save;

  // Native Blender handles its own shortcuts when focused. Capture Ctrl+S here when the
  // webview holds focus so saving the Blender model never triggers App.tsx scene save.
  useEffect(() => {
    if (!active) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (closing && e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onCancel();
        return;
      }
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.code === "KeyS") {
        e.preventDefault();
        e.stopPropagation();
        if (!e.repeat) {
          void saveRef.current(false);
        }
      }
    };
    window.addEventListener("keydown", onKeyDown, { capture: true });
    return () => window.removeEventListener("keydown", onKeyDown, { capture: true });
  }, [active, closing, onCancel]);

  if (!active) return null;

  const activeError = error || status.error;
  const statusLabel = busy
    ? "Saving…"
    : fatalError
      ? "Exited"
      : !status.ready
        ? "Starting…"
        : status.dirty
          ? "Modified"
          : status.saved > 0
            ? "Saved"
            : "Ready";
  const statusClass = busy
    ? "starting"
    : fatalError
      ? "crashed"
      : !status.ready
        ? "starting"
        : status.dirty
          ? "starting"
          : "running";

  return (
    <div className="model-document">
      <div className="model-actions">
        <span className="model-actions-brand">
          <Box size={13} />
          <strong>Blender</strong>
        </span>
        <span className="truncate model-actions-path" title={path}>
          {path}
        </span>
        <span className={`badge ${statusClass}`}>{statusLabel}</span>
        {!closing && (
          <button
            type="button"
            className="primary"
            disabled={busy || !status.ready || fatalError}
            title="Publish GLB & save Blender source (Ctrl+S)"
            onClick={() => void save(false)}
          >
            <Save size={13} />
            {busy ? "Saving…" : "Save model"}
          </button>
        )}
      </div>

      {activeError && (
        <div className="cb-error-bar model-error-bar" role="alert">
          <span className="model-error-text">{activeError}</span>
          {error && (
            <button
              type="button"
              className="ghost-btn"
              onClick={() => setError(null)}
            >
              Dismiss
            </button>
          )}
        </div>
      )}

      {closing ? (
        <div className="model-close-prompt">
          <div className="model-prompt-card">
            <div className="model-prompt-title">Unsaved model changes</div>
            <p className="muted">
              Save and publish <strong>{path}</strong> to the project before closing this tab?
            </p>
            <div className="model-prompt-actions">
              <button
                type="button"
                className="primary"
                disabled={busy || !status.ready || fatalError}
                onClick={() => void save(true)}
              >
                <Save size={13} />
                {busy ? "Saving…" : "Save and close"}
              </button>
              <button
                type="button"
                className="ghost-btn danger"
                disabled={busy}
                onClick={() =>
                  void onClose().catch((err) => setError(errorMessage(err)))
                }
              >
                Discard changes
              </button>
              <button type="button" disabled={busy} onClick={onCancel}>
                Cancel
              </button>
            </div>
          </div>
        </div>
      ) : (
        <div className="model-native-slot" ref={slot}>
          {fatalError ? (
            <div className="model-slot-card">
              <AlertTriangle size={22} className="model-slot-error-icon" />
              <strong>Blender session stopped</strong>
              <p className="muted">
                {activeError ??
                  "Blender exited unexpectedly. Verify Blender is installed on PATH or configure BEVY9_BLENDER."}
              </p>
              <button
                type="button"
                onClick={() =>
                  void onClose().catch((err) => setError(errorMessage(err)))
                }
              >
                Close tab
              </button>
            </div>
          ) : overlayPaused ? (
            <div className="model-slot-placeholder muted">
              Blender paused while menu or dialog is open
            </div>
          ) : !status.ready ? (
            <div className="model-slot-placeholder muted">
              Starting Blender…
            </div>
          ) : null}
        </div>
      )}
    </div>
  );
}
