import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  Plus,
  RefreshCw,
  Trash2,
} from "lucide-react";
import {
  buildDefaultValueForType,
  isComputedRuntimeComponent,
  isProtectedComponent,
  shortComponentName,
  sortComponentPaths,
} from "../lib/schemaDefaults";
import type { BrpClient } from "../protocol/brpClient";
import type { BrpRegistrySchema, SceneTreeNode } from "../protocol/types";
import { SchemaField } from "./SchemaField";

interface InspectorPanelProps {
  selectedNode: SceneTreeNode | null;
  selectedCount: number;
  brpClient: BrpClient | null;
  registrySchema: BrpRegistrySchema | null;
  inspectorRevision?: number;
  onComponentChanged: (refreshTree: boolean) => void;
  onDeleteEntity: (node: SceneTreeNode) => void;
  onCollapse: () => void;
}

export function InspectorPanel({
  selectedNode,
  selectedCount,
  brpClient,
  registrySchema,
  inspectorRevision = 0,
  onComponentChanged,
  onDeleteEntity,
  onCollapse,
}: InspectorPanelProps) {
  const [showRuntime, setShowRuntime] = useState(false);
  const [collapsedCards, setCollapsedCards] = useState<Set<string>>(
    () => new Set(),
  );
  const [componentValues, setComponentValues] = useState<Record<string, unknown>>(
    {},
  );
  const [componentErrors, setComponentErrors] = useState<Record<string, string>>(
    {},
  );
  const [loading, setLoading] = useState(false);
  const [errorBanner, setErrorBanner] = useState<string | null>(null);
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const [addSearch, setAddSearch] = useState("");

  const fetchTokenRef = useRef(0);
  const pendingTimersRef = useRef<
    Map<
      string,
      {
        timerId: number;
        guid: string;
        typePath: string;
        value: unknown;
        label: string;
        refreshTree: boolean;
      }
    >
  >(new Map());

  const flushPendingWrites = useCallback(() => {
    for (const [key, entry] of pendingTimersRef.current.entries()) {
      window.clearTimeout(entry.timerId);
      pendingTimersRef.current.delete(key);
      if (brpClient) {
        void brpClient.setComponent(
          entry.guid,
          entry.typePath,
          entry.value,
          `${entry.guid}:${entry.typePath}`,
          entry.label,
        );
      }
    }
  }, [brpClient]);

  useEffect(() => {
    return () => {
      flushPendingWrites();
    };
  }, [flushPendingWrites, selectedNode?.guid]);

  const visibleComponentPaths = useMemo(() => {
    if (!selectedNode) {
      return [];
    }
    const filtered = showRuntime
      ? selectedNode.components
      : selectedNode.components.filter(
          (typePath) => !isComputedRuntimeComponent(typePath),
        );
    return sortComponentPaths(filtered);
  }, [selectedNode, showRuntime]);

  const fetchComponentValues = useCallback(async () => {
    if (!selectedNode || !brpClient || visibleComponentPaths.length === 0) {
      setComponentValues({});
      setComponentErrors({});
      return;
    }

    const token = ++fetchTokenRef.current;
    setLoading(true);
    setErrorBanner(null);

    try {
      const response = await brpClient.getComponents(
        selectedNode.entity_bits,
        visibleComponentPaths,
        false,
      );
      if (fetchTokenRef.current !== token) {
        return;
      }

      const raw = response as unknown as Record<string, unknown>;
      if ("components" in raw && raw.components && typeof raw.components === "object") {
        setComponentValues(raw.components as Record<string, unknown>);
        const errMap: Record<string, string> = {};
        if (raw.errors && typeof raw.errors === "object") {
          for (const [k, v] of Object.entries(raw.errors as Record<string, unknown>)) {
            if (v && typeof v === "object" && "message" in v) {
              errMap[k] = String((v as { message: unknown }).message);
            } else {
              errMap[k] = String(v);
            }
          }
        }
        setComponentErrors(errMap);
      } else {
        setComponentValues(raw);
        setComponentErrors({});
      }
    } catch (err) {
      if (fetchTokenRef.current === token) {
        setErrorBanner(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (fetchTokenRef.current === token) {
        setLoading(false);
      }
    }
  }, [selectedNode, brpClient, visibleComponentPaths]);

  useEffect(() => {
    void inspectorRevision;
    void fetchComponentValues();
  }, [fetchComponentValues, inspectorRevision]);

  const addableComponents = useMemo(() => {
    if (!registrySchema || !selectedNode) {
      return [];
    }
    const existing = new Set(selectedNode.components);
    const needle = addSearch.trim().toLowerCase();

    const items = Object.entries(registrySchema)
      .filter(([typePath, def]) => {
        if (existing.has(typePath)) {
          return false;
        }
        const isComponent =
          def.componentInfo !== undefined ||
          Boolean(def.reflectTypes?.includes("Component"));
        if (!isComponent) {
          return false;
        }
        if (isComputedRuntimeComponent(typePath) && !needle) {
          return false;
        }
        if (typePath.includes("<") && !needle) {
          return false;
        }
        if (!needle) {
          return true;
        }
        const shortName = def.shortPath ?? typePath;
        return (
          shortName.toLowerCase().includes(needle) ||
          typePath.toLowerCase().includes(needle)
        );
      })
      .map(([typePath, def]) => ({
        typePath,
        shortPath: def.shortPath ?? shortComponentName(typePath, registrySchema),
        isEngineScene: typePath.startsWith("engine_scene::components::"),
      }));

    items.sort((a, b) => {
      if (a.isEngineScene !== b.isEngineScene) {
        return a.isEngineScene ? -1 : 1;
      }
      return a.shortPath.localeCompare(b.shortPath);
    });
    return items.slice(0, 60);
  }, [registrySchema, selectedNode, addSearch]);

  function handleFieldMutate(
    typePath: string,
    nextValue: unknown,
    immediate = false,
  ) {
    if (!selectedNode || !brpClient) {
      return;
    }

    const guid = selectedNode.guid;
    const title = shortComponentName(typePath, registrySchema);
    const mergeKey = `${guid}:${typePath}`;
    const label = `Edit ${title}`;
    const refreshTree =
      typePath === "bevy_ecs::name::Name" ||
      typePath === "bevy_camera::visibility::Visibility";

    setComponentValues((prev) => ({
      ...prev,
      [typePath]: nextValue,
    }));

    const existingPending = pendingTimersRef.current.get(mergeKey);
    if (existingPending) {
      window.clearTimeout(existingPending.timerId);
      pendingTimersRef.current.delete(mergeKey);
    }

    const executeWrite = async () => {
      try {
        await brpClient.setComponent(
          guid,
          typePath,
          nextValue,
          mergeKey,
          label,
        );
        onComponentChanged(refreshTree);
      } catch (err) {
        setErrorBanner(err instanceof Error ? err.message : String(err));
      }
    };

    if (immediate) {
      void executeWrite();
      return;
    }

    const timerId = window.setTimeout(() => {
      pendingTimersRef.current.delete(mergeKey);
      void executeWrite();
    }, 90);

    pendingTimersRef.current.set(mergeKey, {
      timerId,
      guid,
      typePath,
      value: nextValue,
      label,
      refreshTree,
    });
  }

  async function handleAddComponent(typePath: string) {
    if (!selectedNode || !brpClient) {
      return;
    }
    setAddMenuOpen(false);
    setAddSearch("");
    setErrorBanner(null);
    const defaultValue = buildDefaultValueForType(typePath, registrySchema);
    try {
      await brpClient.addComponent(selectedNode.guid, typePath, defaultValue);
      setComponentValues((prev) => ({
        ...prev,
        [typePath]: defaultValue,
      }));
      onComponentChanged(true);
    } catch (err) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleRemoveComponent(typePath: string) {
    if (!selectedNode || !brpClient || isProtectedComponent(typePath)) {
      return;
    }
    setErrorBanner(null);
    try {
      await brpClient.removeComponent(selectedNode.guid, typePath);
      onComponentChanged(true);
    } catch (err) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    }
  }

  function toggleCardCollapse(typePath: string) {
    setCollapsedCards((prev) => {
      const next = new Set(prev);
      if (next.has(typePath)) {
        next.delete(typePath);
      } else {
        next.add(typePath);
      }
      return next;
    });
  }

  return (
    <div className="panel">
      <div className="panel-header">
        <span>Inspector</span>
        <div className="panel-actions">
          <button
            type="button"
            className={`ghost-btn ${showRuntime ? "active-toggle" : ""}`}
            title="Show internal runtime components"
            onClick={() => setShowRuntime((v) => !v)}
          >
            Runtime
          </button>
          <button
            type="button"
            className="ghost-btn"
            disabled={!selectedNode || !brpClient || loading}
            title="Refresh component values"
            onClick={() => void fetchComponentValues()}
          >
            <RefreshCw size={12} />
          </button>
          <button
            type="button"
            className="ghost-btn"
            title="Collapse Inspector"
            onClick={onCollapse}
          >
            ×
          </button>
        </div>
      </div>

      {!selectedNode ? (
        <div className="panel-body center-empty">
          <div className="muted">
            Select an entity in the World Outliner to inspect its components.
          </div>
        </div>
      ) : (
        <div className="panel-body inspector-body">
          <div className="inspector-entity-card">
            <div className="inspector-entity-top">
              <strong>{selectedNode.name}</strong>
              <button
                type="button"
                className="ghost-btn icon-mini"
                title="Delete entity"
                onClick={() => onDeleteEntity(selectedNode)}
              >
                <Trash2 size={13} />
              </button>
            </div>
            <div className="muted inspector-meta">
              GUID: {selectedNode.guid} · ECS: {selectedNode.entity_bits}
              {selectedCount > 1 ? ` · (${selectedCount} selected)` : ""}
            </div>
          </div>

          {errorBanner && <div className="badge crashed">{errorBanner}</div>}

          <div className="inspector-actions-bar">
            <div className="popover-anchor" style={{ width: "100%" }}>
              <button
                type="button"
                className="primary"
                style={{ width: "100%", justifyContent: "center" }}
                disabled={!registrySchema || !brpClient}
                onClick={() => setAddMenuOpen((open) => !open)}
              >
                <Plus size={13} />
                Add Component
              </button>

              {addMenuOpen && (
                <div className="popover-card add-component-popover">
                  <input
                    type="text"
                    className="text-input"
                    value={addSearch}
                    onChange={(e) => setAddSearch(e.target.value)}
                    placeholder="Search component type..."
                    autoFocus
                  />
                  <div className="add-component-list">
                    {addableComponents.length === 0 ? (
                      <div className="muted">No matching components</div>
                    ) : (
                      addableComponents.map((item) => (
                        <button
                          key={item.typePath}
                          type="button"
                          className="ghost-btn preset-btn"
                          title={item.typePath}
                          onClick={() => void handleAddComponent(item.typePath)}
                        >
                          <span>{item.shortPath}</span>
                          {item.isEngineScene && (
                            <span className="badge">Scene</span>
                          )}
                        </button>
                      ))
                    )}
                  </div>
                </div>
              )}
            </div>
          </div>

          {visibleComponentPaths.map((typePath) => {
            const collapsed = collapsedCards.has(typePath);
            const title = shortComponentName(typePath, registrySchema);
            const canRemove = !isProtectedComponent(typePath);
            const compErr = componentErrors[typePath];
            const hasValue = typePath in componentValues;

            return (
              <section key={typePath} className="component-card">
                <div
                  className="component-card-header"
                  title={typePath}
                  onClick={() => toggleCardCollapse(typePath)}
                >
                  <span className="component-card-title">
                    {collapsed ? (
                      <ChevronRight size={13} />
                    ) : (
                      <ChevronDown size={13} />
                    )}
                    <span>{title}</span>
                  </span>

                  {canRemove && (
                    <button
                      type="button"
                      className="ghost-btn icon-mini"
                      title={`Remove ${title}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        void handleRemoveComponent(typePath);
                      }}
                    >
                      ×
                    </button>
                  )}
                </div>

                {!collapsed && (
                  <div className="component-card-body">
                    {compErr ? (
                      <div className="muted" style={{ fontSize: 11 }}>
                        Read-only / not BRP-serializable: {compErr}
                      </div>
                    ) : !hasValue ? (
                      <div className="muted" style={{ fontSize: 11 }}>
                        {loading ? "Loading component..." : "No serialized data"}
                      </div>
                    ) : (
                      <SchemaField
                        typePath={typePath}
                        value={componentValues[typePath]}
                        schema={registrySchema}
                        onChange={(nextVal, immediate) =>
                          handleFieldMutate(typePath, nextVal, immediate)
                        }
                      />
                    )}
                  </div>
                )}
              </section>
            );
          })}
        </div>
      )}
    </div>
  );
}
