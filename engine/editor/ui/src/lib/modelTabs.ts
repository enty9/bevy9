import { invoke, isTauri } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import type { AssetRecord } from "../protocol/types";

export interface ModelTab {
  token: string;
  uuid: string;
  path: string;
  project: string;
}

export function isEditableModel(asset: AssetRecord): boolean {
  return asset.kind === "gltf_model" && asset.relative_path.toLowerCase().endsWith(".glb");
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export function useModelTabs(project: string | null, onError: (error: string) => void) {
  const [tabs, setTabs] = useState<ModelTab[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const projectRef = useRef<string | null>(project);
  projectRef.current = project;
  const openedTokensRef = useRef<Map<string, { project: string; uuid: string }>>(new Map());

  // Immediately unmap any native Blender child windows on webview mount/reload
  // so none linger over the Bevy viewport while UI state initializes.
  useEffect(() => {
    if (!isTauri()) return;
    void invoke("hide_model_tabs").catch(() => {});
  }, []);

  // Reconcile active selection purely whenever the tab list changes.
  useEffect(() => {
    setActive((current) =>
      current && tabs.some((tab) => tab.token === current) ? current : null,
    );
  }, [tabs]);

  useEffect(() => {
    openedTokensRef.current.clear();

    if (!isTauri()) {
      setTabs([]);
      setActive(null);
      return;
    }

    if (!project) {
      setTabs([]);
      setActive(null);
      void invoke("hide_model_tabs").catch(() => {});
      return;
    }

    // Drop any tabs belonging to another project immediately.
    setTabs((prev) => {
      const scoped = prev.filter((tab) => tab.project === project);
      return scoped.length === prev.length ? prev : scoped;
    });
    setActive(null);

    let live = true;
    // Reconnect to native documents on a webview reload, instead of abandoning them.
    void invoke<ModelTab[]>("list_model_tabs")
      .then((found) => {
        if (!live) return;
        const forProject = found.filter((tab) => tab.project === project);
        const openedSnapshot = new Set(openedTokensRef.current.keys());
        setTabs((prev) => {
          const merged = new Map<string, ModelTab>();
          const byUuid = new Map<string, string>();
          for (const tab of forProject) {
            merged.set(tab.token, tab);
            byUuid.set(tab.uuid, tab.token);
          }
          for (const tab of prev) {
            if (tab.project === project && openedSnapshot.has(tab.token)) {
              const existingToken = byUuid.get(tab.uuid);
              if (existingToken && existingToken !== tab.token) {
                merged.delete(existingToken);
              }
              merged.set(tab.token, tab);
              byUuid.set(tab.uuid, tab.token);
            }
          }
          return [...merged.values()];
        });
        for (const tab of found) {
          if (tab.project !== project) {
            void invoke("model_layout", { token: tab.token, rect: null }).catch(() => {});
          }
        }
      })
      .catch((err) => {
        if (live) onError(errorMessage(err));
      });

    return () => {
      live = false;
    };
  }, [project, onError]);

  const open = useCallback(
    async (asset: AssetRecord) => {
      const targetProject = projectRef.current;
      if (!targetProject || !isTauri()) {
        throw new Error("Model editing requires the desktop editor with an active project");
      }
      const token = await invoke<string>("open_model", {
        project: targetProject,
        path: asset.relative_path,
        uuid: asset.uuid,
      });
      if (projectRef.current !== targetProject) {
        // Active project changed while open_model was in flight; ensure the window stays hidden.
        void invoke("model_layout", { token, rect: null }).catch(() => {});
        return;
      }
      for (const [oldToken, meta] of openedTokensRef.current.entries()) {
        if (
          oldToken !== token &&
          meta.project === targetProject &&
          meta.uuid === asset.uuid
        ) {
          openedTokensRef.current.delete(oldToken);
        }
      }
      openedTokensRef.current.set(token, { project: targetProject, uuid: asset.uuid });
      setTabs((prev) => {
        if (prev.some((t) => t.token === token)) {
          return prev;
        }
        // Replace any stale/dead tab for the same model so reopening never leaves a ghost tab.
        const withoutStale = prev.filter(
          (existing) => !(existing.project === targetProject && existing.uuid === asset.uuid),
        );
        return [
          ...withoutStale,
          { project: targetProject, token, uuid: asset.uuid, path: asset.relative_path },
        ];
      });
      setActive(token);
    },
    [],
  );

  const close = useCallback(async (token: string) => {
    openedTokensRef.current.delete(token);
    await invoke("close_model", { token });
    setTabs((prev) => prev.filter((tab) => tab.token !== token));
    setActive((prev) => (prev === token ? null : prev));
  }, []);

  const hideAll = useCallback(() => {
    if (!isTauri()) return;
    void invoke("hide_model_tabs").catch(() => {});
  }, []);

  return { tabs, active, setActive, open, close, hideAll };
}
