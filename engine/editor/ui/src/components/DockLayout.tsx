import { useRef, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";

export const LAYOUT_STORAGE_KEY = "bevy9.editor.layout.v2";

export interface DockLayoutState {
  leftWidth: number;
  rightWidth: number;
  bottomHeight: number;
  leftCollapsed: boolean;
  rightCollapsed: boolean;
  bottomCollapsed: boolean;
}

export const DEFAULT_LAYOUT_STATE: DockLayoutState = {
  leftWidth: 270,
  rightWidth: 420,
  bottomHeight: 210,
  leftCollapsed: false,
  rightCollapsed: false,
  bottomCollapsed: false,
};

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value));
}

export function loadSavedLayout(): DockLayoutState {
  try {
    const raw = window.localStorage.getItem(LAYOUT_STORAGE_KEY);
    if (!raw) {
      return DEFAULT_LAYOUT_STATE;
    }
    const parsed = JSON.parse(raw) as Partial<DockLayoutState>;
    return {
      leftWidth:
        typeof parsed.leftWidth === "number"
          ? clamp(parsed.leftWidth, 180, 520)
          : DEFAULT_LAYOUT_STATE.leftWidth,
      rightWidth:
        typeof parsed.rightWidth === "number"
          ? clamp(parsed.rightWidth, 280, 720)
          : DEFAULT_LAYOUT_STATE.rightWidth,
      bottomHeight:
        typeof parsed.bottomHeight === "number"
          ? clamp(parsed.bottomHeight, 120, 460)
          : DEFAULT_LAYOUT_STATE.bottomHeight,
      leftCollapsed: Boolean(parsed.leftCollapsed),
      rightCollapsed: Boolean(parsed.rightCollapsed),
      bottomCollapsed: Boolean(parsed.bottomCollapsed),
    };
  } catch {
    return DEFAULT_LAYOUT_STATE;
  }
}

export function persistLayout(state: DockLayoutState): void {
  try {
    window.localStorage.setItem(LAYOUT_STORAGE_KEY, JSON.stringify(state));
  } catch {
    // Ignore storage quota errors cleanly.
  }
}

export type BottomDockTab = "content" | "console";

interface DockLayoutProps {
  layout: DockLayoutState;
  onChange: (next: DockLayoutState) => void;
  left: ReactNode;
  center: ReactNode;
  bottomContentBrowser: ReactNode;
  bottomConsole: ReactNode;
  activeBottomTab: BottomDockTab;
  onChangeBottomTab: (tab: BottomDockTab) => void;
  consoleBadgeCount?: number;
  right: ReactNode;
}

type DragKind = "left" | "right" | "bottom";

export function DockLayout({
  layout,
  onChange,
  left,
  center,
  bottomContentBrowser,
  bottomConsole,
  activeBottomTab,
  onChangeBottomTab,
  consoleBadgeCount = 0,
  right,
}: DockLayoutProps) {
  const dragRef = useRef<{
    kind: DragKind;
    startCoord: number;
    startSize: number;
  } | null>(null);

  function beginDrag(
    kind: DragKind,
    event: ReactPointerEvent<HTMLDivElement>,
    startSize: number,
  ) {
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = {
      kind,
      startCoord: kind === "bottom" ? event.clientY : event.clientX,
      startSize,
    };
  }

  function handlePointerMove(event: ReactPointerEvent<HTMLDivElement>) {
    const drag = dragRef.current;
    if (!drag) {
      return;
    }

    if (drag.kind === "left") {
      const delta = event.clientX - drag.startCoord;
      onChange({
        ...layout,
        leftCollapsed: false,
        leftWidth: clamp(drag.startSize + delta, 180, 520),
      });
    } else if (drag.kind === "right") {
      const delta = drag.startCoord - event.clientX;
      onChange({
        ...layout,
        rightCollapsed: false,
        rightWidth: clamp(drag.startSize + delta, 280, 720),
      });
    } else {
      const delta = drag.startCoord - event.clientY;
      onChange({
        ...layout,
        bottomCollapsed: false,
        bottomHeight: clamp(drag.startSize + delta, 120, 460),
      });
    }
  }

  function endDrag(event: ReactPointerEvent<HTMLDivElement>) {
    if (dragRef.current) {
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId);
      }
      dragRef.current = null;
    }
  }

  return (
    <div className="dock-root">
      {!layout.leftCollapsed ? (
        <>
          <aside className="dock-pane" style={{ width: layout.leftWidth }}>
            {left}
          </aside>
          <div
            className="splitter splitter-v"
            role="separator"
            aria-orientation="vertical"
            title="Drag to resize Outliner"
            onPointerDown={(e) => beginDrag("left", e, layout.leftWidth)}
            onPointerMove={handlePointerMove}
            onPointerUp={endDrag}
            onPointerCancel={endDrag}
          />
        </>
      ) : (
        <button
          type="button"
          className="dock-collapsed-tab"
          title="Expand World Outliner"
          onClick={() => onChange({ ...layout, leftCollapsed: false })}
        >
          Outliner
        </button>
      )}

      <div className="dock-center-column">
        <div className="dock-viewport-slot">{center}</div>

        <div
          className="splitter splitter-h"
          role="separator"
          aria-orientation="horizontal"
          title={
            layout.bottomCollapsed
              ? "Expand Bottom Dock"
              : "Drag to resize Bottom Dock"
          }
          onPointerDown={(e) => beginDrag("bottom", e, layout.bottomHeight)}
          onPointerMove={handlePointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
          onDoubleClick={() =>
            onChange({ ...layout, bottomCollapsed: !layout.bottomCollapsed })
          }
        />

        {!layout.bottomCollapsed ? (
          <section className="dock-bottom-pane" style={{ height: layout.bottomHeight }}>
            <div className="bottom-dock-tabs">
              <button
                type="button"
                className={`bottom-tab-btn ${activeBottomTab === "content" ? "active" : ""}`}
                onClick={() => onChangeBottomTab("content")}
              >
                Content Browser
              </button>
              <button
                type="button"
                className={`bottom-tab-btn ${activeBottomTab === "console" ? "active" : ""}`}
                onClick={() => onChangeBottomTab("console")}
              >
                Console
                {consoleBadgeCount > 0 && (
                  <span className="bottom-tab-badge">{consoleBadgeCount}</span>
                )}
              </button>
              <div className="bottom-tabs-spacer" />
              <button
                type="button"
                className="ghost-btn"
                onClick={() => onChange({ ...layout, bottomCollapsed: true })}
              >
                Hide
              </button>
            </div>
            <div className="bottom-dock-body">
              {activeBottomTab === "content" ? bottomContentBrowser : bottomConsole}
            </div>
          </section>
        ) : (
          <div className="dock-bottom-collapsed">
            <button
              type="button"
              className="ghost-btn"
              onClick={() => {
                onChangeBottomTab("content");
                onChange({ ...layout, bottomCollapsed: false });
              }}
            >
              Show Content Browser
            </button>
            <button
              type="button"
              className="ghost-btn"
              onClick={() => {
                onChangeBottomTab("console");
                onChange({ ...layout, bottomCollapsed: false });
              }}
            >
              Show Console{consoleBadgeCount > 0 ? ` (${consoleBadgeCount})` : ""}
            </button>
          </div>
        )}
      </div>

      {!layout.rightCollapsed ? (
        <>
          <div
            className="splitter splitter-v"
            role="separator"
            aria-orientation="vertical"
            title="Drag to resize Inspector"
            onPointerDown={(e) => beginDrag("right", e, layout.rightWidth)}
            onPointerMove={handlePointerMove}
            onPointerUp={endDrag}
            onPointerCancel={endDrag}
          />
          <aside className="dock-pane" style={{ width: layout.rightWidth }}>
            {right}
          </aside>
        </>
      ) : (
        <button
          type="button"
          className="dock-collapsed-tab"
          title="Expand Inspector"
          onClick={() => onChange({ ...layout, rightCollapsed: false })}
        >
          Inspector
        </button>
      )}
    </div>
  );
}
