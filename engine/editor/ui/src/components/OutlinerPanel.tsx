import {
  useMemo,
  useState,
  type DragEvent as ReactDragEvent,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import {
  ChevronDown,
  ChevronRight,
  Eye,
  EyeOff,
  FolderOpen,
  Package,
  Pencil,
  Plus,
  Search,
  Trash2,
} from "lucide-react";
import type {
  EntityPresetKind,
  PrefabFileEntry,
  SceneTreeNode,
} from "../protocol/types";

interface OutlinerPanelProps {
  roots: SceneTreeNode[];
  selectedGuids: string[];
  prefabs: PrefabFileEntry[];
  connected: boolean;
  pieActive?: boolean;
  onSelect: (guids: string[]) => void;
  onToggleVisibility: (node: SceneTreeNode) => void;
  onSpawnPreset: (preset: EntityPresetKind, parentGuid: string | null) => void;
  onSpawnPrefab: (path: string, parentGuid: string | null) => void;
  onBrowseSpawnPrefab: (parentGuid: string | null) => void;
  onRename: (node: SceneTreeNode, newName: string) => void;
  onDelete: (node: SceneTreeNode) => void;
  onReparent: (guid: string, parentGuid: string | null) => void;
  onCollapse: () => void;
}

interface PresetGroup {
  label: string;
  items: Array<{ kind: EntityPresetKind; label: string }>;
}

const PRESET_GROUPS: PresetGroup[] = [
  {
    label: "General",
    items: [{ kind: "empty", label: "Empty Entity" }],
  },
  {
    label: "3D",
    items: [
      { kind: "cube3d", label: "Cube 3D" },
      { kind: "sphere3d", label: "Sphere 3D" },
      { kind: "plane3d", label: "Plane 3D" },
      { kind: "directional_light", label: "Directional Light" },
      { kind: "point_light", label: "Point Light" },
      { kind: "camera3d", label: "Camera 3D" },
    ],
  },
  {
    label: "2D",
    items: [
      { kind: "sprite2d", label: "Sprite 2D" },
      { kind: "quad2d", label: "Quad 2D" },
      { kind: "circle2d", label: "Circle 2D" },
      { kind: "camera2d", label: "Camera 2D" },
    ],
  },
];

function findNode(roots: SceneTreeNode[], guid: string): SceneTreeNode | null {
  for (const node of roots) {
    if (node.guid === guid) {
      return node;
    }
    const found = findNode(node.children, guid);
    if (found) {
      return found;
    }
  }
  return null;
}

function subtreeContainsGuid(node: SceneTreeNode, guid: string): boolean {
  if (node.guid === guid) {
    return true;
  }
  return node.children.some((child) => subtreeContainsGuid(child, guid));
}

function isDescendant(
  roots: SceneTreeNode[],
  ancestorGuid: string,
  candidateGuid: string,
): boolean {
  const ancestor = findNode(roots, ancestorGuid);
  if (!ancestor) {
    return false;
  }
  return ancestor.children.some((child) => subtreeContainsGuid(child, candidateGuid));
}

function filterTree(nodes: SceneTreeNode[], query: string): SceneTreeNode[] {
  if (!query) {
    return nodes;
  }
  const needle = query.toLowerCase();
  const out: SceneTreeNode[] = [];
  for (const node of nodes) {
    const filteredChildren = filterTree(node.children, query);
    if (
      node.name.toLowerCase().includes(needle) ||
      filteredChildren.length > 0
    ) {
      out.push({
        ...node,
        children: filteredChildren,
      });
    }
  }
  return out;
}

export function OutlinerPanel({
  roots,
  selectedGuids,
  prefabs,
  connected,
  pieActive = false,
  onSelect,
  onToggleVisibility,
  onSpawnPreset,
  onSpawnPrefab,
  onBrowseSpawnPrefab,
  onRename,
  onDelete,
  onReparent,
  onCollapse,
}: OutlinerPanelProps) {
  const [search, setSearch] = useState("");
  const [collapsedGuids, setCollapsedGuids] = useState<Set<string>>(
    () => new Set(),
  );
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const [spawnUnderSelected, setSpawnUnderSelected] = useState(true);
  const [editingGuid, setEditingGuid] = useState<string | null>(null);
  const [editingDraft, setEditingDraft] = useState("");
  const [draggedNode, setDraggedNode] = useState<{
    guid: string;
    entityBits: number;
    hasParent: boolean;
  } | null>(null);
  const [dragOverGuid, setDragOverGuid] = useState<string | "ROOT" | null>(null);

  const selectedSet = useMemo(() => new Set(selectedGuids), [selectedGuids]);
  const normalizedSearch = search.trim();
  const visibleRoots = useMemo(
    () => filterTree(roots, normalizedSearch),
    [roots, normalizedSearch],
  );

  function toggleExpand(guid: string) {
    setCollapsedGuids((prev) => {
      const next = new Set(prev);
      if (next.has(guid)) {
        next.delete(guid);
      } else {
        next.add(guid);
      }
      return next;
    });
  }

  function startRename(node: SceneTreeNode) {
    setEditingGuid(node.guid);
    setEditingDraft(node.name);
  }

  function commitRename(node: SceneTreeNode) {
    const trimmed = editingDraft.trim();
    setEditingGuid(null);
    if (trimmed.length > 0 && trimmed !== node.name) {
      onRename(node, trimmed);
    }
  }

  function cancelRename() {
    setEditingGuid(null);
  }

  function handleContainerKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
    if (editingGuid !== null || pieActive) {
      return;
    }
    if (event.key === "Delete" && selectedGuids.length > 0 && connected) {
      const target = findNode(roots, selectedGuids[0]);
      if (target) {
        event.preventDefault();
        onDelete(target);
      }
    }
  }

  function canDropOnNode(target: SceneTreeNode): boolean {
    if (!draggedNode) {
      return false;
    }
    if (draggedNode.guid === target.guid) {
      return false;
    }
    if (target.guid === findNode(roots, draggedNode.guid)?.parent_guid) {
      return false;
    }
    return !isDescendant(roots, draggedNode.guid, target.guid);
  }

  function handleRowDragStart(
    event: ReactDragEvent<HTMLDivElement>,
    node: SceneTreeNode,
  ) {
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", node.guid);
    setDraggedNode({
      guid: node.guid,
      entityBits: node.entity_bits,
      hasParent: node.parent_guid !== null,
    });
  }

  function handleRowDragEnd() {
    setDraggedNode(null);
    setDragOverGuid(null);
  }

  function renderNode(node: SceneTreeNode, depth: number) {
    const hasChildren = node.children.length > 0;
    const isExpanded =
      normalizedSearch.length > 0 || !collapsedGuids.has(node.guid);
    const isSelected = selectedSet.has(node.guid);
    const isEditing = editingGuid === node.guid;
    const isDragTarget = dragOverGuid === node.guid;

    return (
      <div key={node.guid}>
        <div
          className={`outliner-row ${isSelected ? "selected" : ""} ${
            isDragTarget ? "drag-over" : ""
          } ${node.is_prefab_member ? "outliner-prefab" : ""} ${!node.visible ? "dimmed" : ""}`}
          style={{ paddingLeft: 6 + depth * 14 }}
          draggable={!isEditing && connected}
          onDragStart={(e) => handleRowDragStart(e, node)}
          onDragEnd={handleRowDragEnd}
          onDragOver={(e) => {
            if (canDropOnNode(node)) {
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              if (dragOverGuid !== node.guid) {
                setDragOverGuid(node.guid);
              }
            }
          }}
          onDragLeave={() => {
            if (dragOverGuid === node.guid) {
              setDragOverGuid(null);
            }
          }}
          onDrop={(e) => {
            e.preventDefault();
            if (draggedNode && canDropOnNode(node)) {
              onReparent(draggedNode.guid, node.guid);
            }
            setDraggedNode(null);
            setDragOverGuid(null);
          }}
          onClick={(e) => {
            if (isEditing) {
              return;
            }
            if (e.ctrlKey || e.metaKey) {
              if (isSelected) {
                onSelect(selectedGuids.filter((g) => g !== node.guid));
              } else {
                onSelect([...selectedGuids, node.guid]);
              }
            } else {
              onSelect([node.guid]);
            }
          }}
          onDoubleClick={() => {
            if (connected) {
              startRename(node);
            }
          }}
        >
          <button
            type="button"
            className="outliner-chevron"
            disabled={!hasChildren}
            tabIndex={-1}
            onClick={(e) => {
              e.stopPropagation();
              if (hasChildren) {
                toggleExpand(node.guid);
              }
            }}
          >
            {hasChildren ? (
              isExpanded ? (
                <ChevronDown size={12} />
              ) : (
                <ChevronRight size={12} />
              )
            ) : (
              <span style={{ width: 12, display: "inline-block" }} />
            )}
          </button>

          {isEditing ? (
            <input
              type="text"
              className="text-input outliner-rename-input"
              value={editingDraft}
              autoFocus
              onClick={(e) => e.stopPropagation()}
              onChange={(e) => setEditingDraft(e.target.value)}
              onBlur={() => commitRename(node)}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") {
                  commitRename(node);
                } else if (e.key === "Escape") {
                  cancelRename();
                }
              }}
            />
          ) : (
            <span
              className="outliner-name"
              title={
                node.prefab_path
                  ? `${node.guid} · Prefab: ${node.prefab_path}`
                  : node.guid
              }
            >
              {node.prefab_path && (
                <Package
                  size={12}
                  style={{ marginRight: 5, verticalAlign: "-1px", flexShrink: 0 }}
                />
              )}
              {node.name}
            </span>
          )}

          <div className="outliner-row-actions">
            <button
              type="button"
              className="ghost-btn icon-mini"
              title="Rename entity"
              disabled={!connected}
              onClick={(e) => {
                e.stopPropagation();
                startRename(node);
              }}
            >
              <Pencil size={11} />
            </button>
            <button
              type="button"
              className="ghost-btn icon-mini"
              title={node.visible ? "Hide entity" : "Show entity"}
              disabled={!connected}
              onClick={(e) => {
                e.stopPropagation();
                onToggleVisibility(node);
              }}
            >
              {node.visible ? <Eye size={12} /> : <EyeOff size={12} />}
            </button>
            <button
              type="button"
              className="ghost-btn icon-mini"
              title="Delete entity"
              disabled={!connected}
              onClick={(e) => {
                e.stopPropagation();
                onDelete(node);
              }}
            >
              <Trash2 size={11} />
            </button>
          </div>
        </div>

        {hasChildren &&
          isExpanded &&
          node.children.map((child) => renderNode(child, depth + 1))}
      </div>
    );
  }

  return (
    <div className="panel" tabIndex={-1} onKeyDown={handleContainerKeyDown}>
      <div className="panel-header">
        <span>World Outliner</span>
        <div className="panel-actions">
          <div className="popover-anchor">
            <button
              type="button"
              className="ghost-btn"
              disabled={!connected}
              title="Add Entity"
              onClick={() => setAddMenuOpen((open) => !open)}
            >
              <Plus size={13} />
              Add
            </button>

            {addMenuOpen && (
              <div className="popover-card outliner-add-menu">
                <label className="checkbox-row">
                  <input
                    type="checkbox"
                    checked={spawnUnderSelected && selectedGuids.length > 0}
                    disabled={selectedGuids.length === 0}
                    onChange={(e) => setSpawnUnderSelected(e.target.checked)}
                  />
                  <span>Spawn under selected</span>
                </label>

                {PRESET_GROUPS.map((group) => (
                  <div key={group.label} className="preset-group">
                    <div className="preset-group-label">{group.label}</div>
                    {group.items.map((item) => (
                      <button
                        key={item.kind}
                        type="button"
                        className="ghost-btn preset-btn"
                        onClick={() => {
                          setAddMenuOpen(false);
                          const parentGuid =
                            spawnUnderSelected && selectedGuids.length > 0
                              ? selectedGuids[0]
                              : null;
                          onSpawnPreset(item.kind, parentGuid);
                        }}
                      >
                        {item.label}
                      </button>
                    ))}
                  </div>
                ))}

                <div className="preset-group">
                  <div className="preset-group-label">Prefabs</div>
                  {prefabs.map((prefab) => (
                    <button
                      key={prefab.relative_path}
                      type="button"
                      className="ghost-btn preset-btn"
                      title={prefab.relative_path}
                      onClick={() => {
                        setAddMenuOpen(false);
                        const parentGuid =
                          spawnUnderSelected && selectedGuids.length > 0
                            ? selectedGuids[0]
                            : null;
                        onSpawnPrefab(prefab.relative_path, parentGuid);
                      }}
                    >
                      <Package size={12} /> {prefab.name}
                    </button>
                  ))}
                  <button
                    type="button"
                    className="ghost-btn preset-btn"
                    onClick={() => {
                      setAddMenuOpen(false);
                      const parentGuid =
                        spawnUnderSelected && selectedGuids.length > 0
                          ? selectedGuids[0]
                          : null;
                      onBrowseSpawnPrefab(parentGuid);
                    }}
                  >
                    <FolderOpen size={12} /> Browse .prefab.ron...
                  </button>
                </div>
              </div>
            )}
          </div>

          <button
            type="button"
            className="ghost-btn"
            title="Collapse World Outliner"
            onClick={onCollapse}
          >
            ×
          </button>
        </div>
      </div>

      <div className="outliner-search-wrap">
        <Search size={13} className="muted" />
        <input
          type="text"
          className="outliner-search-input"
          value={search}
          placeholder="Search entities..."
          onChange={(e) => setSearch(e.target.value)}
        />
        {search && (
          <button
            type="button"
            className="ghost-btn"
            onClick={() => setSearch("")}
          >
            ×
          </button>
        )}
      </div>

      {draggedNode?.hasParent && (
        <div
          className={`outliner-root-drop ${
            dragOverGuid === "ROOT" ? "drag-over" : ""
          }`}
          onDragOver={(e) => {
            e.preventDefault();
            e.dataTransfer.dropEffect = "move";
            if (dragOverGuid !== "ROOT") {
              setDragOverGuid("ROOT");
            }
          }}
          onDragLeave={() => {
            if (dragOverGuid === "ROOT") {
              setDragOverGuid(null);
            }
          }}
          onDrop={(e) => {
            e.preventDefault();
            if (draggedNode) {
              onReparent(draggedNode.guid, null);
            }
            setDraggedNode(null);
            setDragOverGuid(null);
          }}
        >
          Drop here to Move to Scene Root
        </div>
      )}

      <div className="panel-body outliner-tree">
        {visibleRoots.length === 0 ? (
          <div className="muted" style={{ padding: 6 }}>
            {roots.length === 0
              ? "Scene is empty. Click + Add to spawn an entity."
              : "No entities match the search filter."}
          </div>
        ) : (
          visibleRoots.map((root) => renderNode(root, 0))
        )}
      </div>
    </div>
  );
}
