import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  Package,
  PackagePlus,
  Play,
  Plus,
  RefreshCw,
  RotateCcw,
  Square,
  Trash2,
} from "lucide-react";
import {
  buildDefaultValueForType,
  getComponentBadgeCategory,
  isComputedRuntimeComponent,
  isHiddenEditorComponent,
  isProtectedComponent,
  shortComponentName,
  sortComponentPaths,
} from "../lib/schemaDefaults";
import type { BrpClient } from "../protocol/brpClient";
import type {
  AssetRecord,
  BrpRegistrySchema,
  PieState,
  PrefabInstanceData,
  PrefabMemberData,
  SceneTreeNode,
} from "../protocol/types";
import { SchemaField, type SceneEntityOption } from "./SchemaField";

interface InspectorPanelProps {
  selectedNode: SceneTreeNode | null;
  prefabRootNode: SceneTreeNode | null;
  selectedCount: number;
  brpClient: BrpClient | null;
  registrySchema: BrpRegistrySchema | null;
  assets?: AssetRecord[];
  sceneEntities?: SceneEntityOption[];
  inspectorRevision?: number;
  pieState?: PieState;
  onComponentChanged: (refreshTree: boolean) => void;
  onDeleteEntity: (node: SceneTreeNode) => void;
  onSaveAsPrefab: (node: SceneTreeNode) => void;
  onApplyPrefab: (node: SceneTreeNode) => void;
  onRevertPrefab: (node: SceneTreeNode) => void;
  onRevertPrefabComponent: (node: SceneTreeNode, typePath: string) => void;
  onCollapse: () => void;
}

const PREFAB_INSTANCE_PATH = "engine_scene::prefab::PrefabInstance";
const PREFAB_MEMBER_PATH = "engine_scene::prefab::PrefabMember";

export function InspectorPanel({
  selectedNode,
  prefabRootNode,
  selectedCount,
  brpClient,
  registrySchema,
  assets,
  sceneEntities,
  inspectorRevision = 0,
  pieState = "editing",
  onComponentChanged,
  onDeleteEntity,
  onSaveAsPrefab,
  onApplyPrefab,
  onRevertPrefab,
  onRevertPrefabComponent,
  onCollapse,
}: InspectorPanelProps) {
  const pieActive = pieState !== "editing";
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
  const [prefabInstance, setPrefabInstance] = useState<PrefabInstanceData | null>(
    null,
  );
  const [prefabMember, setPrefabMember] = useState<PrefabMemberData | null>(null);
  const [loading, setLoading] = useState(false);
  const [errorBanner, setErrorBanner] = useState<string | null>(null);
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const [addSearch, setAddSearch] = useState("");

  const fetchTokenRef = useRef(0);
  const inspectorBodyRef = useRef<HTMLDivElement | null>(null);
  const addMenuAnchorRef = useRef<HTMLDivElement | null>(null);
  const addMenuListRef = useRef<HTMLDivElement | null>(null);
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

  const flushPendingWrites = useCallback(async () => {
    const writes: Array<Promise<unknown>> = [];
    for (const [key, entry] of pendingTimersRef.current.entries()) {
      window.clearTimeout(entry.timerId);
      pendingTimersRef.current.delete(key);
      if (brpClient) {
        writes.push(
          brpClient.setComponent(
            entry.guid,
            entry.typePath,
            entry.value,
            `${entry.guid}:${entry.typePath}`,
            entry.label,
          ),
        );
      }
    }
    if (writes.length > 0) {
      await Promise.allSettled(writes);
    }
  }, [brpClient]);

  useEffect(() => {
    setAddMenuOpen(false);
    setAddSearch("");
    return () => {
      void flushPendingWrites();
    };
  }, [flushPendingWrites, selectedNode?.guid]);

  useEffect(() => {
    if (!addMenuOpen) {
      return;
    }

    const closeMenu = () => {
      setAddMenuOpen(false);
      setAddSearch("");
    };

    const handleOutsidePointerDown = (event: PointerEvent) => {
      if (
        addMenuAnchorRef.current &&
        event.target instanceof Node &&
        !addMenuAnchorRef.current.contains(event.target)
      ) {
        closeMenu();
      }
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        closeMenu();
      }
    };

    const handleInspectorScroll = () => {
      closeMenu();
    };

    const handleWindowWheel = (event: WheelEvent) => {
      if (
        event.target instanceof Node &&
        addMenuListRef.current?.contains(event.target)
      ) {
        return;
      }
      closeMenu();
    };

    const handleAnyScroll = (event: Event) => {
      if (
        event.target === addMenuListRef.current ||
        (event.target instanceof Node &&
          addMenuListRef.current?.contains(event.target))
      ) {
        return;
      }
      closeMenu();
    };

    const inspectorBody = inspectorBodyRef.current;
    window.addEventListener("pointerdown", handleOutsidePointerDown, true);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("wheel", handleWindowWheel, {
      capture: true,
      passive: true,
    });
    window.addEventListener("scroll", handleAnyScroll, {
      capture: true,
      passive: true,
    });
    inspectorBody?.addEventListener("scroll", handleInspectorScroll, {
      passive: true,
    });

    return () => {
      window.removeEventListener("pointerdown", handleOutsidePointerDown, true);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("wheel", handleWindowWheel, true);
      window.removeEventListener("scroll", handleAnyScroll, true);
      inspectorBody?.removeEventListener("scroll", handleInspectorScroll);
    };
  }, [addMenuOpen]);

  const visibleComponentPaths = useMemo(() => {
    if (!selectedNode) {
      return [];
    }
    const base = selectedNode.components.filter(
      (typePath) => !isHiddenEditorComponent(typePath),
    );
    const filtered = showRuntime
      ? base
      : base.filter((typePath) => !isComputedRuntimeComponent(typePath));
    return sortComponentPaths(filtered);
  }, [selectedNode, showRuntime]);

  const fetchPrefabMeta = useCallback(
    async (token: number) => {
      if (!selectedNode || !brpClient || !prefabRootNode) {
        if (fetchTokenRef.current === token) {
          setPrefabInstance(null);
          setPrefabMember(null);
        }
        return;
      }

      try {
        const [instRes, memberRes] = await Promise.all([
          brpClient.getComponents(
            prefabRootNode.entity_bits,
            [PREFAB_INSTANCE_PATH],
            false,
          ),
          selectedNode.is_prefab_member
            ? brpClient.getComponents(
                selectedNode.entity_bits,
                [PREFAB_MEMBER_PATH],
                false,
              )
            : Promise.resolve(null),
        ]);
        if (fetchTokenRef.current !== token) {
          return;
        }
        const instMap =
          (instRes as { components?: Record<string, unknown> })?.components ??
          (instRes as unknown as Record<string, unknown>);
        const memberMap = memberRes
          ? ((memberRes as { components?: Record<string, unknown> })?.components ??
            (memberRes as unknown as Record<string, unknown>))
          : null;
        setPrefabInstance(
          (instMap?.[PREFAB_INSTANCE_PATH] as PrefabInstanceData | undefined) ??
            null,
        );
        setPrefabMember(
          (memberMap?.[PREFAB_MEMBER_PATH] as PrefabMemberData | undefined) ??
            null,
        );
      } catch {
        if (fetchTokenRef.current === token) {
          setPrefabInstance(null);
          setPrefabMember(null);
        }
      }
    },
    [selectedNode, prefabRootNode, brpClient],
  );

  const fetchComponentValues = useCallback(async () => {
    if (!selectedNode || !brpClient || visibleComponentPaths.length === 0) {
      setComponentValues({});
      setComponentErrors({});
      setPrefabInstance(null);
      setPrefabMember(null);
      return;
    }

    const token = ++fetchTokenRef.current;
    setLoading(true);
    setErrorBanner(null);

    try {
      const [response] = await Promise.all([
        brpClient.getComponents(
          selectedNode.entity_bits,
          visibleComponentPaths,
          false,
        ),
        fetchPrefabMeta(token),
      ]);
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
  }, [selectedNode, brpClient, visibleComponentPaths, fetchPrefabMeta]);

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
        if (existing.has(typePath) || isHiddenEditorComponent(typePath)) {
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
      .map(([typePath, def]) => {
        const badgeCategory = getComponentBadgeCategory(typePath);
        return {
          typePath,
          shortPath: def.shortPath ?? shortComponentName(typePath, registrySchema),
          badgeCategory,
          isEngineScene: badgeCategory !== null,
        };
      });

    items.sort((a, b) => {
      if (a.isEngineScene !== b.isEngineScene) {
        return a.isEngineScene ? -1 : 1;
      }
      return a.shortPath.localeCompare(b.shortPath);
    });
    return items.slice(0, 60);
  }, [registrySchema, selectedNode, addSearch]);

  async function previewEmitter(guid: string | null) {
    if (!brpClient) return;
    setErrorBanner(null);
    try {
      await flushPendingWrites();
      if (guid) await brpClient.call("engine.audio.preview_emitter", { guid });
      else await brpClient.call("engine.audio.preview", { uuid: null });
    } catch (error) {
      setErrorBanner(String(error));
    }
  }

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
        if (prefabRootNode) {
          void fetchPrefabMeta(fetchTokenRef.current);
        }
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
      if (prefabRootNode) {
        void fetchPrefabMeta(fetchTokenRef.current);
      }
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
      if (prefabRootNode) {
        void fetchPrefabMeta(fetchTokenRef.current);
      }
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

      {pieActive && (
        <div
          className={`inspector-pie-banner ${pieState === "playing" ? "playing" : "paused"}`}
          role="status"
        >
          <strong>PIE {pieState === "playing" ? "Playing" : "Paused"}:</strong>{" "}
          Runtime component edits are temporary and revert on Stop.
        </div>
      )}

      {!selectedNode ? (
        <div className="panel-body center-empty">
          <div className="muted">
            Select an entity in the World Outliner to inspect its components.
          </div>
        </div>
      ) : (
        <div ref={inspectorBodyRef} className="panel-body inspector-body">
          <div className="inspector-entity-card">
            <div className="inspector-entity-top">
              <strong className="inspector-entity-name">{selectedNode.name}</strong>
              <div className="panel-actions">
                {!prefabRootNode && (
                  <button
                    type="button"
                    className="ghost-btn icon-mini"
                    disabled={pieActive}
                    title={
                      pieActive
                        ? "Saving prefabs is disabled during Play-In-Editor"
                        : "Save subtree as .prefab.ron"
                    }
                    onClick={() => onSaveAsPrefab(selectedNode)}
                  >
                    <PackagePlus size={13} />
                  </button>
                )}
                <button
                  type="button"
                  className="ghost-btn icon-mini"
                  title="Delete entity"
                  onClick={() => onDeleteEntity(selectedNode)}
                >
                  <Trash2 size={13} />
                </button>
              </div>
            </div>
            {selectedCount > 1 && (
              <div className="muted inspector-meta">{selectedCount} selected</div>
            )}
          </div>

          {prefabRootNode && (
            <div className="prefab-banner">
              <div className="prefab-banner-header">
                <span className="prefab-banner-title">
                  <Package size={13} />
                  <span>
                    {prefabInstance?.prefab_path ??
                      prefabRootNode.prefab_path ??
                      "Prefab Instance"}
                  </span>
                </span>
                <span className="badge">
                  {Object.keys(prefabInstance?.overrides ?? {}).length} overrides
                </span>
              </div>
              <div className="prefab-banner-actions">
                <button
                  type="button"
                  className="primary"
                  disabled={!brpClient || pieActive}
                  title={
                    pieActive
                      ? "Applying changes to prefab assets is disabled during Play-In-Editor"
                      : "Write instance changes back to .prefab.ron and propagate to sibling instances"
                  }
                  onClick={() => onApplyPrefab(selectedNode)}
                >
                  Apply to Prefab
                </button>
                <button
                  type="button"
                  disabled={
                    !brpClient ||
                    Object.keys(prefabInstance?.overrides ?? {}).length === 0
                  }
                  title="Revert all overrides on this prefab instance back to template"
                  onClick={() => onRevertPrefab(selectedNode)}
                >
                  <RotateCcw size={12} /> Revert All
                </button>
              </div>
            </div>
          )}

          {errorBanner && <div className="badge crashed">{errorBanner}</div>}

          <div className="inspector-actions-bar">
            <div
              ref={addMenuAnchorRef}
              className="popover-anchor"
              style={{ width: "100%" }}
            >
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
                  <div ref={addMenuListRef} className="add-component-list">
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
                          {item.badgeCategory && (
                            <span
                              className={`badge badge-${item.badgeCategory.toLowerCase()}`}
                            >
                              {item.badgeCategory}
                            </span>
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

            const activeSourceGuid =
              prefabMember?.source_guid ?? prefabInstance?.root_source_guid ?? null;
            const overridesMap = prefabInstance?.overrides ?? {};
            const modifiedFields: string[] = [];
            if (activeSourceGuid) {
              const compKey = `${activeSourceGuid}::${typePath}`;
              if (compKey in overridesMap) {
                modifiedFields.push("entire component");
              } else {
                const fieldPrefix = `${compKey}::`;
                for (const ovKey of Object.keys(overridesMap)) {
                  if (ovKey.startsWith(fieldPrefix)) {
                    modifiedFields.push(ovKey.slice(fieldPrefix.length));
                  }
                }
              }
            }
            const isOverridden = modifiedFields.length > 0;

            return (
              <section
                key={typePath}
                className={`component-card ${isOverridden ? "prefab-override" : ""}`}
              >
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
                    {isOverridden && (
                      <span
                        className="override-badge"
                        title={`Modified from prefab: ${modifiedFields.join(", ")}`}
                      >
                        Modified
                      </span>
                    )}
                  </span>

                  <div className="panel-actions">
                    {isOverridden && (
                      <button
                        type="button"
                        className="ghost-btn icon-mini"
                        title={`Revert ${title} to prefab template`}
                        onClick={(e) => {
                          e.stopPropagation();
                          onRevertPrefabComponent(selectedNode, typePath);
                        }}
                      >
                        <RotateCcw size={11} />
                      </button>
                    )}
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
                </div>

                {!collapsed && (
                  <div className="component-card-body">
                    {typePath === "engine_audio::AudioEmitter" && brpClient && (
                      <div className="audio-preview-bar">
                        <button
                          type="button"
                          className="primary"
                          onClick={() => void previewEmitter(selectedNode.guid)}
                        >
                          <Play size={12} /> Preview emitter
                        </button>
                        <button
                          type="button"
                          onClick={() => void previewEmitter(null)}
                        >
                          <Square size={11} /> Stop
                        </button>
                      </div>
                    )}
                    {compErr ? (
                      <div className="muted">
                        Read-only / not BRP-serializable: {compErr}
                      </div>
                    ) : !hasValue ? (
                      <div className="muted">
                        {loading ? "Loading component..." : "No serialized data"}
                      </div>
                    ) : (
                      <SchemaField
                        typePath={typePath}
                        value={componentValues[typePath]}
                        schema={registrySchema}
                        assets={assets}
                        sceneEntities={sceneEntities}
                        currentEntityGuid={selectedNode.guid}
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
