import type {
  BrpGetComponentsResponse,
  BrpRegistrySchema,
  ComponentMutationResult,
  DeleteEntityResult,
  HistoryStatePayload,
  HistoryStepResult,
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
      throw new Error(
        `BRP '${method}' error (${payload.error.code}): ${payload.error.message}`,
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
}
