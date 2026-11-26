import type {
  AssetDeleteResult,
  AssetFolderEntry,
  AssetImportResult,
  AssetImportSettings,
  AssetListParams,
  AssetRecord,
  AssetRegistrySnapshot,
  AssetSpawnInViewportResult,
  AssetThumbnailResult,
  BrpGetComponentsResponse,
  BrpRegistrySchema,
  ComponentMutationResult,
  DeleteEntityResult,
  HistoryStatePayload,
  HistoryStepResult,
  PrefabApplyResult,
  PrefabListResult,
  PrefabRevertResult,
  PrefabSaveParams,
  PrefabSaveResult,
  PrefabSpawnParams,
  PrefabSpawnResult,
  ReparentEntityResult,
  SceneLoadParams,
  SceneNewParams,
  SceneOperationResult,
  SceneSaveParams,
  SetViewportModeResult,
  SpawnPresetParams,
  SpawnPresetResult,
  ViewportMode,
  WorldTreeResult,
} from "./types";

interface JsonRpcErrorPayload {
  code: number;
  message: string;
  data?: unknown;
}

export class BrpRpcError extends Error {
  constructor(
    public readonly method: string,
    public readonly code: number,
    public readonly rpcMessage: string,
    public readonly data?: unknown,
  ) {
    super(`BRP '${method}' error (${code}): ${rpcMessage}`);
    this.name = "BrpRpcError";
  }
}

interface JsonRpcResponse<T> {
  jsonrpc: string;
  id: number;
  result?: T;
  error?: JsonRpcErrorPayload;
}

export class BrpClient {
  private nextId = 1;

  constructor(private readonly baseUrl: string) {}

  async call<T>(method: string, params?: unknown): Promise<T> {
    const id = this.nextId++;
    const response = await fetch(this.baseUrl, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id,
        method,
        params: params ?? null,
      }),
    });

    if (!response.ok) {
      throw new Error(`HTTP ${response.status} while calling BRP method '${method}'`);
    }

    const payload = (await response.json()) as JsonRpcResponse<T>;
    if (payload.error) {
      throw new BrpRpcError(
        method,
        payload.error.code,
        payload.error.message,
        payload.error.data,
      );
    }

    if (payload.result === undefined) {
      return null as T;
    }
    return payload.result;
  }

  getRegistrySchema(): Promise<BrpRegistrySchema> {
    return this.call<BrpRegistrySchema>("registry.schema");
  }

  getWorldTree(): Promise<WorldTreeResult> {
    return this.call<WorldTreeResult>("engine.world.tree");
  }

  newScene(params: SceneNewParams): Promise<SceneOperationResult> {
    return this.call<SceneOperationResult>("engine.scene.new", params);
  }

  loadScene(params: SceneLoadParams): Promise<SceneOperationResult> {
    return this.call<SceneOperationResult>("engine.scene.load", params);
  }

  saveScene(params: SceneSaveParams = {}): Promise<SceneOperationResult> {
    return this.call<SceneOperationResult>("engine.scene.save", params);
  }

  spawnPreset(params: SpawnPresetParams): Promise<SpawnPresetResult> {
    return this.call<SpawnPresetResult>("engine.entity.spawn_preset", params);
  }

  setViewportMode(mode: ViewportMode): Promise<SetViewportModeResult> {
    return this.call<SetViewportModeResult>("engine.viewport.set_mode", { mode });
  }

  getComponents(
    entityBits: number,
    components: string[],
    strict = false,
  ): Promise<BrpGetComponentsResponse> {
    return this.call<BrpGetComponentsResponse>("world.get_components", {
      entity: entityBits,
      components,
      strict,
    });
  }

  insertComponents(
    entityBits: number,
    components: Record<string, unknown>,
  ): Promise<null> {
    return this.call<null>("world.insert_components", {
      entity: entityBits,
      components,
    });
  }

  removeComponents(entityBits: number, components: string[]): Promise<null> {
    return this.call<null>("world.remove_components", {
      entity: entityBits,
      components,
    });
  }

  despawnEntity(entityBits: number): Promise<null> {
    return this.call<null>("world.despawn_entity", {
      entity: entityBits,
    });
  }

  reparentEntities(entities: number[], parent: number | null): Promise<null> {
    return this.call<null>("world.reparent_entities", {
      entities,
      parent,
    });
  }

  undo(): Promise<HistoryStepResult> {
    return this.call<HistoryStepResult>("engine.history.undo");
  }

  redo(): Promise<HistoryStepResult> {
    return this.call<HistoryStepResult>("engine.history.redo");
  }

  getHistoryState(): Promise<HistoryStatePayload> {
    return this.call<HistoryStatePayload>("engine.history.state");
  }

  deleteEntity(guid: string): Promise<DeleteEntityResult> {
    return this.call<DeleteEntityResult>("engine.entity.delete", { guid });
  }

  reparentEntity(
    guid: string,
    parentGuid: string | null,
  ): Promise<ReparentEntityResult> {
    return this.call<ReparentEntityResult>("engine.entity.reparent", {
      guid,
      parent_guid: parentGuid,
    });
  }

  setComponent(
    guid: string,
    typePath: string,
    value: unknown,
    mergeKey?: string,
    label?: string,
  ): Promise<ComponentMutationResult> {
    return this.call<ComponentMutationResult>("engine.component.set", {
      guid,
      type_path: typePath,
      value,
      merge_key: mergeKey ?? null,
      label: label ?? null,
    });
  }

  addComponent(
    guid: string,
    typePath: string,
    value: unknown,
  ): Promise<ComponentMutationResult> {
    return this.call<ComponentMutationResult>("engine.component.add", {
      guid,
      type_path: typePath,
      value,
    });
  }

  removeComponent(
    guid: string,
    typePath: string,
  ): Promise<ComponentMutationResult> {
    return this.call<ComponentMutationResult>("engine.component.remove", {
      guid,
      type_path: typePath,
    });
  }

  savePrefab(params: PrefabSaveParams): Promise<PrefabSaveResult> {
    return this.call<PrefabSaveResult>("engine.prefab.save", params);
  }

  spawnPrefab(params: PrefabSpawnParams): Promise<PrefabSpawnResult> {
    return this.call<PrefabSpawnResult>("engine.prefab.spawn", params);
  }

  applyPrefab(guid: string): Promise<PrefabApplyResult> {
    return this.call<PrefabApplyResult>("engine.prefab.apply", { guid });
  }

  revertPrefab(
    guid: string,
    typePath?: string | null,
  ): Promise<PrefabRevertResult> {
    return this.call<PrefabRevertResult>("engine.prefab.revert", {
      guid,
      type_path: typePath ?? null,
    });
  }

  listPrefabs(): Promise<PrefabListResult> {
    return this.call<PrefabListResult>("engine.prefab.list");
  }

  listAssets(params: AssetListParams = {}): Promise<AssetRegistrySnapshot> {
    return this.call<AssetRegistrySnapshot>("engine.asset.list", params);
  }

  importAssets(sourcePaths: string[], targetFolder = ""): Promise<AssetImportResult> {
    return this.call<AssetImportResult>("engine.asset.import", {
      source_paths: sourcePaths,
      target_folder: targetFolder,
    });
  }

  moveAsset(uuid: string, newRelativePath: string): Promise<AssetRecord> {
    return this.call<AssetRecord>("engine.asset.move", {
      uuid,
      new_relative_path: newRelativePath,
    });
  }

  deleteAsset(uuid: string, force = false): Promise<AssetDeleteResult> {
    return this.call<AssetDeleteResult>("engine.asset.delete", { uuid, force });
  }

  createAssetFolder(relativePath: string): Promise<AssetFolderEntry> {
    return this.call<AssetFolderEntry>("engine.asset.create_folder", {
      relative_path: relativePath,
    });
  }

  updateAssetSettings(
    uuid: string,
    importSettings: AssetImportSettings,
    tags?: string[] | null,
  ): Promise<AssetRecord> {
    return this.call<AssetRecord>("engine.asset.update_settings", {
      uuid,
      import_settings: importSettings,
      tags: tags ?? null,
    });
  }

  getAssetThumbnail(uuid: string): Promise<AssetThumbnailResult> {
    return this.call<AssetThumbnailResult>("engine.asset.thumbnail", { uuid });
  }

  spawnAssetInViewport(
    uuid: string,
    x: number,
    y: number,
  ): Promise<AssetSpawnInViewportResult> {
    return this.call<AssetSpawnInViewportResult>("engine.asset.spawn_in_viewport", {
      uuid,
      x,
      y,
    });
  }

  rescanAssets(params: AssetListParams = {}): Promise<AssetRegistrySnapshot> {
    return this.call<AssetRegistrySnapshot>("engine.asset.rescan", params);
  }
}
