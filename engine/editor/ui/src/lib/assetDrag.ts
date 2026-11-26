import type { DragEvent as ReactDragEvent } from "react";
import type { AssetRecord } from "../protocol/types";

export const ASSET_DRAG_MIME = "application/x-bevy9-asset";

let activeDraggedAsset: AssetRecord | null = null;

export function setDraggedAsset(asset: AssetRecord | null): void {
  activeDraggedAsset = asset;
}

export function getDraggedAsset(): AssetRecord | null {
  return activeDraggedAsset;
}

export function clearDraggedAsset(): void {
  activeDraggedAsset = null;
}

export function isAssetDragActive(e: ReactDragEvent): boolean {
  if (activeDraggedAsset !== null) {
    return true;
  }
  const types = e.dataTransfer?.types;
  if (!types) {
    return false;
  }
  return Array.from(types).includes(ASSET_DRAG_MIME);
}

export function readDraggedAssetFromEvent(
  e: ReactDragEvent,
  assets?: AssetRecord[],
): AssetRecord | null {
  const raw = e.dataTransfer?.getData(ASSET_DRAG_MIME);
  if (raw) {
    try {
      const parsed = JSON.parse(raw) as AssetRecord;
      if (parsed && typeof parsed.uuid === "string") {
        return parsed;
      }
    } catch {
      // Ignore malformed MIME payload and fall back below.
    }
  }
  if (activeDraggedAsset) {
    return activeDraggedAsset;
  }
  const plain = e.dataTransfer?.getData("text/plain")?.trim();
  if (plain && assets) {
    return assets.find((a) => a.uuid === plain) ?? null;
  }
  return null;
}
