import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type DragEvent as ReactDragEvent,
} from "react";
import {
  Box,
  ChevronRight,
  Code2,
  Film,
  Folder,
  FolderPlus,
  Grid2x2,
  Image as ImageIcon,
  LayoutGrid,
  List,
  Music2,
  Package,
  Play,
  RefreshCw,
  Search,
  Sparkles,
  Square,
  Trash2,
  Upload,
} from "lucide-react";
import {
  ASSET_DRAG_MIME,
  clearDraggedAsset,
  setDraggedAsset,
} from "../lib/assetDrag";
import { pickImportAssetFiles } from "../lib/tauriBridge";
import { isEditableModel } from "../lib/modelTabs";
import { BrpRpcError, type BrpClient } from "../protocol/brpClient";
import type {
  AssetImportSettings,
  AssetKind,
  AssetRecord,
  AssetRegistrySnapshot,
  TextureFilterMode,
} from "../protocol/types";

interface ContentBrowserPanelProps {
  onCreateModel?: (path: string) => Promise<string>;
  onOpenModel?: (asset: AssetRecord) => Promise<void>;
  openModelIds: string[];
  snapshot: AssetRegistrySnapshot | null;
  currentFolder: string;
  onChangeFolder: (folder: string) => void;
  brpClient: BrpClient | null;
  onImportPaths: (paths: string[], folder: string) => Promise<void>;
  onRefreshAssets: () => Promise<void>;
  onOpenScenePath?: (scenePath: string) => void;
  onSpawnAssetAtCenter?: (asset: AssetRecord) => void;
}

const KIND_FILTERS: Array<{ id: AssetKind | "all"; label: string }> = [
  { id: "all", label: "All" },
  { id: "image", label: "Images" },
  { id: "gltf_model", label: "3D glTF" },
  { id: "tilemap", label: "Tilemaps" },
  { id: "prefab", label: "Prefabs" },
  { id: "scene", label: "Scenes" },
  { id: "script", label: "Scripts" },
  { id: "audio", label: "Audio" },
];

function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`;
  }
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

function directParentFolder(relPath: string): string {
  const idx = relPath.lastIndexOf("/");
  return idx >= 0 ? relPath.slice(0, idx) : "";
}

function renderKindIcon(kind: AssetKind, size = 18) {
  switch (kind) {
    case "image":
      return <ImageIcon size={size} className="cb-kind-icon kind-image" />;
    case "gltf_model":
      return <Box size={size} className="cb-kind-icon kind-gltf" />;
    case "tilemap":
      return <Grid2x2 size={size} className="cb-kind-icon kind-tilemap" />;
    case "prefab":
      return <Package size={size} className="cb-kind-icon kind-prefab" />;
    case "scene":
      return <Film size={size} className="cb-kind-icon kind-scene" />;
    case "script":
      return <Code2 size={size} className="cb-kind-icon kind-script" />;
    case "material":
      return <Sparkles size={size} className="cb-kind-icon kind-material" />;
    case "audio":
      return <Music2 size={size} className="cb-kind-icon kind-audio" />;
    case "unknown":
      return <Box size={size} className="cb-kind-icon" />;
  }
}

function kindShortLabel(kind: AssetKind): string {
  switch (kind) {
    case "image":
      return "IMG";
    case "gltf_model":
      return "glTF";
    case "tilemap":
      return "TILE";
    case "prefab":
      return "PREFAB";
    case "scene":
      return "SCENE";
    case "script":
      return "SCRIPT";
    case "material":
      return "MAT";
    case "audio":
      return "AUDIO";
    case "unknown":
      return "FILE";
  }
}

export function ContentBrowserPanel({
  onCreateModel,
  onOpenModel,
  openModelIds,
  snapshot,
  currentFolder,
  onChangeFolder,
  brpClient,
  onImportPaths,
  onRefreshAssets,
  onOpenScenePath,
  onSpawnAssetAtCenter,
}: ContentBrowserPanelProps) {
  const [search, setSearch] = useState("");
  const [kindFilter, setKindFilter] = useState<AssetKind | "all">("all");
  const [viewMode, setViewMode] = useState<"grid" | "list">("grid");
  const [selectedUuid, setSelectedUuid] = useState<string | null>(null);
  const [thumbnails, setThumbnails] = useState<Record<string, string>>({});

  const [newFolderOpen, setNewFolderOpen] = useState(false);
  const [modelName, setModelName] = useState<string | null>(null);
  const [newFolderName, setNewFolderName] = useState("");
  const [renamePath, setRenamePath] = useState("");
  const [tagsDraft, setTagsDraft] = useState("");
  const [settingsDraft, setSettingsDraft] = useState<AssetImportSettings | null>(
    null,
  );
  const [blockedDependents, setBlockedDependents] = useState<string[] | null>(
    null,
  );
  const [actionError, setActionError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState(false);

  const selectedAsset = useMemo(
    () =>
      snapshot?.assets.find((asset) => asset.uuid === selectedUuid) ?? null,
    [snapshot, selectedUuid],
  );

  async function previewAudio(uuid: string | null) {
    if (!brpClient) return;
    setBusyAction(true);
    setActionError(null);
    try {
      await brpClient.call("engine.audio.preview", { uuid });
    } catch (error) {
      setActionError(String(error));
    } finally {
      setBusyAction(false);
    }
  }

  async function createModel() {
    if (!onCreateModel || busyAction || modelName === null) return;
    const base = modelName.trim().replace(/\.glb$/i, "").trim();
    if (!base || base.startsWith(".") || base.endsWith(".") || /[\\/:*?"<>|]/.test(base)) {
      setActionError("Enter a valid model file name without path separators.");
      return;
    }
    setBusyAction(true);
    setActionError(null);
    try {
      const id = await onCreateModel([currentFolder, `${base}.glb`].filter(Boolean).join("/"));
      setSelectedUuid(id);
      setModelName(null);
      setKindFilter("all");
      setSearch("");
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyAction(false);
    }
  }

  function selectAsset(asset: AssetRecord) {
    setSelectedUuid(asset.uuid);
  }

  function activateAsset(asset: AssetRecord) {
    if (onOpenModel && isEditableModel(asset)) {
      void onOpenModel(asset).catch((err) =>
        setActionError(err instanceof Error ? err.message : String(err)),
      );
      return;
    }
    if (asset.kind === "scene" && onOpenScenePath && snapshot) {
      onOpenScenePath(`${snapshot.root_dir}/${asset.relative_path}`);
    } else if (asset.kind === "audio") {
      void previewAudio(asset.uuid);
    } else {
      onSpawnAssetAtCenter?.(asset);
    }
  }

  // Close inline creation bars when switching or stopping projects.
  useEffect(() => {
    setModelName(null);
    setNewFolderOpen(false);
  }, [snapshot?.root_dir]);

  // Sync details editor drafts when selecting a different asset or updated revision.
  useEffect(() => {
    setBlockedDependents(null);
    setActionError(null);
    if (!selectedAsset) {
      setRenamePath("");
      setTagsDraft("");
      setSettingsDraft(null);
      return;
    }
    setRenamePath(selectedAsset.relative_path);
    setTagsDraft(selectedAsset.tags.join(", "));
    setSettingsDraft(structuredClone(selectedAsset.import_settings));
  }, [selectedAsset]);

  // Direct subfolders inside `currentFolder` (when no search/kind filter narrows globally).
  const visibleSubfolders = useMemo(() => {
    if (!snapshot) {
      return [];
    }
    const q = search.trim().toLowerCase();
    return snapshot.folders.filter((folder) => {
      if (directParentFolder(folder.relative_path) !== currentFolder) {
        return false;
      }
      if (q && !folder.name.toLowerCase().includes(q)) {
        return false;
      }
      return true;
    });
  }, [snapshot, currentFolder, search]);

  const visibleAssets = useMemo(() => {
    if (!snapshot) {
      return [];
    }
    const q = search.trim().toLowerCase();
    const globalFilterActive = q.length > 0 || kindFilter !== "all";
    return snapshot.assets.filter((asset) => {
      if (kindFilter !== "all" && asset.kind !== kindFilter) {
        return false;
      }
      if (!globalFilterActive) {
        if (directParentFolder(asset.relative_path) !== currentFolder) {
          return false;
        }
      } else if (
        currentFolder &&
        !asset.relative_path.startsWith(`${currentFolder}/`)
      ) {
        return false;
      }
      if (q) {
        const hay = `${asset.name} ${asset.relative_path} ${asset.tags.join(" ")}`.toLowerCase();
        if (!hay.includes(q)) {
          return false;
        }
      }
      return true;
    });
  }, [snapshot, currentFolder, kindFilter, search]);

  // Lazy-fetch PNG thumbnails for visible assets.
  useEffect(() => {
    if (!brpClient || visibleAssets.length === 0) {
      return;
    }
    let cancelled = false;
    const missing = visibleAssets.filter(
      (asset) => !thumbnails[`${asset.uuid}:${asset.source_hash}`],
    );
    if (missing.length === 0) {
      return;
    }

    const fetchNextBatch = async () => {
      for (const asset of missing.slice(0, 24)) {
        if (cancelled) {
          return;
        }
        try {
          const res = await brpClient.getAssetThumbnail(asset.uuid);
          if (!cancelled && res.ready && res.data_url) {
            const key = `${asset.uuid}:${asset.source_hash}`;
            setThumbnails((prev) =>
              prev[key] === res.data_url ? prev : { ...prev, [key]: res.data_url! },
            );
          }
        } catch {
          // Non-fatal; next AssetsChanged / selection will retry.
        }
      }
    };
    void fetchNextBatch();
    return () => {
      cancelled = true;
    };
  }, [brpClient, visibleAssets, thumbnails]);

  const breadcrumbs = useMemo(() => {
    if (!currentFolder) {
      return [];
    }
    const parts = currentFolder.split("/").filter(Boolean);
    return parts.map((name, idx) => ({
      name,
      path: parts.slice(0, idx + 1).join("/"),
    }));
  }, [currentFolder]);

  const handleImportClick = useCallback(async () => {
    try {
      const files = await pickImportAssetFiles();
      if (files.length > 0) {
        await onImportPaths(files, currentFolder);
      }
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
    }
  }, [currentFolder, onImportPaths]);

  const handleCreateFolder = useCallback(async () => {
    if (!brpClient) {
      return;
    }
    const trimmed = newFolderName.trim().replace(/^\/+|\/+$/g, "");
    if (!trimmed) {
      return;
    }
    const target = currentFolder ? `${currentFolder}/${trimmed}` : trimmed;
    setBusyAction(true);
    setActionError(null);
    try {
      await brpClient.createAssetFolder(target);
      setNewFolderName("");
      setNewFolderOpen(false);
      await onRefreshAssets();
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyAction(false);
    }
  }, [brpClient, currentFolder, newFolderName, onRefreshAssets]);

  const handleMoveRename = useCallback(async () => {
    if (!brpClient || !selectedAsset) {
      return;
    }
    const target = renamePath.trim();
    if (!target || target === selectedAsset.relative_path) {
      return;
    }
    setBusyAction(true);
    setActionError(null);
    try {
      await brpClient.moveAsset(selectedAsset.uuid, target);
      await onRefreshAssets();
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyAction(false);
    }
  }, [brpClient, onRefreshAssets, renamePath, selectedAsset]);

  const handleDelete = useCallback(
    async (force: boolean) => {
      if (!brpClient || !selectedAsset) {
        return;
      }
      setBusyAction(true);
      setActionError(null);
      try {
        await brpClient.deleteAsset(selectedAsset.uuid, force);
        setBlockedDependents(null);
        setSelectedUuid(null);
        await onRefreshAssets();
      } catch (err) {
        if (
          err instanceof BrpRpcError &&
          err.data &&
          typeof err.data === "object" &&
          "blocked_by_dependents" in err.data &&
          Array.isArray((err.data as { blocked_by_dependents?: unknown }).blocked_by_dependents)
        ) {
          const deps = (err.data as { blocked_by_dependents: unknown[] })
            .blocked_by_dependents
            .filter((d): d is string => typeof d === "string");
          setBlockedDependents(deps);
        } else {
          setActionError(err instanceof Error ? err.message : String(err));
        }
      } finally {
        setBusyAction(false);
      }
    },
    [brpClient, onRefreshAssets, selectedAsset],
  );

  const handleSaveSettings = useCallback(async () => {
    if (!brpClient || !selectedAsset || !settingsDraft) {
      return;
    }
    const tags = tagsDraft
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    setBusyAction(true);
    setActionError(null);
    try {
      await brpClient.updateAssetSettings(selectedAsset.uuid, settingsDraft, tags);
      await onRefreshAssets();
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyAction(false);
    }
  }, [brpClient, onRefreshAssets, selectedAsset, settingsDraft, tagsDraft]);

  const handleAssetDragStart = (
    e: ReactDragEvent<HTMLElement>,
    asset: AssetRecord,
  ) => {
    setDraggedAsset(asset);
    e.dataTransfer.effectAllowed = "copy";
    try {
      e.dataTransfer.setData(ASSET_DRAG_MIME, JSON.stringify(asset));
      e.dataTransfer.setData("text/plain", asset.uuid);
    } catch {
      // Fallback relies on `setDraggedAsset`.
    }
  };

  return (
    <div className="content-browser-root">
      <div className="cb-toolbar">
        <div className="cb-breadcrumbs">
          <button
            type="button"
            className={`cb-crumb ${currentFolder === "" ? "active" : ""}`}
            onClick={() => onChangeFolder("")}
          >
            assets
          </button>
          {breadcrumbs.map((crumb) => (
            <span key={crumb.path} className="cb-crumb-sep">
              <ChevronRight size={12} />
              <button
                type="button"
                className={`cb-crumb ${currentFolder === crumb.path ? "active" : ""}`}
                onClick={() => onChangeFolder(crumb.path)}
              >
                {crumb.name}
              </button>
            </span>
          ))}
        </div>

        <div className="cb-search-box">
          <Search size={12} />
          <input
            type="text"
            className="text-input cb-search-input"
            placeholder="Filter assets or tags..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
        </div>

        <div className="cb-kind-pills">
          {KIND_FILTERS.map((f) => (
            <button
              key={f.id}
              type="button"
              className={`cb-pill ${kindFilter === f.id ? "active" : ""}`}
              onClick={() => setKindFilter(f.id)}
            >
              {f.label}
            </button>
          ))}
        </div>

        <div className="cb-actions">
          <button
            type="button"
            className="ghost-btn"
            disabled={!onCreateModel || !snapshot || busyAction}
            title={
              onCreateModel
                ? "Create a new Blender .glb model in this folder"
                : "New Model requires the desktop editor with an active project"
            }
            onClick={() => {
              setNewFolderOpen(false);
              setModelName((prev) => (prev === null ? "New Model" : null));
            }}
          >
            <Box size={13} /> New Model
          </button>
          <button
            type="button"
            className="ghost-btn"
            disabled={!brpClient || busyAction}
            onClick={() => { setModelName(null); setNewFolderOpen((open) => !open); }}
            title="New folder inside current directory"
          >
            <FolderPlus size={13} />
            Folder
          </button>
          <button
            type="button"
            className="ghost-btn"
            disabled={!brpClient || busyAction}
            onClick={() => void handleImportClick()}
            title="Import PNG/glTF/RON files into current folder"
          >
            <Upload size={13} />
            Import
          </button>
          <button
            type="button"
            className="icon-btn"
            disabled={!brpClient || busyAction}
            onClick={() => void brpClient?.rescanAssets().then(() => onRefreshAssets())}
            title="Rescan project assets"
          >
            <RefreshCw size={13} />
          </button>
          <button
            type="button"
            className="icon-btn"
            onClick={() => setViewMode((m) => (m === "grid" ? "list" : "grid"))}
            title="Toggle Grid/List view"
          >
            {viewMode === "grid" ? <List size={13} /> : <LayoutGrid size={13} />}
          </button>
        </div>
      </div>

      {newFolderOpen && (
        <div className="cb-inline-bar">
          <input
            type="text"
            className="text-input"
            placeholder="New folder name (e.g. textures/ui)..."
            value={newFolderName}
            onChange={(e) => setNewFolderName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                void handleCreateFolder();
              } else if (e.key === "Escape") {
                setNewFolderOpen(false);
              }
            }}
          />
          <button
            type="button"
            className="primary-btn"
            disabled={busyAction}
            onClick={() => void handleCreateFolder()}
          >
            Create
          </button>
          <button
            type="button"
            className="ghost-btn"
            onClick={() => setNewFolderOpen(false)}
          >
            Cancel
          </button>
        </div>
      )}

      {modelName !== null && (
        <form className="cb-inline-bar" onSubmit={(e) => { e.preventDefault(); void createModel(); }}>
          <input
            autoFocus
            className="text-input"
            aria-label="New model name"
            value={modelName}
            disabled={busyAction}
            onFocus={(e) => e.target.select()}
            onChange={(e) => setModelName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setModelName(null);
            }}
          />
          <span className="muted">.glb</span>
          <button type="submit" className="primary-btn" disabled={busyAction}>
            {busyAction ? "Creating…" : "Create model"}
          </button>
          <button
            type="button"
            className="ghost-btn"
            disabled={busyAction}
            onClick={() => setModelName(null)}
          >
            Cancel
          </button>
        </form>
      )}

      {actionError && (
        <div className="cb-error-bar">
          <span>{actionError}</span>
          <button
            type="button"
            className="ghost-btn"
            onClick={() => setActionError(null)}
          >
            Dismiss
          </button>
        </div>
      )}

      <div className="cb-body">
        {/* Left folder tree */}
        <div className="cb-folder-tree">
          <button
            type="button"
            className={`cb-tree-item ${currentFolder === "" ? "selected" : ""}`}
            onClick={() => onChangeFolder("")}
          >
            <Folder size={13} />
            <span>assets/</span>
          </button>
          {(snapshot?.folders ?? []).map((folder) => {
            const depth = folder.relative_path.split("/").length - 1;
            return (
              <button
                key={folder.relative_path}
                type="button"
                className={`cb-tree-item ${currentFolder === folder.relative_path ? "selected" : ""}`}
                style={{ paddingLeft: `${12 + depth * 12}px` }}
                onClick={() => onChangeFolder(folder.relative_path)}
              >
                <Folder size={13} />
                <span className="truncate">{folder.name}</span>
              </button>
            );
          })}
        </div>

        {/* Center grid/list */}
        <div className="cb-main-area">
          {!snapshot ? (
            <div className="empty-state">
              Start an engine project to browse project assets.
            </div>
          ) : visibleSubfolders.length === 0 && visibleAssets.length === 0 ? (
            <div className="empty-state">
              Empty folder. Drop files from OS or click Import.
            </div>
          ) : viewMode === "grid" ? (
            <div className="cb-grid">
              {visibleSubfolders.map((folder) => (
                <div
                  key={`dir:${folder.relative_path}`}
                  className="cb-card cb-folder-card"
                  onDoubleClick={() => onChangeFolder(folder.relative_path)}
                  title={`Open folder '${folder.relative_path}'`}
                >
                  <div className="cb-thumb-box">
                    <Folder size={34} className="cb-folder-glyph" />
                  </div>
                  <div className="cb-card-name truncate">{folder.name}</div>
                  <div className="cb-card-sub">FOLDER</div>
                </div>
              ))}

              {visibleAssets.map((asset) => {
                const thumbUrl = thumbnails[`${asset.uuid}:${asset.source_hash}`];
                const isSelected = asset.uuid === selectedUuid;
                const isOpenModel = openModelIds.includes(asset.uuid);
                const modelHint =
                  isEditableModel(asset) && onOpenModel
                    ? "Double-click to edit in Blender · "
                    : "";
                return (
                  <div
                    key={asset.uuid}
                    className={`cb-card ${isSelected ? "selected" : ""} ${isOpenModel ? "cb-card-open" : ""}`}
                    draggable
                    onDragStart={(e) => handleAssetDragStart(e, asset)}
                    onDragEnd={() => clearDraggedAsset()}
                    onClick={() => selectAsset(asset)}
                    onDoubleClick={() => activateAsset(asset)}
                    title={`${asset.relative_path}\nUUID: ${asset.uuid}\n${modelHint}Drag to Viewport or Inspector`}
                  >
                    <div className="cb-thumb-box">
                      {thumbUrl ? (
                        <img
                          src={thumbUrl}
                          alt={asset.name}
                          className="cb-thumb-img"
                          draggable={false}
                        />
                      ) : (
                        renderKindIcon(asset.kind, 32)
                      )}
                      {isOpenModel && (
                        <span className="cb-model-open-badge" title="Open in Blender tab">
                          BLENDER
                        </span>
                      )}
                      <span className="cb-badge">{kindShortLabel(asset.kind)}</span>
                    </div>
                    <div className="cb-card-name truncate">{asset.name}</div>
                    <div className="cb-card-sub truncate">
                      {formatBytes(asset.file_size)}
                    </div>
                  </div>
                );
              })}
            </div>
          ) : (
            <div className="cb-list">
              {visibleSubfolders.map((folder) => (
                <div
                  key={`dir:${folder.relative_path}`}
                  className="cb-list-row"
                  onDoubleClick={() => onChangeFolder(folder.relative_path)}
                >
                  <Folder size={14} />
                  <span className="cb-list-name">{folder.name}</span>
                  <span className="muted">FOLDER</span>
                </div>
              ))}
              {visibleAssets.map((asset) => {
                const isOpenModel = openModelIds.includes(asset.uuid);
                return (
                  <div
                    key={asset.uuid}
                    className={`cb-list-row ${asset.uuid === selectedUuid ? "selected" : ""}`}
                    draggable
                    onDragStart={(e) => handleAssetDragStart(e, asset)}
                    onDragEnd={() => clearDraggedAsset()}
                    onClick={() => selectAsset(asset)}
                    onDoubleClick={() => activateAsset(asset)}
                  >
                    {renderKindIcon(asset.kind, 14)}
                    <span className="cb-list-name truncate">{asset.relative_path}</span>
                    {isOpenModel && <span className="cb-model-open-pill">BLENDER</span>}
                    <span className="muted">{kindShortLabel(asset.kind)}</span>
                    <span className="muted">{formatBytes(asset.file_size)}</span>
                  </div>
                );
              })}
            </div>
          )}
        </div>

        {/* Right asset details & import settings */}
        {selectedAsset && (
        <div className="cb-details">
            <div className="cb-details-header">
              <div className="cb-details-title">
                {renderKindIcon(selectedAsset.kind, 15)}
                <span className="truncate">{selectedAsset.name}</span>
              </div>
              <button
                type="button"
                className="ghost-btn danger"
                disabled={busyAction || openModelIds.includes(selectedAsset.uuid)}
                onClick={() => void handleDelete(false)}
                title={
                  openModelIds.includes(selectedAsset.uuid)
                    ? "Close the Blender model tab before deleting"
                    : "Safe Delete asset and .b9meta"
                }
              >
                <Trash2 size={12} />
                Delete
              </button>
            </div>

            {isEditableModel(selectedAsset) && (onOpenModel || onSpawnAssetAtCenter) && (
              <div className="model-details-bar">
                {onOpenModel && (
                  <button
                    type="button"
                    className="primary"
                    disabled={busyAction}
                    onClick={() =>
                      void onOpenModel(selectedAsset).catch((err) =>
                        setActionError(err instanceof Error ? err.message : String(err)),
                      )
                    }
                  >
                    <Box size={12} />
                    {openModelIds.includes(selectedAsset.uuid)
                      ? "Focus Blender tab"
                      : "Edit in Blender"}
                  </button>
                )}
                {onSpawnAssetAtCenter && (
                  <button
                    type="button"
                    disabled={busyAction}
                    onClick={() => onSpawnAssetAtCenter(selectedAsset)}
                  >
                    Spawn in scene
                  </button>
                )}
              </div>
            )}

            {selectedAsset.kind === "audio" && (
              <div className="audio-preview-bar">
                <button
                  type="button"
                  className="primary"
                  disabled={busyAction}
                  onClick={() => void previewAudio(selectedAsset.uuid)}
                >
                  <Play size={12} /> Play preview
                </button>
                <button
                  type="button"
                  disabled={busyAction}
                  onClick={() => void previewAudio(null)}
                >
                  <Square size={11} /> Stop
                </button>
              </div>
            )}

            {blockedDependents && (
              <div className="cb-delete-warning">
                <div className="cb-delete-title">
                  Referenced by {blockedDependents.length} dependent(s):
                </div>
                <ul className="cb-dep-list">
                  {blockedDependents.map((dep) => (
                    <li key={dep} className="truncate">
                      {dep}
                    </li>
                  ))}
                </ul>
                <div className="cb-delete-actions">
                  <button
                    type="button"
                    className="ghost-btn"
                    onClick={() => setBlockedDependents(null)}
                  >
                    Cancel
                  </button>
                  <button
                    type="button"
                    className="primary-btn danger"
                    disabled={busyAction}
                    onClick={() => void handleDelete(true)}
                  >
                    Force Delete
                  </button>
                </div>
              </div>
            )}

            <div className="cb-meta-kv">
              <span className="muted">UUID</span>
              <code className="cb-mono truncate" title={selectedAsset.uuid}>
                {selectedAsset.uuid}
              </code>
              <span className="muted">BLAKE3</span>
              <span className="cb-mono">
                {selectedAsset.source_hash.slice(0, 14)}…
              </span>
              <span className="muted">Links</span>
              <span>
                {selectedAsset.dependencies.length} deps ·{" "}
                {selectedAsset.dependents.length} refs
              </span>
            </div>

            <div className="cb-field-group">
              <label className="muted">Relative Path / Rename</label>
              <div className="cb-inline-bar compact">
                <input
                  type="text"
                  className="text-input"
                  value={renamePath}
                  onChange={(e) => setRenamePath(e.target.value)}
                />
                <button
                  type="button"
                  className="ghost-btn"
                  disabled={
                    busyAction ||
                    openModelIds.includes(selectedAsset.uuid) ||
                    !renamePath.trim() ||
                    renamePath.trim() === selectedAsset.relative_path
                  }
                  title={
                    openModelIds.includes(selectedAsset.uuid)
                      ? "Close the Blender model tab before moving or renaming"
                      : "Move or rename asset"
                  }
                  onClick={() => void handleMoveRename()}
                >
                  Move
                </button>
              </div>
            </div>

            {selectedAsset.sub_assets.length > 0 && (
              <div className="cb-field-group">
                <label className="muted">
                  Sub-Assets ({selectedAsset.sub_assets.length})
                </label>
                <div className="cb-subassets">
                  {selectedAsset.sub_assets.map((sub) => (
                    <span
                      key={sub.label}
                      className="cb-subasset-chip"
                      title={sub.label}
                    >
                      {sub.kind}: {sub.name}
                    </span>
                  ))}
                </div>
              </div>
            )}

            {settingsDraft && (
              <div className="cb-field-group">
                <label className="muted">Import Settings</label>

                {settingsDraft.kind === "image" && (
                  <div className="cb-settings-grid">
                    <label className="cb-check">
                      <input
                        type="checkbox"
                        checked={settingsDraft.srgb}
                        onChange={(e) =>
                          setSettingsDraft({
                            ...settingsDraft,
                            srgb: e.target.checked,
                          })
                        }
                      />
                      sRGB
                    </label>
                    <div className="prop-row">
                      <span className="prop-label">Filter</span>
                      <select
                        className="select-input prop-control"
                        value={settingsDraft.filter_mode}
                        onChange={(e) =>
                          setSettingsDraft({
                            ...settingsDraft,
                            filter_mode: e.target.value as TextureFilterMode,
                          })
                        }
                      >
                        <option value="linear">Linear</option>
                        <option value="nearest">Nearest</option>
                      </select>
                    </div>
                    <label className="cb-check">
                      <input
                        type="checkbox"
                        checked={settingsDraft.atlas_grid !== null}
                        onChange={(e) =>
                          setSettingsDraft({
                            ...settingsDraft,
                            atlas_grid: e.target.checked
                              ? {
                                  tile_size: [32, 32],
                                  columns: 2,
                                  rows: 2,
                                  padding: [0, 0],
                                  offset: [0, 0],
                                }
                              : null,
                          })
                        }
                      />
                      Atlas Grid
                    </label>
                    {settingsDraft.atlas_grid && (
                      <div className="cb-atlas-inputs">
                        <input
                          type="number"
                          className="num-input"
                          value={settingsDraft.atlas_grid.tile_size[0]}
                          title="Tile Width"
                          onChange={(e) =>
                            setSettingsDraft({
                              ...settingsDraft,
                              atlas_grid: {
                                ...settingsDraft.atlas_grid!,
                                tile_size: [
                                  Math.max(1, Number(e.target.value) || 1),
                                  settingsDraft.atlas_grid!.tile_size[1],
                                ],
                              },
                            })
                          }
                        />
                        <input
                          type="number"
                          className="num-input"
                          value={settingsDraft.atlas_grid.tile_size[1]}
                          title="Tile Height"
                          onChange={(e) =>
                            setSettingsDraft({
                              ...settingsDraft,
                              atlas_grid: {
                                ...settingsDraft.atlas_grid!,
                                tile_size: [
                                  settingsDraft.atlas_grid!.tile_size[0],
                                  Math.max(1, Number(e.target.value) || 1),
                                ],
                              },
                            })
                          }
                        />
                        <input
                          type="number"
                          className="num-input"
                          value={settingsDraft.atlas_grid.columns}
                          title="Columns"
                          onChange={(e) =>
                            setSettingsDraft({
                              ...settingsDraft,
                              atlas_grid: {
                                ...settingsDraft.atlas_grid!,
                                columns: Math.max(1, Number(e.target.value) || 1),
                              },
                            })
                          }
                        />
                        <input
                          type="number"
                          className="num-input"
                          value={settingsDraft.atlas_grid.rows}
                          title="Rows"
                          onChange={(e) =>
                            setSettingsDraft({
                              ...settingsDraft,
                              atlas_grid: {
                                ...settingsDraft.atlas_grid!,
                                rows: Math.max(1, Number(e.target.value) || 1),
                              },
                            })
                          }
                        />
                      </div>
                    )}
                  </div>
                )}

                {settingsDraft.kind === "gltf" && (
                  <div className="cb-settings-grid">
                    <div className="prop-row">
                      <span className="prop-label">Scale</span>
                      <input
                        type="number"
                        step="0.1"
                        className="num-input"
                        value={settingsDraft.scale}
                        onChange={(e) =>
                          setSettingsDraft({
                            ...settingsDraft,
                            scale: Number(e.target.value) || 1.0,
                          })
                        }
                      />
                    </div>
                    <label className="cb-check">
                      <input
                        type="checkbox"
                        checked={settingsDraft.load_meshes}
                        onChange={(e) =>
                          setSettingsDraft({
                            ...settingsDraft,
                            load_meshes: e.target.checked,
                          })
                        }
                      />
                      Meshes
                    </label>
                    <label className="cb-check">
                      <input
                        type="checkbox"
                        checked={settingsDraft.load_materials}
                        onChange={(e) =>
                          setSettingsDraft({
                            ...settingsDraft,
                            load_materials: e.target.checked,
                          })
                        }
                      />
                      Materials
                    </label>
                    <label className="cb-check">
                      <input
                        type="checkbox"
                        checked={settingsDraft.load_lights}
                        onChange={(e) =>
                          setSettingsDraft({
                            ...settingsDraft,
                            load_lights: e.target.checked,
                          })
                        }
                      />
                      Lights
                    </label>
                    <label className="cb-check">
                      <input
                        type="checkbox"
                        checked={settingsDraft.load_animations}
                        onChange={(e) =>
                          setSettingsDraft({
                            ...settingsDraft,
                            load_animations: e.target.checked,
                          })
                        }
                      />
                      Animations
                    </label>
                  </div>
                )}

                {settingsDraft.kind === "tilemap" && (
                  <div className="cb-settings-grid">
                    <div className="prop-row">
                      <span className="prop-label">PPU Scale</span>
                      <input
                        type="number"
                        step="0.1"
                        className="num-input"
                        value={settingsDraft.pixels_per_unit}
                        onChange={(e) =>
                          setSettingsDraft({
                            ...settingsDraft,
                            pixels_per_unit: Number(e.target.value) || 1.0,
                          })
                        }
                      />
                    </div>
                  </div>
                )}

                <label className="muted">Tags (comma separated)</label>
                <div className="cb-inline-bar compact">
                  <input
                    type="text"
                    className="text-input"
                    value={tagsDraft}
                    onChange={(e) => setTagsDraft(e.target.value)}
                  />
                  <button
                    type="button"
                    className="primary-btn"
                    disabled={busyAction}
                    onClick={() => void handleSaveSettings()}
                  >
                    Apply
                  </button>
                </div>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
