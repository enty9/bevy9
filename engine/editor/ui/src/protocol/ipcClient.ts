import { decodeViewportFrame } from "./frameCodec";
import {
  PROTOCOL_VERSION,
  type ClientIpcMessage,
  type DecodedViewportFrame,
  type EditorLogEntry,
  type FrameMetrics,
  type GizmoStatePayload,
  type HistoryStatePayload,
  type SceneTreeNode,
  type ServerIpcMessage,
  type ViewportCameraMode,
  type ViewportInputEvent,
  type ViewportMode,
  type ViewportStatePayload,
  type WelcomePayload,
} from "./types";

export type IpcConnectionState = "disconnected" | "connecting" | "connected";

type Listener<T> = (payload: T) => void;

export class EditorIpcClient {
  private ws: WebSocket | null = null;
  private closedByCaller = false;
  private reconnectTimer: number | null = null;
  private backoffMs = 500;
  private state: IpcConnectionState = "disconnected";

  private readonly stateListeners = new Set<Listener<IpcConnectionState>>();
  private readonly welcomeListeners = new Set<Listener<WelcomePayload>>();
  private readonly hierarchyListeners = new Set<Listener<SceneTreeNode[]>>();
  private readonly selectionListeners = new Set<Listener<string[]>>();
  private readonly viewportStateListeners = new Set<Listener<ViewportStatePayload>>();
  private readonly gizmoStateListeners = new Set<Listener<GizmoStatePayload>>();
  private readonly inspectorRefreshListeners = new Set<Listener<string[]>>();
  private readonly historyStateListeners = new Set<Listener<HistoryStatePayload>>();
  private readonly metricsListeners = new Set<Listener<FrameMetrics>>();
  private readonly logListeners = new Set<Listener<EditorLogEntry>>();
  private readonly errorListeners = new Set<Listener<string>>();
  private readonly frameListeners = new Set<Listener<DecodedViewportFrame>>();

  constructor(private readonly url: string) {}

  getConnectionState(): IpcConnectionState {
    return this.state;
  }

  connect(): void {
    this.closedByCaller = false;
    this.clearReconnectTimer();
    this.openSocket();
  }

  disconnect(): void {
    this.closedByCaller = true;
    this.clearReconnectTimer();
    if (this.ws) {
      this.ws.onopen = null;
      this.ws.onmessage = null;
      this.ws.onclose = null;
      this.ws.onerror = null;
      this.ws.close();
      this.ws = null;
    }
    this.setState("disconnected");
  }

  sendViewportInput(event: ViewportInputEvent): void {
    this.send({ type: "viewport_input", event });
  }

  setViewportMode(mode: ViewportMode): void {
    this.send({ type: "set_viewport_mode", mode });
  }

  setCameraMode(cameraMode: ViewportCameraMode): void {
    this.send({ type: "set_camera_mode", camera_mode: cameraMode });
  }

  selectEntities(guids: string[]): void {
    this.send({ type: "select_entities", guids });
  }

  setGizmoState(next: GizmoStatePayload): void {
    this.send({
      type: "set_gizmo_state",
      mode: next.mode,
      space: next.space,
      snap: next.snap,
    });
  }

  ping(nonce = Date.now()): void {
    this.send({ type: "ping", nonce });
  }

  onConnectionState(listener: Listener<IpcConnectionState>): () => void {
    this.stateListeners.add(listener);
    return () => {
      this.stateListeners.delete(listener);
    };
  }

  onWelcome(listener: Listener<WelcomePayload>): () => void {
    this.welcomeListeners.add(listener);
    return () => {
      this.welcomeListeners.delete(listener);
    };
  }

  onHierarchy(listener: Listener<SceneTreeNode[]>): () => void {
    this.hierarchyListeners.add(listener);
    return () => {
      this.hierarchyListeners.delete(listener);
    };
  }

  onSelection(listener: Listener<string[]>): () => void {
    this.selectionListeners.add(listener);
    return () => {
      this.selectionListeners.delete(listener);
    };
  }

  onViewportState(listener: Listener<ViewportStatePayload>): () => void {
    this.viewportStateListeners.add(listener);
    return () => {
      this.viewportStateListeners.delete(listener);
    };
  }

  onGizmoState(listener: Listener<GizmoStatePayload>): () => void {
    this.gizmoStateListeners.add(listener);
    return () => {
      this.gizmoStateListeners.delete(listener);
    };
  }

  onInspectorRefresh(listener: Listener<string[]>): () => void {
    this.inspectorRefreshListeners.add(listener);
    return () => {
      this.inspectorRefreshListeners.delete(listener);
    };
  }

  onHistoryState(listener: Listener<HistoryStatePayload>): () => void {
    this.historyStateListeners.add(listener);
    return () => {
      this.historyStateListeners.delete(listener);
    };
  }

  onMetrics(listener: Listener<FrameMetrics>): () => void {
    this.metricsListeners.add(listener);
    return () => {
      this.metricsListeners.delete(listener);
    };
  }

  onLog(listener: Listener<EditorLogEntry>): () => void {
    this.logListeners.add(listener);
    return () => {
      this.logListeners.delete(listener);
    };
  }

  onError(listener: Listener<string>): () => void {
    this.errorListeners.add(listener);
    return () => {
      this.errorListeners.delete(listener);
    };
  }

  onFrame(listener: Listener<DecodedViewportFrame>): () => void {
    this.frameListeners.add(listener);
    return () => {
      this.frameListeners.delete(listener);
    };
  }

  private openSocket(): void {
    if (this.closedByCaller) {
      return;
    }
    this.setState("connecting");

    const ws = new WebSocket(this.url);
    ws.binaryType = "arraybuffer";
    this.ws = ws;

    ws.onopen = () => {
      if (this.ws !== ws || this.closedByCaller) {
        return;
      }
      this.backoffMs = 500;
      this.setState("connected");
      this.send({
        type: "hello",
        protocol_version: PROTOCOL_VERSION,
        client_name: "bevy9-editor-ui",
      });
    };

    ws.onmessage = (event) => {
      if (this.ws !== ws) {
        return;
      }

      if (event.data instanceof ArrayBuffer) {
        const frame = decodeViewportFrame(event.data);
        if (frame) {
          for (const listener of this.frameListeners) {
            listener(frame);
          }
        }
        return;
      }

      if (typeof event.data === "string") {
        this.handleTextMessage(event.data);
      }
    };

    ws.onerror = () => {
      // `onclose` immediately follows on WebSocket failures and triggers reconnect.
    };

    ws.onclose = () => {
      if (this.ws === ws) {
        this.ws = null;
      }
      this.setState("disconnected");
      if (!this.closedByCaller) {
        this.scheduleReconnect();
      }
    };
  }

  private handleTextMessage(raw: string): void {
    let parsed: ServerIpcMessage;
    try {
      parsed = JSON.parse(raw) as ServerIpcMessage;
    } catch {
      return;
    }

    switch (parsed.type) {
      case "welcome":
        for (const listener of this.welcomeListeners) {
          listener(parsed);
        }
        break;
      case "hierarchy_snapshot":
        for (const listener of this.hierarchyListeners) {
          listener(parsed.roots);
        }
        break;
      case "selection_changed":
        for (const listener of this.selectionListeners) {
          listener(parsed.guids);
        }
        break;
      case "viewport_state_changed":
        for (const listener of this.viewportStateListeners) {
          listener(parsed);
        }
        break;
      case "gizmo_state_changed":
        for (const listener of this.gizmoStateListeners) {
          listener({
            mode: parsed.mode,
            space: parsed.space,
            snap: parsed.snap,
          });
        }
        break;
      case "inspector_refresh":
        for (const listener of this.inspectorRefreshListeners) {
          listener(parsed.guids);
        }
        break;
      case "history_state_changed":
        for (const listener of this.historyStateListeners) {
          listener(parsed.state);
        }
        break;
      case "log":
        for (const listener of this.logListeners) {
          listener(parsed.entry);
        }
        break;
      case "metrics":
        for (const listener of this.metricsListeners) {
          listener(parsed.metrics);
        }
        break;
      case "error":
        for (const listener of this.errorListeners) {
          listener(parsed.message);
        }
        break;
      case "pong":
        break;
    }
  }

  private send(message: ClientIpcMessage): void {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(message));
    }
  }

  private setState(next: IpcConnectionState): void {
    if (this.state === next) {
      return;
    }
    this.state = next;
    for (const listener of this.stateListeners) {
      listener(next);
    }
  }

  private scheduleReconnect(): void {
    this.clearReconnectTimer();
    const delay = this.backoffMs;
    this.backoffMs = Math.min(Math.round(this.backoffMs * 1.6), 3000);
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = null;
      this.openSocket();
    }, delay);
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      window.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }
}
