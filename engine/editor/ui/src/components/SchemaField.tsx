import { useEffect, useRef, useState, type DragEvent as ReactDragEvent } from "react";
import { RotateCcw, X } from "lucide-react";
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
    />
  );
}

interface VectorRowProps {
  axes: readonly string[];
  values: number[];
  onChange: (next: number[], immediate?: boolean) => void;
}

const AXIS_CLASSES = ["axis-x", "axis-y", "axis-z", "axis-w"] as const;

function VectorRow({ axes, values, onChange }: VectorRowProps) {
  return (
    <div className="vec-row">
      {axes.map((axis, idx) => (
        <label key={axis} className="vec-cell">
          <span className={`vec-tag ${AXIS_CLASSES[idx] ?? ""}`}>{axis}</span>
          <NumericInput
            value={typeof values[idx] === "number" ? values[idx] : 0}
            step={0.05}
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
  if (parentTypePath?.includes("SceneTilemap")) {
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
    <div className="prop-row">
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
  onChange: (next: unknown, immediate?: boolean) => void;
}

export function SchemaField({
  label,
  typePath,
  parentTypePath,
  value,
  schema,
  assets,
  onChange,
}: SchemaFieldProps) {
  const def: BrpSchemaType | undefined = schema?.[typePath];

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
      <div className="prop-row">
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
      <div className="prop-row">
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
        <div className="prop-row">
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
            <span className="muted" style={{ fontSize: 11 }}>
              {info.variantName} ({hex.toUpperCase()})
            </span>
          </div>
        </div>
        <div className="rgba-row">
          {(["red", "green", "blue", "alpha"] as const).map((channel) => (
            <label key={channel} className="rgba-cell">
              <span className="muted">{channel[0].toUpperCase()}</span>
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
      <div className="prop-row">
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
      label === "metallic" || label === "perceptual_roughness";
    const step =
      def?.type === "integer" || typePath.startsWith("u") || typePath.startsWith("i")
        ? 1
        : isUnitSlider
          ? 0.01
          : 0.1;

    return (
      <div className="prop-row">
        {label && <span className="prop-label">{label}</span>}
        <div className="scalar-control">
          {isUnitSlider && (
            <input
              type="range"
              className="unit-slider"
              min={0}
              max={1}
              step={0.01}
              value={Math.max(0, Math.min(1, numVal))}
              onChange={(e) => onChange(Number(e.target.value))}
            />
          )}
          <NumericInput
            value={numVal}
            step={step}
            min={isUnitSlider ? 0 : undefined}
            max={isUnitSlider ? 1 : undefined}
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
      <div className="prop-row">
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
        <div className="prop-row">
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
                onChange(buildDefaultValueForVariant(chosen, schema), true);
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
  if (def?.kind === "Struct" && def.properties) {
    const obj =
      value && typeof value === "object"
        ? (value as Record<string, unknown>)
        : {};
    const entries = Object.entries(def.properties);
    if (entries.length === 0) {
      return (
        <div className="muted" style={{ fontSize: 11 }}>
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
  if (
    (def?.kind === "Tuple" || def?.kind === "TupleStruct") &&
    Array.isArray(def.prefixItems)
  ) {
    const arr = Array.isArray(value) ? value : [value];
    return (
      <div className="struct-fields">
        {def.prefixItems.map((itemRef, idx) => {
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
