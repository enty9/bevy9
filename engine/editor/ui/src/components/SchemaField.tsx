import { useEffect, useRef, useState, type DragEvent as ReactDragEvent } from "react";
import { Plus, RotateCcw, Trash2, X } from "lucide-react";
import {
  clearDraggedAsset,
  isAssetDragActive,
  readDraggedAssetFromEvent,
} from "../lib/assetDrag";
import { buildColorValue, extractColorInfo, hexToRgba, rgbaToHex } from "../lib/color";
import { eulerDegToQuat, quatToEulerDeg } from "../lib/math";
import {
  buildDefaultValueForType,
  buildDefaultValueForVariant,
  resolveSchemaRef,
} from "../lib/schemaDefaults";
import type {
  AssetKind,
  AssetRecord,
  BrpRegistrySchema,
  BrpSchemaType,
} from "../protocol/types";

export interface SceneEntityOption {
  guid: string;
  name: string;
}

interface NumericInputProps {
  value: number;
  step?: number;
  min?: number;
  max?: number;
  disabled?: boolean;
  onChange: (next: number) => void;
}

export function NumericInput({
  value,
  step = 0.05,
  min,
  max,
  disabled,
  onChange,
}: NumericInputProps) {
  const formatted = Number.isFinite(value)
    ? String(Math.round(value * 10000) / 10000)
    : "0";
  const [draft, setDraft] = useState(formatted);
  const [focused, setFocused] = useState(false);

  useEffect(() => {
    if (!focused) {
      setDraft(formatted);
    }
  }, [formatted, focused]);

  function commit(raw: string) {
    const parsed = Number(raw);
    if (Number.isFinite(parsed)) {
      let clamped = parsed;
      if (min !== undefined) {
        clamped = Math.max(min, clamped);
      }
      if (max !== undefined) {
        clamped = Math.min(max, clamped);
      }
      onChange(Object.is(clamped, -0) ? 0 : clamped);
    } else {
      setDraft(formatted);
    }
  }

  return (
    <input
      type="number"
      className="num-input"
      value={focused ? draft : formatted}
      step={step}
      min={min}
      max={max}
      disabled={disabled}
      onFocus={() => {
        setFocused(true);
        setDraft(formatted);
      }}
      onChange={(e) => {
        const raw = e.target.value;
        setDraft(raw);
        const parsed = Number(raw);
        if (raw.trim() !== "" && !raw.endsWith(".") && Number.isFinite(parsed)) {
          let clamped = parsed;
          if (min !== undefined) {
            clamped = Math.max(min, clamped);
          }
          if (max !== undefined) {
            clamped = Math.min(max, clamped);
          }
          onChange(Object.is(clamped, -0) ? 0 : clamped);
        }
      }}
      onBlur={() => {
        setFocused(false);
        commit(draft);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          commit(draft);
          e.currentTarget.blur();
        }
      }}
      onWheel={(e) => {
        if (document.activeElement === e.currentTarget) {
          e.currentTarget.blur();
        }
      }}
    />
  );
}

interface VectorRowProps {
  axes: readonly string[];
  values: number[];
  step?: number;
  min?: number;
  onChange: (next: number[], immediate?: boolean) => void;
}

const AXIS_CLASSES = ["axis-x", "axis-y", "axis-z", "axis-w"] as const;

function VectorRow({ axes, values, step = 0.05, min, onChange }: VectorRowProps) {
  return (
    <div className="vec-row">
      {axes.map((axis, idx) => (
        <label key={axis} className="vec-cell">
          <span className={`vec-tag ${AXIS_CLASSES[idx] ?? ""}`}>{axis}</span>
          <NumericInput
            value={typeof values[idx] === "number" ? values[idx] : 0}
            step={step}
            min={min}
            onChange={(nextVal) => {
              const copy = axes.map((_, i) =>
                typeof values[i] === "number" ? values[i] : 0,
              );
              copy[idx] = nextVal;
              onChange(copy);
            }}
          />
        </label>
      ))}
    </div>
  );
}

interface QuatEulerEditorProps {
  value: unknown;
  onChange: (nextQuat: [number, number, number, number]) => void;
}

function toQuatTuple(raw: unknown): [number, number, number, number] {
  if (Array.isArray(raw) && raw.length >= 4) {
    return [
      Number(raw[0]) || 0,
      Number(raw[1]) || 0,
      Number(raw[2]) || 0,
      Number(raw[3]) ?? 1,
    ];
  }
  return [0, 0, 0, 1];
}

function QuatEulerEditor({ value, onChange }: QuatEulerEditorProps) {
  const quat = toQuatTuple(value);
  const [euler, setEuler] = useState<[number, number, number]>(() =>
    quatToEulerDeg(quat),
  );
  const eulerRef = useRef(euler);
  eulerRef.current = euler;

  const qx = quat[0];
  const qy = quat[1];
  const qz = quat[2];
  const qw = quat[3];

  useEffect(() => {
    const reconstructed = eulerDegToQuat(eulerRef.current);
    const dot =
      reconstructed[0] * qx +
      reconstructed[1] * qy +
      reconstructed[2] * qz +
      reconstructed[3] * qw;
    if (Math.abs(dot) < 0.9999) {
      setEuler(quatToEulerDeg([qx, qy, qz, qw]));
    }
  }, [qx, qy, qz, qw]);

  return (
    <div className="vec-row">
      {(["X", "Y", "Z"] as const).map((axis, idx) => (
        <label key={axis} className="vec-cell">
          <span className={`vec-tag ${AXIS_CLASSES[idx]}`}>{axis}°</span>
          <NumericInput
            value={euler[idx]}
            step={1}
            onChange={(nextDeg) => {
              const nextEuler: [number, number, number] = [
                euler[0],
                euler[1],
                euler[2],
              ];
              nextEuler[idx] = nextDeg;
              setEuler(nextEuler);
              onChange(eulerDegToQuat(nextEuler));
            }}
          />
        </label>
      ))}
    </div>
  );
}

function unwrapOptionInnerPath(typePath: string): string | null {
  const prefix = "core::option::Option<";
  if (typePath.startsWith(prefix) && typePath.endsWith(">")) {
    return typePath.slice(prefix.length, -1);
  }
  const stdPrefix = "std::option::Option<";
  if (typePath.startsWith(stdPrefix) && typePath.endsWith(">")) {
    return typePath.slice(stdPrefix.length, -1);
  }
  return null;
}

function isVecType(typePath: string): 2 | 3 | 4 | null {
  if (typePath === "glam::Vec2" || typePath === "bevy_math::vec2::Vec2") {
    return 2;
  }
  if (typePath === "glam::Vec3" || typePath === "glam::Vec3A" || typePath === "bevy_math::vec3::Vec3") {
    return 3;
  }
  if (typePath === "glam::Vec4" || typePath === "bevy_math::vec4::Vec4") {
    return 4;
  }
  return null;
}

function isColorType(typePath: string): boolean {
  return (
    typePath === "bevy_color::color::Color" ||
    typePath === "bevy_color::linear_rgba::LinearRgba" ||
    typePath === "bevy_color::srgba::Srgba"
  );
}

function inferAssetKindFilter(
  parentTypePath?: string,
  fieldLabel?: string,
): AssetKind | null {
  if (fieldLabel === "base_color_texture" || fieldLabel === "texture") {
    return "image";
  }
  if (parentTypePath?.includes("SceneGltfModel")) {
    return "gltf_model";
  }
  if (parentTypePath?.includes("AudioEmitter") && fieldLabel === "clip") {
    return "audio";
  }
  if (parentTypePath?.includes("ScriptComponent") && fieldLabel === "script") {
    return "script";
  }
  if (parentTypePath?.includes("SceneTilemap")) {
    return "tilemap";
  }
  if (parentTypePath?.includes("Collider2D") && fieldLabel === "asset") {
    return "tilemap";
  }
  if (fieldLabel?.includes("prefab")) {
    return "prefab";
  }
  return null;
}

function readAssetUuidValue(value: unknown): string {
  if (typeof value === "string") {
    return value;
  }
  if (Array.isArray(value) && typeof value[0] === "string") {
    return value[0];
  }
  return "";
}

interface AssetSlotFieldProps {
  label?: string;
  isOptional: boolean;
  value: unknown;
  assets: AssetRecord[];
  kindFilter: AssetKind | null;
  onChange: (next: unknown, immediate?: boolean) => void;
}

function AssetSlotField({
  label,
  isOptional,
  value,
  assets,
  kindFilter,
  onChange,
}: AssetSlotFieldProps) {
  const [dropActive, setDropActive] = useState(false);
  const currentUuid = readAssetUuidValue(value);
  const currentAsset = assets.find((a) => a.uuid === currentUuid) ?? null;

  const compatibleAssets = kindFilter
    ? assets.filter((a) => a.kind === kindFilter)
    : assets;

  const handleDragOver = (e: ReactDragEvent<HTMLDivElement>) => {
    if (!isAssetDragActive(e)) {
      return;
    }
    const candidate = readDraggedAssetFromEvent(e, assets);
    if (candidate && kindFilter && candidate.kind !== kindFilter) {
      return;
    }
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    if (!dropActive) {
      setDropActive(true);
    }
  };

  const handleDrop = (e: ReactDragEvent<HTMLDivElement>) => {
    setDropActive(false);
    const asset = readDraggedAssetFromEvent(e, assets);
    clearDraggedAsset();
    if (!asset) {
      return;
    }
    if (kindFilter && asset.kind !== kindFilter) {
      return;
    }
    e.preventDefault();
    onChange(asset.uuid, true);
  };

  return (
    <div className={label ? "prop-row" : "prop-row prop-row-full"}>
      {label && <span className="prop-label">{label}</span>}
      <div
        className={`asset-slot-control prop-control ${dropActive ? "drop-active" : ""}`}
        onDragOver={handleDragOver}
        onDragLeave={() => setDropActive(false)}
        onDrop={handleDrop}
        title={
          currentAsset
            ? `${currentAsset.relative_path} (${currentAsset.uuid})`
            : currentUuid || "Drop asset from Content Browser or select"
        }
      >
        <select
          className="select-input asset-slot-select"
          value={currentUuid}
          onChange={(e) => {
            const next = e.target.value;
            onChange(next ? next : isOptional ? null : currentUuid, true);
          }}
        >
          <option value="">
            {currentUuid && !currentAsset
              ? `Missing (${currentUuid.slice(0, 8)}…)`
              : "None (Drop asset)"}
          </option>
          {compatibleAssets.map((asset) => (
            <option key={asset.uuid} value={asset.uuid}>
              {asset.relative_path}
            </option>
          ))}
        </select>
        {isOptional && currentUuid !== "" && (
          <button
            type="button"
            className="ghost-btn icon-mini"
            title="Clear asset reference"
            onClick={() => onChange(null, true)}
          >
            <X size={11} />
          </button>
        )}
      </div>
    </div>
  );
}

interface SchemaFieldProps {
  label?: string;
  typePath: string;
  parentTypePath?: string;
  value: unknown;
  schema: BrpRegistrySchema | null;
  assets?: AssetRecord[];
  sceneEntities?: SceneEntityOption[];
  currentEntityGuid?: string;
  onChange: (next: unknown, immediate?: boolean) => void;
}

function formatNumCompact(n: number): string {
  if (!Number.isFinite(n)) return "0";
  return String(Math.round(n * 1000) / 1000);
}

interface CollisionLayersFieldProps {
  value: unknown;
  onChange: (next: unknown, immediate?: boolean) => void;
}

function LayerMaskRow({
  label,
  mask,
  onChange,
}: {
  label: string;
  mask: number;
  onChange: (nextMask: number) => void;
}) {
  const unsignedMask = mask >>> 0;
  return (
    <div className="layer-mask-block">
      <div className="layer-mask-header">
        <span className="prop-label">{label}</span>
        <div className="panel-actions">
          <code className="cb-mono muted">
            0x{unsignedMask.toString(16).toUpperCase().padStart(8, "0")}
          </code>
          <button
            type="button"
            className="ghost-btn icon-mini"
            onClick={() => onChange(4294967295)}
          >
            All
          </button>
          <button
            type="button"
            className="ghost-btn icon-mini"
            onClick={() => onChange(0)}
          >
            None
          </button>
        </div>
      </div>
      <div className="layer-bit-grid">
        {Array.from({ length: 16 }, (_, bit) => {
          const active = ((unsignedMask >>> bit) & 1) === 1;
          return (
            <button
              key={bit}
              type="button"
              className={`layer-bit-btn ${active ? "active" : ""}`}
              title={`Layer ${bit + 1} (bit ${bit})`}
              onClick={() => onChange((unsignedMask ^ (1 << bit)) >>> 0)}
            >
              {bit + 1}
            </button>
          );
        })}
      </div>
    </div>
  );
}

function CollisionLayersField({ value, onChange }: CollisionLayersFieldProps) {
  const obj =
    value && typeof value === "object"
      ? (value as Record<string, unknown>)
      : {};
  const memberships =
    typeof obj.memberships === "number" ? obj.memberships >>> 0 : 4294967295;
  const filter =
    typeof obj.filter === "number" ? obj.filter >>> 0 : 4294967295;

  return (
    <div className="struct-fields">
      <LayerMaskRow
        label="Memberships"
        mask={memberships}
        onChange={(nextMask) =>
          onChange({ memberships: nextMask, filter }, true)
        }
      />
      <LayerMaskRow
        label="Filter"
        mask={filter}
        onChange={(nextMask) =>
          onChange({ memberships, filter: nextMask }, true)
        }
      />
    </div>
  );
}

interface ColliderFieldProps {
  is3d: boolean;
  typePath: string;
  value: unknown;
  schema: BrpRegistrySchema | null;
  assets: AssetRecord[];
  onChange: (next: unknown, immediate?: boolean) => void;
}

function ColliderField({
  is3d,
  typePath,
  value,
  schema,
  assets,
  onChange,
}: ColliderFieldProps) {
  const def = schema?.[typePath];
  const variants =
    def?.kind === "Enum" && Array.isArray(def.oneOf)
      ? def.oneOf.filter((v): v is BrpSchemaType => typeof v !== "string")
      : [];

  let activeVariantName = variants[0]?.shortPath ?? "Cuboid";
  if (typeof value === "string") {
    activeVariantName = value;
  } else if (value && typeof value === "object") {
    const keys = Object.keys(value as Record<string, unknown>);
    if (keys.length > 0) {
      activeVariantName = keys[0];
    }
  }
  const payload =
    value && typeof value === "object"
      ? (value as Record<string, unknown>)[activeVariantName]
      : undefined;
  const minDim = is3d ? 0.001 : 0.5;
  const dimStep = is3d ? 0.05 : 1;

  function renderShapeBody() {
    switch (activeVariantName) {
      case "Cuboid": {
        const arr = Array.isArray(payload)
          ? (payload as number[])
          : is3d
            ? [0.5, 0.5, 0.5]
            : [32, 32];
        const axes = is3d ? (["X", "Y", "Z"] as const) : (["X", "Y"] as const);
        return (
          <>
            <div className="prop-row prop-row-stacked">
              <span className="prop-label">Half Extents</span>
              <VectorRow
                axes={axes}
                values={arr}
                step={dimStep}
                min={minDim}
                onChange={(next) => onChange({ Cuboid: next })}
              />
            </div>
            <div className="collider-meta-note">
              Full size:{" "}
              {axes
                .map((_, idx) =>
                  formatNumCompact((typeof arr[idx] === "number" ? arr[idx] : 0) * 2),
                )
                .join(" × ")}
              {is3d ? " m" : " px"}
            </div>
          </>
        );
      }
      case "Ball": {
        const radius = typeof payload === "number" ? payload : is3d ? 0.5 : 32;
        return (
          <>
            <div className="prop-row">
              <span className="prop-label">Radius</span>
              <NumericInput
                value={radius}
                step={dimStep}
                min={minDim}
                onChange={(next) => onChange({ Ball: next })}
              />
            </div>
            <div className="collider-meta-note">
              Diameter: {formatNumCompact(radius * 2)}
              {is3d ? " m" : " px"}
            </div>
          </>
        );
      }
      case "Capsule":
      case "Cylinder": {
        const arr = Array.isArray(payload)
          ? (payload as number[])
          : [is3d ? 0.5 : 24, is3d ? 0.35 : 16];
        const firstVal = typeof arr[0] === "number" ? arr[0] : 0.5;
        const radiusVal = typeof arr[1] === "number" ? arr[1] : 0.35;
        const firstLabel =
          activeVariantName === "Capsule" ? "Half Segment" : "Half Height";
        const totalHeight =
          activeVariantName === "Capsule"
            ? (firstVal + radiusVal) * 2
            : firstVal * 2;
        return (
          <>
            <div className="prop-row">
              <span className="prop-label">{firstLabel}</span>
              <NumericInput
                value={firstVal}
                step={dimStep}
                min={minDim}
                onChange={(next) =>
                  onChange({ [activeVariantName]: [next, radiusVal] })
                }
              />
            </div>
            <div className="prop-row">
              <span className="prop-label">Radius</span>
              <NumericInput
                value={radiusVal}
                step={dimStep}
                min={minDim}
                onChange={(next) =>
                  onChange({ [activeVariantName]: [firstVal, next] })
                }
              />
            </div>
            <div className="collider-meta-note">
              Height: {formatNumCompact(totalHeight)} · Diameter:{" "}
              {formatNumCompact(radiusVal * 2)}
              {is3d ? " m" : " px"}
            </div>
          </>
        );
      }
      case "Segment": {
        const pair = Array.isArray(payload) ? payload : [[-32, 0], [32, 0]];
        const ptA = Array.isArray(pair[0]) ? (pair[0] as number[]) : [-32, 0];
        const ptB = Array.isArray(pair[1]) ? (pair[1] as number[]) : [32, 0];
        return (
          <>
            <div className="prop-row prop-row-stacked">
              <span className="prop-label">Point A</span>
              <VectorRow
                axes={["X", "Y"]}
                values={ptA}
                step={1}
                onChange={(nextA) => onChange({ Segment: [nextA, ptB] })}
              />
            </div>
            <div className="prop-row prop-row-stacked">
              <span className="prop-label">Point B</span>
              <VectorRow
                axes={["X", "Y"]}
                values={ptB}
                step={1}
                onChange={(nextB) => onChange({ Segment: [ptA, nextB] })}
              />
            </div>
          </>
        );
      }
      case "TilemapCollider": {
        const obj =
          payload && typeof payload === "object"
            ? (payload as Record<string, unknown>)
            : {};
        const assetUuid =
          typeof obj.asset === "string"
            ? obj.asset
            : "00000000-0000-0000-0000-000000000000";
        const tileScale =
          typeof obj.tile_scale === "number" ? obj.tile_scale : 1.0;
        return (
          <>
            <AssetSlotField
              label="Tilemap"
              isOptional={false}
              value={assetUuid}
              assets={assets}
              kindFilter="tilemap"
              onChange={(nextAsset, immediate) =>
                onChange(
                  {
                    TilemapCollider: {
                      asset: nextAsset,
                      tile_scale: tileScale,
                    },
                  },
                  immediate,
                )
              }
            />
            <div className="prop-row">
              <span className="prop-label">Tile Scale</span>
              <NumericInput
                value={tileScale}
                step={0.1}
                min={0.01}
                onChange={(nextScale) =>
                  onChange({
                    TilemapCollider: {
                      asset: assetUuid,
                      tile_scale: nextScale,
                    },
                  })
                }
              />
            </div>
          </>
        );
      }
      case "ConvexPolygon":
      case "Polyline":
      case "ConvexHull": {
        const pts = Array.isArray(payload) ? (payload as number[][]) : [];
        const minPoints =
          activeVariantName === "ConvexHull"
            ? 4
            : activeVariantName === "ConvexPolygon"
              ? 3
              : 2;
        const axes = is3d ? (["X", "Y", "Z"] as const) : (["X", "Y"] as const);
        return (
          <div className="vertex-list">
            <div className="layer-mask-header">
              <span className="collider-meta-note">
                {pts.length} vertices (min {minPoints})
              </span>
              <button
                type="button"
                className="ghost-btn icon-mini"
                onClick={() => {
                  const last = pts[pts.length - 1] ?? (is3d ? [0, 0, 0] : [0, 0]);
                  const nextPt = [...last];
                  nextPt[0] = (nextPt[0] ?? 0) + (is3d ? 0.5 : 16);
                  onChange({ [activeVariantName]: [...pts, nextPt] }, true);
                }}
              >
                <Plus size={11} /> Point
              </button>
            </div>
            {pts.map((pt, idx) => (
              <div key={idx} className="vertex-row">
                <span className="muted" style={{ width: 22 }}>
                  #{idx}
                </span>
                <VectorRow
                  axes={axes}
                  values={Array.isArray(pt) ? pt : []}
                  step={dimStep}
                  onChange={(nextPt) => {
                    const next = pts.map((p, i) => (i === idx ? nextPt : p));
                    onChange({ [activeVariantName]: next });
                  }}
                />
                <button
                  type="button"
                  className="ghost-btn icon-mini"
                  disabled={pts.length <= minPoints}
                  title="Remove vertex"
                  onClick={() => {
                    const next = pts.filter((_, i) => i !== idx);
                    onChange({ [activeVariantName]: next }, true);
                  }}
                >
                  <Trash2 size={11} />
                </button>
              </div>
            ))}
          </div>
        );
      }
      case "TriMesh": {
        const obj =
          payload && typeof payload === "object"
            ? (payload as Record<string, unknown>)
            : {};
        const vertices = Array.isArray(obj.vertices)
          ? (obj.vertices as number[][])
          : [];
        const indices = Array.isArray(obj.indices)
          ? (obj.indices as number[][])
          : [];
        return (
          <div className="vertex-list">
            <div className="collider-meta-note">
              {vertices.length} vertices · {indices.length} triangles
            </div>
            <div className="layer-mask-header">
              <span className="prop-label">Vertices</span>
              <button
                type="button"
                className="ghost-btn icon-mini"
                onClick={() =>
                  onChange(
                    {
                      TriMesh: {
                        vertices: [...vertices, [0, 0, 0]],
                        indices,
                      },
                    },
                    true,
                  )
                }
              >
                <Plus size={11} /> Vertex
              </button>
            </div>
            {vertices.map((v, idx) => (
              <div key={idx} className="vertex-row">
                <span className="muted" style={{ width: 22 }}>
                  v{idx}
                </span>
                <VectorRow
                  axes={["X", "Y", "Z"]}
                  values={Array.isArray(v) ? v : []}
                  step={0.05}
                  onChange={(nextV) => {
                    const nextVerts = vertices.map((item, i) =>
                      i === idx ? nextV : item,
                    );
                    onChange({
                      TriMesh: { vertices: nextVerts, indices },
                    });
                  }}
                />
              </div>
            ))}
          </div>
        );
      }
      case "Heightfield": {
        const obj =
          payload && typeof payload === "object"
            ? (payload as Record<string, unknown>)
            : {};
        const rows =
          typeof obj.rows === "number" ? Math.max(2, Math.floor(obj.rows)) : 2;
        const columns =
          typeof obj.columns === "number"
            ? Math.max(2, Math.floor(obj.columns))
            : 2;
        const heights = Array.isArray(obj.heights)
          ? (obj.heights as number[])
          : [0, 0, 0, 0];
        const scale = Array.isArray(obj.scale)
          ? (obj.scale as number[])
          : [2, 1, 2];

        function updateGrid(nextRows: number, nextCols: number) {
          const total = nextRows * nextCols;
          const nextHeights = Array.from({ length: total }, (_, idx) =>
            typeof heights[idx] === "number" ? heights[idx] : 0,
          );
          onChange(
            {
              Heightfield: {
                rows: nextRows,
                columns: nextCols,
                heights: nextHeights,
                scale,
              },
            },
            true,
          );
        }

        return (
          <div className="struct-fields">
            <div className="prop-row">
              <span className="prop-label">Rows</span>
              <NumericInput
                value={rows}
                step={1}
                min={2}
                max={64}
                onChange={(n) => updateGrid(Math.max(2, Math.round(n)), columns)}
              />
            </div>
            <div className="prop-row">
              <span className="prop-label">Columns</span>
              <NumericInput
                value={columns}
                step={1}
                min={2}
                max={64}
                onChange={(n) => updateGrid(rows, Math.max(2, Math.round(n)))}
              />
            </div>
            <div className="prop-row prop-row-stacked">
              <span className="prop-label">Scale</span>
              <VectorRow
                axes={["X", "Y", "Z"]}
                values={scale}
                step={0.1}
                min={0.01}
                onChange={(nextScale) =>
                  onChange({
                    Heightfield: { rows, columns, heights, scale: nextScale },
                  })
                }
              />
            </div>
            <div className="collider-meta-note">
              Height samples: {rows} × {columns} ({heights.length} entries)
            </div>
          </div>
        );
      }
      default:
        return (
          <pre className="json-preview">{JSON.stringify(value, null, 2)}</pre>
        );
    }
  }

  return (
    <div className="enum-complex-block">
      {variants.length > 0 && (
        <div className="prop-row">
          <span className="prop-label">Shape</span>
          <select
            className="select-input prop-control"
            value={activeVariantName}
            onChange={(e) => {
              const chosen = variants.find(
                (v) => v.shortPath === e.target.value,
              );
              if (!chosen) return;
              const next = buildDefaultValueForVariant(
                chosen,
                schema,
                0,
                typePath,
              );
              if (
                chosen.shortPath === "TilemapCollider" &&
                next &&
                typeof next === "object"
              ) {
                const firstTilemap = assets.find((a) => a.kind === "tilemap");
                if (firstTilemap) {
                  const rec = next as {
                    TilemapCollider: { asset: string; tile_scale: number };
                  };
                  rec.TilemapCollider.asset = firstTilemap.uuid;
                }
              }
              onChange(next, true);
            }}
          >
            {variants.map((variant) => {
              const name = variant.shortPath ?? "Variant";
              return (
                <option key={name} value={name}>
                  {name}
                </option>
              );
            })}
          </select>
        </div>
      )}
      <div className="nested-indent">{renderShapeBody()}</div>
    </div>
  );
}

const SCRIPT_PROP_KINDS = [
  "Float",
  "Int",
  "Bool",
  "String",
  "Vec2",
  "Vec3",
] as const;

type ScriptPropKind = (typeof SCRIPT_PROP_KINDS)[number];

type ScriptPropertyValueJson =
  | { Bool: boolean }
  | { Int: number }
  | { Float: number }
  | { String: string }
  | { Vec2: [number, number] }
  | { Vec3: [number, number, number] };

interface ParsedScriptProp {
  kind: ScriptPropKind;
  boolVal: boolean;
  numVal: number;
  strVal: string;
  vec2Val: [number, number];
  vec3Val: [number, number, number];
}

function defaultScriptPropValue(kind: ScriptPropKind): ScriptPropertyValueJson {
  switch (kind) {
    case "Bool":
      return { Bool: false };
    case "Int":
      return { Int: 0 };
    case "Float":
      return { Float: 0.0 };
    case "String":
      return { String: "" };
    case "Vec2":
      return { Vec2: [0, 0] };
    case "Vec3":
      return { Vec3: [0, 0, 0] };
  }
}

function parseScriptPropValue(raw: unknown): ParsedScriptProp {
  const base: ParsedScriptProp = {
    kind: "Float",
    boolVal: false,
    numVal: 0,
    strVal: "",
    vec2Val: [0, 0],
    vec3Val: [0, 0, 0],
  };
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    return base;
  }
  const rec = raw as Record<string, unknown>;
  if ("Bool" in rec) {
    return { ...base, kind: "Bool", boolVal: Boolean(rec.Bool) };
  }
  if ("Int" in rec) {
    const n = typeof rec.Int === "number" && Number.isFinite(rec.Int) ? Math.trunc(rec.Int) : 0;
    return { ...base, kind: "Int", numVal: n };
  }
  if ("Float" in rec) {
    const n = typeof rec.Float === "number" && Number.isFinite(rec.Float) ? rec.Float : 0;
    return { ...base, kind: "Float", numVal: n };
  }
  if ("String" in rec) {
    return { ...base, kind: "String", strVal: typeof rec.String === "string" ? rec.String : "" };
  }
  if ("Vec2" in rec) {
    const arr = Array.isArray(rec.Vec2) ? rec.Vec2 : [0, 0];
    return {
      ...base,
      kind: "Vec2",
      vec2Val: [Number(arr[0]) || 0, Number(arr[1]) || 0],
    };
  }
  if ("Vec3" in rec) {
    const arr = Array.isArray(rec.Vec3) ? rec.Vec3 : [0, 0, 0];
    return {
      ...base,
      kind: "Vec3",
      vec3Val: [Number(arr[0]) || 0, Number(arr[1]) || 0, Number(arr[2]) || 0],
    };
  }
  return base;
}

function convertScriptPropKind(
  current: ParsedScriptProp,
  target: ScriptPropKind,
): ScriptPropertyValueJson {
  if (current.kind === target) {
    switch (target) {
      case "Bool":
        return { Bool: current.boolVal };
      case "Int":
        return { Int: Math.trunc(current.numVal) };
      case "Float":
        return { Float: current.numVal };
      case "String":
        return { String: current.strVal };
      case "Vec2":
        return { Vec2: current.vec2Val };
      case "Vec3":
        return { Vec3: current.vec3Val };
    }
  }
  switch (target) {
    case "Bool":
      if (current.kind === "Int" || current.kind === "Float") {
        return { Bool: current.numVal !== 0 };
      }
      if (current.kind === "String") {
        return { Bool: current.strVal.trim().toLowerCase() === "true" };
      }
      return { Bool: false };
    case "Int":
      if (current.kind === "Float") {
        return { Int: Math.trunc(current.numVal) };
      }
      if (current.kind === "Bool") {
        return { Int: current.boolVal ? 1 : 0 };
      }
      return { Int: 0 };
    case "Float":
      if (current.kind === "Int") {
        return { Float: current.numVal };
      }
      if (current.kind === "Bool") {
        return { Float: current.boolVal ? 1.0 : 0.0 };
      }
      return { Float: 0.0 };
    case "String":
      if (current.kind === "Bool") {
        return { String: current.boolVal ? "true" : "false" };
      }
      if (current.kind === "Int" || current.kind === "Float") {
        return { String: String(current.numVal) };
      }
      return { String: "" };
    case "Vec2":
      if (current.kind === "Vec3") {
        return { Vec2: [current.vec3Val[0], current.vec3Val[1]] };
      }
      return { Vec2: [0, 0] };
    case "Vec3":
      if (current.kind === "Vec2") {
        return { Vec3: [current.vec2Val[0], current.vec2Val[1], 0] };
      }
      return { Vec3: [0, 0, 0] };
  }
}

function readScriptPropertiesMap(raw: unknown): Record<string, unknown> {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    return {};
  }
  const obj = raw as Record<string, unknown>;
  if (obj.values && typeof obj.values === "object" && !Array.isArray(obj.values)) {
    return obj.values as Record<string, unknown>;
  }
  return {};
}

interface ScriptPropertiesFieldProps {
  value: unknown;
  onChange: (next: { values: Record<string, unknown> }, immediate?: boolean) => void;
}

function ScriptPropertiesField({ value, onChange }: ScriptPropertiesFieldProps) {
  const [newName, setNewName] = useState("");
  const [newKind, setNewKind] = useState<ScriptPropKind>("Float");
  const [addError, setAddError] = useState<string | null>(null);

  const propMap = readScriptPropertiesMap(value);
  const propKeys = Object.keys(propMap).sort((a, b) => a.localeCompare(b));

  function updateProp(key: string, nextVal: ScriptPropertyValueJson, immediate = false) {
    onChange({ values: { ...propMap, [key]: nextVal } }, immediate);
  }

  function removeProp(key: string) {
    const copy = { ...propMap };
    delete copy[key];
    onChange({ values: copy }, true);
  }

  function handleAddProperty() {
    const trimmed = newName.trim();
    if (!trimmed) {
      setAddError("Enter a property name.");
      return;
    }
    if (trimmed in propMap) {
      setAddError(`Property '${trimmed}' already exists.`);
      return;
    }
    setAddError(null);
    onChange({ values: { ...propMap, [trimmed]: defaultScriptPropValue(newKind) } }, true);
    setNewName("");
  }

  return (
    <div className="script-props-section">
      <div className="script-props-header">
        <span className="nested-struct-title">Properties</span>
        <span className="badge">{propKeys.length}</span>
      </div>

      {propKeys.length === 0 ? (
        <div className="muted script-props-empty">
          No properties configured. Add keys below to expose them as <code>props.&lt;name&gt;</code> in Rhai.
        </div>
      ) : (
        <div className="script-props-list">
          {propKeys.map((key) => {
            const parsed = parseScriptPropValue(propMap[key]);
            return (
              <div key={key} className="script-prop-card">
                <div className="script-prop-top">
                  <span className="script-prop-name truncate" title={key}>
                    {key}
                  </span>
                  <div className="script-prop-controls">
                    <select
                      className="select-input script-prop-type"
                      aria-label={`Type for property ${key}`}
                      value={parsed.kind}
                      onChange={(e) => {
                        const targetKind = e.target.value as ScriptPropKind;
                        updateProp(key, convertScriptPropKind(parsed, targetKind), true);
                      }}
                    >
                      {SCRIPT_PROP_KINDS.map((kind) => (
                        <option key={kind} value={kind}>
                          {kind}
                        </option>
                      ))}
                    </select>
                    <button
                      type="button"
                      className="ghost-btn icon-mini danger"
                      title={`Remove property '${key}'`}
                      aria-label={`Remove property ${key}`}
                      onClick={() => removeProp(key)}
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                </div>

                <div className="script-prop-value">
                  {parsed.kind === "Bool" && (
                    <label className="option-toggle">
                      <input
                        type="checkbox"
                        checked={parsed.boolVal}
                        onChange={(e) =>
                          updateProp(key, { Bool: e.target.checked }, true)
                        }
                      />
                      <span className="muted">{parsed.boolVal ? "true" : "false"}</span>
                    </label>
                  )}

                  {parsed.kind === "Int" && (
                    <NumericInput
                      value={parsed.numVal}
                      step={1}
                      onChange={(nextNum) => {
                        const truncated = Math.trunc(nextNum);
                        updateProp(key, { Int: truncated === 0 ? 0 : truncated });
                      }}
                    />
                  )}

                  {parsed.kind === "Float" && (
                    <NumericInput
                      value={parsed.numVal}
                      step={0.1}
                      onChange={(nextNum) => updateProp(key, { Float: nextNum })}
                    />
                  )}

                  {parsed.kind === "String" && (
                    <input
                      type="text"
                      className="text-input script-prop-text"
                      value={parsed.strVal}
                      onChange={(e) => updateProp(key, { String: e.target.value })}
                    />
                  )}

                  {parsed.kind === "Vec2" && (
                    <VectorRow
                      axes={["X", "Y"]}
                      values={parsed.vec2Val}
                      onChange={(next) =>
                        updateProp(key, { Vec2: [next[0] ?? 0, next[1] ?? 0] })
                      }
                    />
                  )}

                  {parsed.kind === "Vec3" && (
                    <VectorRow
                      axes={["X", "Y", "Z"]}
                      values={parsed.vec3Val}
                      onChange={(next) =>
                        updateProp(key, {
                          Vec3: [next[0] ?? 0, next[1] ?? 0, next[2] ?? 0],
                        })
                      }
                    />
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}

      <form
        className="script-prop-add"
        onSubmit={(e) => {
          e.preventDefault();
          handleAddProperty();
        }}
      >
        <input
          type="text"
          className="text-input script-prop-add-name"
          aria-label="New script property name"
          placeholder="New property (e.g. move_speed)"
          value={newName}
          onChange={(e) => {
            setNewName(e.target.value);
            if (addError) setAddError(null);
          }}
        />
        <select
          className="select-input script-prop-add-kind"
          aria-label="New script property type"
          value={newKind}
          onChange={(e) => setNewKind(e.target.value as ScriptPropKind)}
        >
          {SCRIPT_PROP_KINDS.map((kind) => (
            <option key={kind} value={kind}>
              {kind}
            </option>
          ))}
        </select>
        <button type="submit" className="primary" disabled={!newName.trim()}>
          <Plus size={12} />
          Add
        </button>
      </form>
      {addError && (
        <div className="script-prop-error" role="alert">
          {addError}
        </div>
      )}
    </div>
  );
}

interface ScriptComponentFieldProps {
  value: unknown;
  assets: AssetRecord[];
  onChange: (next: unknown, immediate?: boolean) => void;
}

function ScriptComponentField({
  value,
  assets,
  onChange,
}: ScriptComponentFieldProps) {
  const obj =
    value && typeof value === "object" && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {};
  const enabled = typeof obj.enabled === "boolean" ? obj.enabled : true;
  const scriptUuid = readAssetUuidValue(obj.script) || null;
  const scriptPath = typeof obj.script_path === "string" ? obj.script_path : "";
  const properties = { values: readScriptPropertiesMap(obj.properties) };
  const resolvedAsset = scriptUuid
    ? assets.find((a) => a.uuid === scriptUuid) ?? null
    : null;
  const isUnassigned = !scriptUuid && scriptPath.trim().length === 0;

  function emit(patch: Record<string, unknown>, immediate = false) {
    onChange(
      {
        ...obj,
        script: scriptUuid,
        script_path: scriptPath,
        enabled,
        properties,
        ...patch,
      },
      immediate,
    );
  }

  return (
    <div className="script-component-editor">
      <div className="prop-row">
        <span className="prop-label">Enabled</span>
        <label className="option-toggle">
          <input
            type="checkbox"
            checked={enabled}
            onChange={(e) => emit({ enabled: e.target.checked }, true)}
          />
          <span className="muted">{enabled ? "Active" : "Disabled"}</span>
        </label>
      </div>

      <AssetSlotField
        label="Script Asset"
        isOptional
        value={scriptUuid}
        assets={assets}
        kindFilter="script"
        onChange={(nextAsset, immediate) =>
          emit({ script: nextAsset ? readAssetUuidValue(nextAsset) : null }, immediate)
        }
      />

      <div className="prop-row">
        <span className="prop-label" title="Fallback relative path under assets/ when no asset UUID is set">
          Script Path
        </span>
        <div className="script-path-control prop-control">
          <input
            type="text"
            className="text-input"
            placeholder={
              resolvedAsset
                ? `${resolvedAsset.relative_path} (from asset)`
                : "scripts/player_3d.rhai"
            }
            value={scriptPath}
            onChange={(e) => emit({ script_path: e.target.value })}
          />
          {scriptPath !== "" && (
            <button
              type="button"
              className="ghost-btn icon-mini"
              title="Clear script path"
              onClick={() => emit({ script_path: "" }, true)}
            >
              <X size={11} />
            </button>
          )}
        </div>
      </div>

      {isUnassigned && (
        <div className="muted script-hint">
          Select or drop a <code>.rhai</code> script asset above, or enter a relative path (e.g.{" "}
          <code>scripts/player_3d.rhai</code>).
        </div>
      )}

      <ScriptPropertiesField
        value={properties}
        onChange={(nextProps, immediate) => emit({ properties: nextProps }, immediate)}
      />
    </div>
  );
}

export function SchemaField({
  label,
  typePath,
  parentTypePath,
  value,
  schema,
  assets,
  sceneEntities,
  currentEntityGuid,
  onChange,
}: SchemaFieldProps) {
  const def: BrpSchemaType | undefined = schema?.[typePath];
  const propRowClass = label ? "prop-row" : "prop-row prop-row-full";

  // Dedicated marker components
  if (
    typePath === "engine_physics::TriggerVolume2D" ||
    typePath === "engine_physics::TriggerVolume3D"
  ) {
    return (
      <div className="marker-component-note">
        Trigger sensor — detects overlaps without solid collision.
      </div>
    );
  }
  if (typePath === "engine_physics::OneWayPlatform2D") {
    return (
      <div className="marker-component-note">
        One-way platform — blocks 2D platformer character only from above (+Y).
      </div>
    );
  }

  // Dedicated CollisionLayers bitmask editor
  if (typePath === "engine_physics::CollisionLayers") {
    return <CollisionLayersField value={value} onChange={onChange} />;
  }

  // Dedicated Collider2D / Collider3D editor
  if (
    typePath === "engine_physics::Collider3D" ||
    typePath === "engine_physics::Collider2D"
  ) {
    return (
      <ColliderField
        is3d={typePath === "engine_physics::Collider3D"}
        typePath={typePath}
        value={value}
        schema={schema}
        assets={assets ?? []}
        onChange={onChange}
      />
    );
  }

  // Dedicated Rhai ScriptComponent & ScriptProperties editors
  if (typePath === "engine_scripting::component::ScriptComponent") {
    return (
      <ScriptComponentField
        value={value}
        assets={assets ?? []}
        onChange={onChange}
      />
    );
  }
  if (typePath === "engine_scripting::component::ScriptProperties") {
    return <ScriptPropertiesField value={value} onChange={onChange} />;
  }

  // 1. Name
  if (typePath === "bevy_ecs::name::Name") {
    const strVal = typeof value === "string" ? value : "";
    return (
      <div className="prop-row">
        <span className="prop-label">{label ?? "Name"}</span>
        <input
          type="text"
          className="text-input prop-control"
          value={strVal}
          onChange={(e) => onChange(e.target.value)}
        />
      </div>
    );
  }

  // 2. Visibility
  if (typePath === "bevy_camera::visibility::Visibility") {
    const current = typeof value === "string" ? value : "Inherited";
    return (
      <div className="prop-row">
        <span className="prop-label">{label ?? "Visibility"}</span>
        <div className="segmented prop-control">
          {(["Inherited", "Visible", "Hidden"] as const).map((opt) => (
            <button
              key={opt}
              type="button"
              className={`seg-btn ${current === opt ? "active" : ""}`}
              onClick={() => onChange(opt, true)}
            >
              {opt}
            </button>
          ))}
        </div>
      </div>
    );
  }

  // 3. Transform
  if (typePath === "bevy_transform::components::transform::Transform") {
    const tr =
      value && typeof value === "object"
        ? (value as Record<string, unknown>)
        : {};
    const translation = Array.isArray(tr.translation)
      ? (tr.translation as number[])
      : [0, 0, 0];
    const scale = Array.isArray(tr.scale) ? (tr.scale as number[]) : [1, 1, 1];

    return (
      <div className="transform-block">
        <div className="transform-row">
          <span className="prop-label">Location</span>
          <VectorRow
            axes={["X", "Y", "Z"]}
            values={translation}
            onChange={(next) => onChange({ ...tr, translation: next })}
          />
        </div>
        <div className="transform-row">
          <span className="prop-label">Rotation</span>
          <QuatEulerEditor
            value={tr.rotation}
            onChange={(nextQuat) => onChange({ ...tr, rotation: nextQuat })}
          />
        </div>
        <div className="transform-row">
          <span className="prop-label">Scale</span>
          <VectorRow
            axes={["X", "Y", "Z"]}
            values={scale}
            onChange={(next) => onChange({ ...tr, scale: next })}
          />
        </div>
        <div className="transform-footer">
          <button
            type="button"
            className="ghost-btn"
            title="Reset Transform to identity"
            onClick={() =>
              onChange(
                {
                  translation: [0, 0, 0],
                  rotation: [0, 0, 0, 1],
                  scale: [1, 1, 1],
                },
                true,
              )
            }
          >
            <RotateCcw size={11} />
            Reset Transform
          </button>
        </div>
      </div>
    );
  }

  // 4. Vectors & Quat
  const stackedRowClass = label ? "prop-row prop-row-stacked" : "prop-row prop-row-full";
  const vecDims = isVecType(typePath);
  if (vecDims !== null) {
    const arr = Array.isArray(value) ? (value as number[]) : [];
    const axes =
      vecDims === 2
        ? (["X", "Y"] as const)
        : vecDims === 3
          ? (["X", "Y", "Z"] as const)
          : (["X", "Y", "Z", "W"] as const);
    return (
      <div className={stackedRowClass}>
        {label && <span className="prop-label">{label}</span>}
        <VectorRow
          axes={axes}
          values={arr}
          onChange={(next) => onChange(next)}
        />
      </div>
    );
  }

  if (typePath === "glam::Quat" || typePath === "bevy_math::quat::Quat") {
    return (
      <div className={stackedRowClass}>
        {label && <span className="prop-label">{label}</span>}
        <QuatEulerEditor value={value} onChange={(q) => onChange(q)} />
      </div>
    );
  }

  // 5. Color / LinearRgba / Srgba
  if (isColorType(typePath)) {
    const info = extractColorInfo(value, typePath);
    const hex = rgbaToHex(info.rgba, info.isLinear);
    return (
      <div className="color-editor-block">
        <div className={propRowClass}>
          {label && <span className="prop-label">{label}</span>}
          <div className="color-swatch-row">
            <input
              type="color"
              className="color-picker"
              value={hex}
              onChange={(e) => {
                const nextRgba = hexToRgba(
                  e.target.value,
                  info.rgba.alpha,
                  info.isLinear,
                );
                onChange(buildColorValue(info, nextRgba));
              }}
            />
            <span className="muted">
              {info.variantName} ({hex.toUpperCase()})
            </span>
          </div>
        </div>
        <div className="rgba-row">
          {(["red", "green", "blue", "alpha"] as const).map((channel, idx) => (
            <label key={channel} className="rgba-cell">
              <span className={`vec-tag ${AXIS_CLASSES[idx]}`}>
                {channel[0].toUpperCase()}
              </span>
              <NumericInput
                value={info.rgba[channel]}
                step={0.02}
                min={0}
                max={channel === "alpha" ? 1 : undefined}
                onChange={(nextNum) => {
                  const nextRgba = { ...info.rgba, [channel]: nextNum };
                  onChange(buildColorValue(info, nextRgba));
                }}
              />
            </label>
          ))}
        </div>
      </div>
    );
  }

  // 6. AssetUuid / Option<AssetUuid> picker & drop slot
  const optionInner = unwrapOptionInnerPath(typePath);
  if (
    typePath === "engine_core::asset_id::AssetUuid" ||
    optionInner === "engine_core::asset_id::AssetUuid"
  ) {
    return (
      <AssetSlotField
        label={label}
        isOptional={optionInner !== null}
        value={value}
        assets={assets ?? []}
        kindFilter={inferAssetKindFilter(parentTypePath, label)}
        onChange={onChange}
      />
    );
  }

  // 6b. EntityGuid / Option<EntityGuid> target selector
  if (
    typePath === "engine_core::guid::EntityGuid" ||
    optionInner === "engine_core::guid::EntityGuid"
  ) {
    const isOptional = optionInner !== null;
    const currentGuid = readAssetUuidValue(value);
    const candidateEntities = (sceneEntities ?? []).filter(
      (ent) => ent.guid !== currentEntityGuid,
    );
    const hasCurrentInList =
      currentGuid !== "" &&
      candidateEntities.some((ent) => ent.guid === currentGuid);
    return (
      <div className={propRowClass}>
        {label && <span className="prop-label">{label}</span>}
        <select
          className="select-input prop-control"
          value={currentGuid}
          onChange={(e) => {
            const next = e.target.value;
            onChange(next ? next : isOptional ? null : currentGuid, true);
          }}
        >
          {isOptional && <option value="">None (Disabled)</option>}
          {currentGuid !== "" && !hasCurrentInList && (
            <option value={currentGuid}>
              {currentGuid === currentEntityGuid
                ? "Self (Invalid target)"
                : `Missing (${currentGuid.slice(0, 8)}…)`}
            </option>
          )}
          {candidateEntities.map((ent) => (
            <option key={ent.guid} value={ent.guid}>
              {ent.name} ({ent.guid.slice(0, 8)})
            </option>
          ))}
        </select>
      </div>
    );
  }

  // 7. Generic Option<T>
  if (optionInner !== null) {
    const isSome = value !== null && value !== undefined;
    return (
      <div className="option-block">
        <div className="prop-row">
          <span className="prop-label">{label ?? "Option"}</span>
          <label className="option-toggle">
            <input
              type="checkbox"
              checked={isSome}
              onChange={(e) => {
                if (e.target.checked) {
                  onChange(buildDefaultValueForType(optionInner, schema), true);
                } else {
                  onChange(null, true);
                }
              }}
            />
            <span className="muted">{isSome ? "Some" : "None"}</span>
          </label>
        </div>
        {isSome && (
          <div className="nested-indent">
            <SchemaField
              typePath={optionInner}
              parentTypePath={parentTypePath}
              value={value}
              schema={schema}
              assets={assets}
              sceneEntities={sceneEntities}
              currentEntityGuid={currentEntityGuid}
              onChange={onChange}
            />
          </div>
        )}
      </div>
    );
  }

  // 7. Booleans, Numbers, Strings
  if (typePath === "bool" || def?.type === "boolean") {
    return (
      <div className={propRowClass}>
        {label && <span className="prop-label">{label}</span>}
        <label className="option-toggle">
          <input
            type="checkbox"
            checked={Boolean(value)}
            onChange={(e) => onChange(e.target.checked, true)}
          />
          <span className="muted">{value ? "true" : "false"}</span>
        </label>
      </div>
    );
  }

  if (
    typePath === "f32" ||
    typePath === "f64" ||
    typePath === "i8" ||
    typePath === "i16" ||
    typePath === "i32" ||
    typePath === "i64" ||
    typePath === "isize" ||
    typePath === "u8" ||
    typePath === "u16" ||
    typePath === "u32" ||
    typePath === "u64" ||
    typePath === "usize" ||
    def?.type === "number" ||
    def?.type === "integer"
  ) {
    const numVal = typeof value === "number" ? value : 0;
    const isUnitSlider =
      label === "metallic" ||
      label === "perceptual_roughness" ||
      label === "restitution" ||
      (parentTypePath === "engine_audio::AudioEmitter" &&
        (label === "gain" || label === "gain_variation"));
    const isPitchSlider =
      parentTypePath === "engine_audio::AudioEmitter" && label === "pitch";
    const isNonNegative =
      label === "min_distance" ||
      label === "max_distance" ||
      label === "friction" ||
      label === "density" ||
      label === "pitch_variation" ||
      label === "slope_degrees" ||
      label === "step_height" ||
      label === "ground_snap";
    const sliderMin = isPitchSlider ? 0.125 : 0;
    const sliderMax = isPitchSlider ? 4.0 : 1;
    const sliderStep = isPitchSlider ? 0.05 : 0.01;
    const hasSlider = isUnitSlider || isPitchSlider;
    const step =
      def?.type === "integer" || typePath.startsWith("u") || typePath.startsWith("i")
        ? 1
        : hasSlider
          ? sliderStep
          : 0.1;

    return (
      <div className={propRowClass}>
        {label && <span className="prop-label">{label}</span>}
        <div className="scalar-control">
          {hasSlider && (
            <input
              type="range"
              className="unit-slider"
              min={sliderMin}
              max={sliderMax}
              step={sliderStep}
              value={Math.max(sliderMin, Math.min(sliderMax, numVal))}
              onChange={(e) => onChange(Number(e.target.value))}
            />
          )}
          <NumericInput
            value={numVal}
            step={step}
            min={hasSlider ? sliderMin : isNonNegative ? 0 : undefined}
            max={hasSlider ? sliderMax : undefined}
            onChange={(nextNum) => onChange(nextNum)}
          />
        </div>
      </div>
    );
  }

  if (
    typePath === "alloc::string::String" ||
    typePath === "std::string::String" ||
    typePath === "&str" ||
    def?.type === "string" && !def.oneOf
  ) {
    return (
      <div className={propRowClass}>
        {label && <span className="prop-label">{label}</span>}
        <input
          type="text"
          className="text-input prop-control"
          value={typeof value === "string" ? value : ""}
          onChange={(e) => onChange(e.target.value)}
        />
      </div>
    );
  }

  // 8. Enums via registry.schema
  if (def?.kind === "Enum" && Array.isArray(def.oneOf) && def.oneOf.length > 0) {
    const first = def.oneOf[0];
    if (typeof first === "string") {
      const variants = def.oneOf.filter((v): v is string => typeof v === "string");
      const current = typeof value === "string" ? value : variants[0] ?? "";
      return (
        <div className={propRowClass}>
          {label && <span className="prop-label">{label}</span>}
          <select
            className="select-input prop-control"
            value={current}
            onChange={(e) => onChange(e.target.value, true)}
          >
            {variants.map((variant) => (
              <option key={variant} value={variant}>
                {variant}
              </option>
            ))}
          </select>
        </div>
      );
    }

    const variants = def.oneOf.filter(
      (v): v is BrpSchemaType => typeof v !== "string",
    );
    let activeVariantName =
      variants[0]?.shortPath ?? "Variant";
    if (typeof value === "string") {
      activeVariantName = value;
    } else if (value && typeof value === "object") {
      const keys = Object.keys(value as Record<string, unknown>);
      if (keys.length > 0) {
        activeVariantName = keys[0];
      }
    }

    const activeVariantSchema = variants.find(
      (v) => v.shortPath === activeVariantName,
    );

    const variantPayload =
      value && typeof value === "object"
        ? (value as Record<string, unknown>)[activeVariantName]
        : undefined;

    return (
      <div className="enum-complex-block">
        <div className="prop-row">
          <span className="prop-label">{label ?? "Variant"}</span>
          <select
            className="select-input prop-control"
            value={activeVariantName}
            onChange={(e) => {
              const chosen = variants.find(
                (v) => v.shortPath === e.target.value,
              );
              if (chosen) {
                onChange(
                  buildDefaultValueForVariant(chosen, schema, 0, typePath),
                  true,
                );
              }
            }}
          >
            {variants.map((variant) => {
              const name = variant.shortPath ?? "Variant";
              return (
                <option key={name} value={name}>
                  {name}
                </option>
              );
            })}
          </select>
        </div>

        {activeVariantSchema?.kind === "Struct" &&
          activeVariantSchema.properties && (
            <div className="nested-indent">
              {Object.entries(activeVariantSchema.properties).map(
                ([fieldName, fieldRef]) => {
                  const child = resolveSchemaRef(fieldRef, schema);
                  const fieldsObj =
                    variantPayload && typeof variantPayload === "object"
                      ? (variantPayload as Record<string, unknown>)
                      : {};
                  return (
                    <SchemaField
                      key={fieldName}
                      label={fieldName}
                      typePath={child.typePath}
                      parentTypePath={typePath}
                      value={fieldsObj[fieldName]}
                      schema={schema}
                      assets={assets}
                      sceneEntities={sceneEntities}
                      currentEntityGuid={currentEntityGuid}
                      onChange={(nextFieldVal, immediate) =>
                        onChange(
                          {
                            [activeVariantName]: {
                              ...fieldsObj,
                              [fieldName]: nextFieldVal,
                            },
                          },
                          immediate,
                        )
                      }
                    />
                  );
                },
              )}
            </div>
          )}

        {activeVariantSchema?.kind === "Tuple" &&
          Array.isArray(activeVariantSchema.prefixItems) && (
            <div className="nested-indent">
              {activeVariantSchema.prefixItems.length === 1 ? (
                <SchemaField
                  typePath={
                    resolveSchemaRef(
                      activeVariantSchema.prefixItems[0],
                      schema,
                    ).typePath
                  }
                  parentTypePath={typePath}
                  value={variantPayload}
                  schema={schema}
                  assets={assets}
                  sceneEntities={sceneEntities}
                  currentEntityGuid={currentEntityGuid}
                  onChange={(nextInner, immediate) =>
                    onChange({ [activeVariantName]: nextInner }, immediate)
                  }
                />
              ) : (
                activeVariantSchema.prefixItems.map((itemRef, idx) => {
                  const child = resolveSchemaRef(itemRef, schema);
                  const arr = Array.isArray(variantPayload)
                    ? [...variantPayload]
                    : [];
                  return (
                    <SchemaField
                      key={idx}
                      label={`[${idx}]`}
                      typePath={child.typePath}
                      parentTypePath={typePath}
                      value={arr[idx]}
                      schema={schema}
                      assets={assets}
                      sceneEntities={sceneEntities}
                      currentEntityGuid={currentEntityGuid}
                      onChange={(nextItem, immediate) => {
                        const copy = [...arr];
                        copy[idx] = nextItem;
                        onChange({ [activeVariantName]: copy }, immediate);
                      }}
                    />
                  );
                })
              )}
            </div>
          )}
      </div>
    );
  }

  // 9. Generic Structs
  if (def?.kind === "Struct") {
    const obj =
      value && typeof value === "object"
        ? (value as Record<string, unknown>)
        : {};
    const entries = Object.entries(def.properties ?? {});
    if (entries.length === 0) {
      return (
        <div className="marker-component-note">
          Marker component (no editable fields)
        </div>
      );
    }

    const content = (
      <div className="struct-fields">
        {entries.map(([fieldName, fieldRef]) => {
          const child = resolveSchemaRef(fieldRef, schema);
          return (
            <SchemaField
              key={fieldName}
              label={fieldName}
              typePath={child.typePath}
              parentTypePath={typePath}
              value={obj[fieldName]}
              schema={schema}
              assets={assets}
              sceneEntities={sceneEntities}
              currentEntityGuid={currentEntityGuid}
              onChange={(nextFieldVal, immediate) =>
                onChange({ ...obj, [fieldName]: nextFieldVal }, immediate)
              }
            />
          );
        })}
      </div>
    );

    if (label) {
      return (
        <div className="nested-struct">
          <div className="nested-struct-title">{label}</div>
          <div className="nested-indent">{content}</div>
        </div>
      );
    }
    return content;
  }

  // 10. Tuple / TupleStruct
  if (def?.kind === "Tuple" || def?.kind === "TupleStruct") {
    const prefixItems = def.prefixItems ?? [];
    if (prefixItems.length === 0) {
      return (
        <div className="marker-component-note">
          Marker component (no editable fields)
        </div>
      );
    }
    const arr = Array.isArray(value) ? value : [value];
    return (
      <div className="struct-fields">
        {prefixItems.map((itemRef, idx) => {
          const child = resolveSchemaRef(itemRef, schema);
          return (
            <SchemaField
              key={idx}
              label={label ? `${label}[${idx}]` : `[${idx}]`}
              typePath={child.typePath}
              parentTypePath={typePath}
              value={arr[idx]}
              schema={schema}
              assets={assets}
              sceneEntities={sceneEntities}
              currentEntityGuid={currentEntityGuid}
              onChange={(nextItem, immediate) => {
                const copy = [...arr];
                copy[idx] = nextItem;
                onChange(copy, immediate);
              }}
            />
          );
        })}
      </div>
    );
  }

  // 11. Fallback JSON preview for opaque/unsupported runtime types
  return (
    <div className="prop-fallback">
      {label && <div className="prop-label">{label}</div>}
      <pre className="json-preview">{JSON.stringify(value, null, 2)}</pre>
    </div>
  );
}
