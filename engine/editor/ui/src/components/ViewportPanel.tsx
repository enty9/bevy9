import {
  useEffect,
  useRef,
  useState,
  type DragEvent as ReactDragEvent,
  type FocusEvent as ReactFocusEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import {
  ChevronDown,
  ExternalLink,
  Magnet,
  Maximize2,
  Monitor,
  Move,
  RotateCw,
} from "lucide-react";
import {
  clearDraggedAsset,
  isAssetDragActive,
  readDraggedAssetFromEvent,
} from "../lib/assetDrag";
import type { IpcConnectionState } from "../protocol/ipcClient";
import type {
  AssetRecord,
  DecodedViewportFrame,
  GizmoMode,
  GizmoSnapConfig,
  GizmoStatePayload,
  InputModifiers,
  PointerButtonCode,
  ViewportCameraMode,
  ViewportInputEvent,
  ViewportMode,
} from "../protocol/types";

export type FrameSubscriber = (listener: (frame: DecodedViewportFrame) => void) => () => void;

interface ViewportPanelProps {
  sceneName: string;
  viewportMode: ViewportMode;
  cameraMode: ViewportCameraMode;
  width: number;
  height: number;
  ipcState: IpcConnectionState;
  gizmoState: GizmoStatePayload;
  assets?: AssetRecord[];
  subscribeFrames: FrameSubscriber;
  onViewportInput: (event: ViewportInputEvent) => void;
  onToggleViewportMode: () => void;
  onChangeGizmoState: (next: GizmoStatePayload) => void;
  onDropAsset?: (asset: AssetRecord, viewportX: number, viewportY: number) => void;
}

function clampDim(value: number): number {
  return Math.max(64, Math.min(4096, Math.round(value)));
}

function extractModifiers(e: {
  shiftKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}): InputModifiers {
  return {
    shift: e.shiftKey,
    ctrl: e.ctrlKey,
    alt: e.altKey,
    meta: e.metaKey,
  };
}

function mapPointerButton(button: number): PointerButtonCode | null {
  switch (button) {
    case 0:
      return "left";
    case 1:
      return "middle";
    case 2:
      return "right";
    default:
      return null;
  }
}

export function ViewportPanel({
  sceneName,
  viewportMode,
  cameraMode,
  width,
  height,
  ipcState,
  gizmoState,
  assets,
  subscribeFrames,
  onViewportInput,
  onToggleViewportMode,
  onChangeGizmoState,
  onDropAsset,
}: ViewportPanelProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const snapMenuRef = useRef<HTMLDivElement | null>(null);
  const lastPointerRef = useRef<{ x: number; y: number } | null>(null);
  const latestFrameRef = useRef<DecodedViewportFrame | null>(null);
  const dirtyFrameRef = useRef(false);
  const receivedOnceRef = useRef(false);
  const lastResizeSentRef = useRef<{ width: number; height: number } | null>(null);

  const onViewportInputRef = useRef(onViewportInput);
  onViewportInputRef.current = onViewportInput;

  const [hasFrame, setHasFrame] = useState(false);
  const [snapPopoverOpen, setSnapPopoverOpen] = useState(false);
  const [assetDropActive, setAssetDropActive] = useState(false);
  const [canvasSize, setCanvasSize] = useState<{ width: number; height: number }>({
    width,
    height,
  });

  useEffect(() => {
    if (!snapPopoverOpen) {
      return;
    }
    function handleOutsidePointer(event: PointerEvent) {
      if (
        snapMenuRef.current &&
        event.target instanceof Node &&
        !snapMenuRef.current.contains(event.target)
      ) {
        setSnapPopoverOpen(false);
      }
    }
    window.addEventListener("pointerdown", handleOutsidePointer);
    return () => {
      window.removeEventListener("pointerdown", handleOutsidePointer);
    };
  }, [snapPopoverOpen]);

  function setGizmoMode(mode: GizmoMode) {
    onChangeGizmoState({ ...gizmoState, mode });
  }

  function toggleGizmoSpace() {
    onChangeGizmoState({
      ...gizmoState,
      space: gizmoState.space === "world" ? "local" : "world",
    });
  }

  function toggleSnapEnabled() {
    onChangeGizmoState({
      ...gizmoState,
      snap: {
        ...gizmoState.snap,
        enabled: !gizmoState.snap.enabled,
      },
    });
  }

  function updateSnapNumber(
    field: Exclude<keyof GizmoSnapConfig, "enabled">,
    rawValue: string,
  ) {
    const parsed = Number(rawValue);
    if (!Number.isFinite(parsed) || parsed <= 0) {
      return;
    }
    onChangeGizmoState({
      ...gizmoState,
      snap: {
        ...gizmoState.snap,
        [field]: parsed,
      },
    });
  }

  // Subscribe to direct frame callbacks and paint inside requestAnimationFrame.
  useEffect(() => {
    if (viewportMode !== "offscreen") {
      receivedOnceRef.current = false;
      setHasFrame(false);
      return;
    }

    const unsubscribe = subscribeFrames((frame) => {
      latestFrameRef.current = frame;
      dirtyFrameRef.current = true;
    });

    let rafId = 0;
    const renderLoop = () => {
      if (dirtyFrameRef.current && latestFrameRef.current && canvasRef.current) {
        const frame = latestFrameRef.current;
        dirtyFrameRef.current = false;
        const canvas = canvasRef.current;
        if (
          canvas.width !== frame.header.width ||
          canvas.height !== frame.header.height
        ) {
          canvas.width = frame.header.width;
          canvas.height = frame.header.height;
          setCanvasSize({
            width: frame.header.width,
            height: frame.header.height,
          });
        }
        const ctx = canvas.getContext("2d", { alpha: false });
        if (ctx) {
          const imageData = new ImageData(
            frame.pixels,
            frame.header.width,
            frame.header.height,
          );
          ctx.putImageData(imageData, 0, 0);
        }
        if (!receivedOnceRef.current) {
          receivedOnceRef.current = true;
          setHasFrame(true);
        }
      }
      rafId = window.requestAnimationFrame(renderLoop);
    };

    rafId = window.requestAnimationFrame(renderLoop);
    return () => {
      unsubscribe();
      window.cancelAnimationFrame(rafId);
    };
  }, [viewportMode, subscribeFrames]);

  // Observe container size with 80ms debounce and sync immediately when returning to offscreen/connected.
  useEffect(() => {
    const container = containerRef.current;
    if (!container || viewportMode !== "offscreen" || ipcState !== "connected") {
      lastResizeSentRef.current = null;
      return;
    }

    let debounceTimer: number | null = null;

    const sendContainerSize = () => {
      const rect = container.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) {
        return;
      }
      const nextW = clampDim(rect.width);
      const nextH = clampDim(rect.height);
      const prev = lastResizeSentRef.current;
      if (prev && prev.width === nextW && prev.height === nextH) {
        return;
      }
      lastResizeSentRef.current = { width: nextW, height: nextH };
      onViewportInputRef.current({
        kind: "resize",
        width: nextW,
        height: nextH,
        scale_factor: 1,
      });
    };

    sendContainerSize();

    const observer = new ResizeObserver(() => {
      if (debounceTimer !== null) {
        window.clearTimeout(debounceTimer);
      }
      debounceTimer = window.setTimeout(() => {
        debounceTimer = null;
        sendContainerSize();
      }, 80);
    });

    observer.observe(container);

    return () => {
      observer.disconnect();
      if (debounceTimer !== null) {
        window.clearTimeout(debounceTimer);
      }
    };
  }, [viewportMode, ipcState]);

  // Register non-passive wheel listener so canvas scroll zooms camera without scrolling page.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || viewportMode !== "offscreen") {
      return;
    }

    function handleWheel(event: WheelEvent) {
      event.preventDefault();
      if (event.deltaY === 0) {
        return;
      }
      const normalizedY =
        -Math.sign(event.deltaY) * Math.min(Math.abs(event.deltaY) / 40, 3.0);
      if (normalizedY === 0) {
        return;
      }
      onViewportInputRef.current({
        kind: "wheel",
        delta_x: 0,
        delta_y: normalizedY,
        modifiers: extractModifiers(event),
      });
    }

    canvas.addEventListener("wheel", handleWheel, { passive: false });
    return () => {
      canvas.removeEventListener("wheel", handleWheel);
    };
  }, [viewportMode]);

  function getCanvasCoords(event: ReactPointerEvent<HTMLCanvasElement>): {
    x: number;
    y: number;
  } {
    const canvas = event.currentTarget;
    const rect = canvas.getBoundingClientRect();
    const scaleX = rect.width > 0 ? canvas.width / rect.width : 1;
    const scaleY = rect.height > 0 ? canvas.height / rect.height : 1;
    return {
      x: (event.clientX - rect.left) * scaleX,
      y: (event.clientY - rect.top) * scaleY,
    };
  }

  function handlePointerDown(event: ReactPointerEvent<HTMLCanvasElement>) {
    event.preventDefault();
    event.currentTarget.focus({ preventScroll: true });
    try {
      event.currentTarget.setPointerCapture(event.pointerId);
    } catch {
      // Ignore synthetic pointer events without active OS pointer capture.
    }

    const pos = getCanvasCoords(event);
    lastPointerRef.current = pos;

    const button = mapPointerButton(event.button);
    if (!button) {
      return;
    }
    onViewportInput({
      kind: "pointer_button",
      button,
      pressed: true,
      x: pos.x,
      y: pos.y,
      modifiers: extractModifiers(event),
    });
  }

  function handlePointerMove(event: ReactPointerEvent<HTMLCanvasElement>) {
    const pos = getCanvasCoords(event);
    const prev = lastPointerRef.current;
    lastPointerRef.current = pos;

    const deltaX = prev ? pos.x - prev.x : 0;
    const deltaY = prev ? pos.y - prev.y : 0;
    if (deltaX === 0 && deltaY === 0) {
      return;
    }

    onViewportInput({
      kind: "pointer_move",
      x: pos.x,
      y: pos.y,
      delta_x: deltaX,
      delta_y: deltaY,
    });
  }

  function handlePointerUp(event: ReactPointerEvent<HTMLCanvasElement>) {
    try {
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId);
      }
    } catch {
      // Ignore release errors.
    }

    const pos = getCanvasCoords(event);
    lastPointerRef.current = pos;

    const button = mapPointerButton(event.button);
    if (!button) {
      return;
    }
    onViewportInput({
      kind: "pointer_button",
      button,
      pressed: false,
      x: pos.x,
      y: pos.y,
      modifiers: extractModifiers(event),
    });
  }

  function handleKeyDown(event: ReactKeyboardEvent<HTMLCanvasElement>) {
    if (event.ctrlKey || event.metaKey) {
      return;
    }
    if (
      event.code === "Space" ||
      event.code.startsWith("Arrow")
    ) {
      event.preventDefault();
    }
    if (event.repeat) {
      return;
    }
    onViewportInput({
      kind: "key",
      code: event.code,
      pressed: true,
      modifiers: extractModifiers(event),
    });
  }

  function handleKeyUp(event: ReactKeyboardEvent<HTMLCanvasElement>) {
    onViewportInput({
      kind: "key",
      code: event.code,
      pressed: false,
      modifiers: extractModifiers(event),
    });
  }

  function handleFocus(_event: ReactFocusEvent<HTMLCanvasElement>) {
    onViewportInput({ kind: "focus_changed", focused: true });
  }

  function handleBlur(_event: ReactFocusEvent<HTMLCanvasElement>) {
    lastPointerRef.current = null;
    onViewportInput({ kind: "focus_changed", focused: false });
  }

  return (
    <div className="panel">
      <div className="panel-header">
        <div className="panel-actions">
          <span>Viewport · {sceneName}</span>
          <span className="badge">{cameraMode.toUpperCase()}</span>
          <span className="muted">
            {canvasSize.width}×{canvasSize.height}
          </span>
        </div>

        <div className="gizmo-bar">
          <div className="segmented" role="group" aria-label="Transform Gizmo Mode">
            <button
              type="button"
              className={gizmoState.mode === "translate" ? "seg-btn active" : "seg-btn"}
              title="Move Gizmo (W)"
              onClick={() => setGizmoMode("translate")}
            >
              <Move size={12} /> Move
            </button>
            <button
              type="button"
              className={gizmoState.mode === "rotate" ? "seg-btn active" : "seg-btn"}
              title="Rotate Gizmo (E)"
              onClick={() => setGizmoMode("rotate")}
            >
              <RotateCw size={12} /> Rotate
            </button>
            <button
              type="button"
              className={gizmoState.mode === "scale" ? "seg-btn active" : "seg-btn"}
              title="Scale Gizmo (R)"
              onClick={() => setGizmoMode("scale")}
            >
              <Maximize2 size={12} /> Scale
            </button>
          </div>

          {cameraMode === "3d" && (
            <button
              type="button"
              className={gizmoState.space === "local" ? "primary" : ""}
              title="Toggle World / Local transform space"
              onClick={toggleGizmoSpace}
            >
              {gizmoState.space === "local" ? "Local" : "World"}
            </button>
          )}

          <div ref={snapMenuRef} className="popover-anchor">
            <div className="segmented">
              <button
                type="button"
                className={gizmoState.snap.enabled ? "seg-btn active" : "seg-btn"}
                title="Toggle grid/angle/scale snapping (Ctrl holds temporary snap)"
                onClick={toggleSnapEnabled}
              >
                <Magnet size={12} /> Snap
              </button>
              <button
                type="button"
                className={snapPopoverOpen ? "seg-btn active" : "seg-btn"}
                title="Configure snap increments"
                onClick={() => setSnapPopoverOpen((open) => !open)}
              >
                <ChevronDown size={12} />
              </button>
            </div>

            {snapPopoverOpen && (
              <div className="popover-card gizmo-snap-popover">
                <div className="popover-title">Gizmo Snapping</div>
                <label className="option-toggle">
                  <input
                    type="checkbox"
                    checked={gizmoState.snap.enabled}
                    onChange={toggleSnapEnabled}
                  />
                  <span>Enable Snap</span>
                </label>
                <label className="snap-row">
                  <span>Move 3D (m)</span>
                  <input
                    type="number"
                    min={0.01}
                    step={0.1}
                    value={gizmoState.snap.translate_step}
                    onChange={(e) =>
                      updateSnapNumber("translate_step", e.target.value)
                    }
                  />
                </label>
                <label className="snap-row">
                  <span>Move 2D (px)</span>
                  <input
                    type="number"
                    min={1}
                    step={1}
                    value={gizmoState.snap.translate_step_2d}
                    onChange={(e) =>
                      updateSnapNumber("translate_step_2d", e.target.value)
                    }
                  />
                </label>
                <label className="snap-row">
                  <span>Rotate (deg)</span>
                  <input
                    type="number"
                    min={1}
                    step={5}
                    value={gizmoState.snap.rotate_step_degrees}
                    onChange={(e) =>
                      updateSnapNumber("rotate_step_degrees", e.target.value)
                    }
                  />
                </label>
                <label className="snap-row">
                  <span>Scale</span>
                  <input
                    type="number"
                    min={0.01}
                    step={0.05}
                    value={gizmoState.snap.scale_step}
                    onChange={(e) =>
                      updateSnapNumber("scale_step", e.target.value)
                    }
                  />
                </label>
              </div>
            )}
          </div>

          <button
            type="button"
            disabled={ipcState !== "connected"}
            title={
              viewportMode === "offscreen"
                ? "Detach viewport into a native Bevy OS window"
                : "Attach viewport back inside the editor canvas"
            }
            onClick={onToggleViewportMode}
          >
            {viewportMode === "offscreen" ? (
              <>
                <ExternalLink size={13} />
                Detach
              </>
            ) : (
              <>
                <Monitor size={13} />
                Attach
              </>
            )}
          </button>
        </div>
      </div>

      <div
        ref={containerRef}
        className={`viewport-container ${assetDropActive ? "asset-drop-active" : ""}`}
        onDragOver={(e: ReactDragEvent<HTMLDivElement>) => {
          if (!onDropAsset || !isAssetDragActive(e)) {
            return;
          }
          e.preventDefault();
          e.dataTransfer.dropEffect = "copy";
          if (!assetDropActive) {
            setAssetDropActive(true);
          }
        }}
        onDragLeave={(e: ReactDragEvent<HTMLDivElement>) => {
          if (
            !e.currentTarget.contains(e.relatedTarget as Node | null) &&
            assetDropActive
          ) {
            setAssetDropActive(false);
          }
        }}
        onDrop={(e: ReactDragEvent<HTMLDivElement>) => {
          setAssetDropActive(false);
          if (!onDropAsset) {
            return;
          }
          const asset = readDraggedAssetFromEvent(e, assets);
          clearDraggedAsset();
          if (!asset) {
            return;
          }
          e.preventDefault();
          const rect = (canvasRef.current ?? containerRef.current)?.getBoundingClientRect();
          const vx = rect
            ? Math.max(0, Math.min(rect.width, e.clientX - rect.left))
            : width * 0.5;
          const vy = rect
            ? Math.max(0, Math.min(rect.height, e.clientY - rect.top))
            : height * 0.5;
          const scaledX =
            rect && rect.width > 0 ? (vx / rect.width) * width : width * 0.5;
          const scaledY =
            rect && rect.height > 0 ? (vy / rect.height) * height : height * 0.5;
          onDropAsset(asset, scaledX, scaledY);
        }}
      >
        {viewportMode === "offscreen" ? (
          <>
            <canvas
              ref={canvasRef}
              tabIndex={0}
              className="viewport-canvas"
              onContextMenu={(e) => e.preventDefault()}
              onPointerDown={handlePointerDown}
              onPointerMove={handlePointerMove}
              onPointerUp={handlePointerUp}
              onPointerCancel={handlePointerUp}
              onKeyDown={handleKeyDown}
              onKeyUp={handleKeyUp}
              onFocus={handleFocus}
              onBlur={handleBlur}
            />

            {assetDropActive && (
              <div className="viewport-drop-overlay">
                Drop asset to spawn in scene
              </div>
            )}

            {!hasFrame && (
              <div className="viewport-overlay-status">
                {ipcState === "connected"
                  ? "Waiting for first rendered offscreen frame..."
                  : `Engine IPC is ${ipcState}. Start Engine Host to stream frames.`}
              </div>
            )}

            <div className="viewport-overlay-legend">
              {cameraMode === "3d"
                ? "3D: LMB Select/Gizmo · W/E/R Gizmo · Ctrl Snap · RMB Orbit+WASD · MMB/Alt+LMB Pan · Wheel Zoom"
                : "2D: LMB Select/Gizmo · W/E/R Gizmo · Ctrl Snap · RMB/MMB Pan · Wheel Zoom"}
            </div>
          </>
        ) : (
          <div className="viewport-detached-card">
            <Monitor size={28} />
            <strong>Viewport Detached to Native Window</strong>
            <span className="muted">
              Navigate directly inside the Bevy OS window. Closing that window
              or clicking Attach returns rendering here.
            </span>
            <button
              type="button"
              className="primary"
              onClick={onToggleViewportMode}
            >
              Attach to Editor
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
