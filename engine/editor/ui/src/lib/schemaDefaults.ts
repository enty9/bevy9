import type {
  BrpRegistrySchema,
  BrpSchemaRef,
  BrpSchemaType,
} from "../protocol/types";

const PROTECTED_COMPONENTS = new Set<string>([
  "engine_core::guid::EntityGuid",
  "engine_scene::components::SceneEntity",
  "engine_scene::hydration::HydratedGltfModelRoot",
  "engine_scene::hydration::HydratedTilemapTile",
  "engine_scene::prefab::PrefabInstance",
  "engine_scene::prefab::PrefabMember",
]);

const INTERNAL_RUNTIME_EXACT = new Set<string>([
  "engine_core::guid::EntityGuid",
  "engine_scene::components::SceneEntity",
  "engine_scene::hydration::HydratedGltfModelRoot",
  "engine_scene::hydration::HydratedTilemapTile",
  "engine_scene::prefab::PrefabInstance",
  "engine_scene::prefab::PrefabMember",
  "bevy_transform::components::global_transform::GlobalTransform",
  "bevy_transform::components::transform::TransformTreeChanged",
  "bevy_camera::visibility::InheritedVisibility",
  "bevy_camera::visibility::ViewVisibility",
  "bevy_camera::visibility::VisibilityClass",
  "bevy_ecs::hierarchy::ChildOf",
  "bevy_ecs::hierarchy::Children",
  "bevy_mesh::components::Mesh3d",
  "bevy_mesh::components::Mesh2d",
  "bevy_sprite::sprite::Sprite",
  "bevy_camera::camera::Camera",
  "bevy_camera::camera::CameraMainTextureUsages",
  "bevy_camera::camera::RenderTarget",
  "bevy_camera::components::Camera3d",
  "bevy_camera::components::Camera2d",
  "bevy_camera::projection::Projection",
  "bevy_camera::primitives::Frustum",
  "bevy_camera::primitives::CascadesFrusta",
  "bevy_camera::primitives::CubemapFrusta",
  "bevy_camera::visibility::VisibleEntities",
  "bevy_camera::visibility::CascadesVisibleEntities",
  "bevy_camera::visibility::CubemapVisibleEntities",
  "bevy_light::directional_light::DirectionalLight",
  "bevy_light::point_light::PointLight",
  "bevy_light::cascade::CascadeShadowConfig",
  "bevy_light::cascade::Cascades",
  "bevy_picking::Pickable",
  "bevy_picking::hover::Hovered",
  "bevy_picking::hover::PickingInteraction",
]);

const COMPONENT_PRIORITY: Record<string, number> = {
  "bevy_ecs::name::Name": 0,
  "bevy_camera::visibility::Visibility": 1,
  "bevy_transform::components::transform::Transform": 2,
};

const CURATED_DEFAULTS: Record<string, unknown> = {
  "bevy_ecs::name::Name": "Entity",
  "bevy_camera::visibility::Visibility": "Inherited",
  "bevy_transform::components::transform::Transform": {
    translation: [0, 0, 0],
    rotation: [0, 0, 0, 1],
    scale: [1, 1, 1],
  },
  "engine_scene::components::MeshPrimitive3d": {
    Cube: { size: [1, 1, 1] },
  },
  "engine_scene::components::StandardMaterialDesc": {
    base_color: {
      Srgba: { red: 0.8, green: 0.82, blue: 0.86, alpha: 1.0 },
    },
    base_color_texture: null,
    metallic: 0.0,
    perceptual_roughness: 0.5,
    emissive: { red: 0.0, green: 0.0, blue: 0.0, alpha: 1.0 },
  },
  "engine_scene::components::MeshPrimitive2d": {
    Rectangle: { size: [64, 64] },
  },
  "engine_scene::components::ColorMaterialDesc": {
    color: {
      Srgba: { red: 0.24, green: 0.62, blue: 0.96, alpha: 1.0 },
    },
  },
  "engine_scene::components::SpriteDesc": {
    color: {
      Srgba: { red: 0.95, green: 0.55, blue: 0.25, alpha: 1.0 },
    },
    texture: null,
    atlas_index: null,
    custom_size: [64, 64],
    flip_x: false,
    flip_y: false,
  },
  "engine_scene::components::SceneGltfModel": {
    asset: null,
    scene_index: 0,
    scale_multiplier: 1.0,
  },
  "engine_scene::components::SceneTilemap": {
    asset: null,
    tile_scale: 1.0,
  },
  "engine_core::asset_id::AssetUuid": null,
  "engine_scene::components::SceneCamera3d": {
    fov_degrees: 45.0,
    active: true,
  },
  "engine_scene::components::SceneCamera2d": {
    scale: 1.0,
    active: true,
  },
  "engine_scene::components::SceneDirectionalLight": {
    color: {
      Srgba: { red: 1.0, green: 1.0, blue: 1.0, alpha: 1.0 },
    },
    illuminance: 10000.0,
    shadows_enabled: true,
  },
  "engine_scene::components::ScenePointLight": {
    color: {
      Srgba: { red: 1.0, green: 1.0, blue: 1.0, alpha: 1.0 },
    },
    intensity: 100000.0,
    range: 20.0,
    shadows_enabled: false,
  },
  "bevy_color::color::Color": {
    Srgba: { red: 1.0, green: 1.0, blue: 1.0, alpha: 1.0 },
  },
  "bevy_color::linear_rgba::LinearRgba": {
    red: 1.0,
    green: 1.0,
    blue: 1.0,
    alpha: 1.0,
  },
  "bevy_color::srgba::Srgba": {
    red: 1.0,
    green: 1.0,
    blue: 1.0,
    alpha: 1.0,
  },
  "glam::Vec2": [0, 0],
  "glam::Vec3": [0, 0, 0],
  "glam::Vec4": [0, 0, 0, 0],
  "glam::Quat": [0, 0, 0, 1],
};

export function extractRefTypePath(refObj: BrpSchemaRef | undefined): string | null {
  if (!refObj) {
    return null;
  }
  const raw = refObj.type?.$ref ?? refObj.$ref;
  if (!raw) {
    return null;
  }
  const prefix = "#/$defs/";
  return raw.startsWith(prefix) ? raw.slice(prefix.length) : raw;
}

export function resolveSchemaRef(
  refObj: BrpSchemaRef | undefined,
  schema: BrpRegistrySchema | null,
): { typePath: string; schemaType?: BrpSchemaType } {
  const typePath = extractRefTypePath(refObj) ?? "";
  return {
    typePath,
    schemaType: typePath && schema ? schema[typePath] : undefined,
  };
}

export function isProtectedComponent(typePath: string): boolean {
  return PROTECTED_COMPONENTS.has(typePath);
}

export function isComputedRuntimeComponent(typePath: string): boolean {
  if (INTERNAL_RUNTIME_EXACT.has(typePath)) {
    return true;
  }
  return (
    typePath.startsWith("bevy_pbr::material::MeshMaterial3d<") ||
    typePath.startsWith("bevy_sprite_render::mesh2d::material::MeshMaterial2d<")
  );
}

export function sortComponentPaths(paths: string[]): string[] {
  return [...paths].sort((a, b) => {
    const pa = COMPONENT_PRIORITY[a] ?? (a.startsWith("engine_scene::") ? 10 : 50);
    const pb = COMPONENT_PRIORITY[b] ?? (b.startsWith("engine_scene::") ? 10 : 50);
    if (pa !== pb) {
      return pa - pb;
    }
    return a.localeCompare(b);
  });
}

export function shortComponentName(
  typePath: string,
  schema: BrpRegistrySchema | null,
): string {
  const fromSchema = schema?.[typePath]?.shortPath;
  if (fromSchema) {
    return fromSchema;
  }
  const tail = typePath.split("::").pop();
  return tail || typePath;
}

export function buildDefaultValueForType(
  typePath: string,
  schema: BrpRegistrySchema | null,
  depth = 0,
): unknown {
  if (typePath in CURATED_DEFAULTS) {
    return structuredClone(CURATED_DEFAULTS[typePath]);
  }
  if (depth > 5) {
    return null;
  }

  if (typePath === "f32" || typePath === "f64") {
    return 0.0;
  }
  if (
    typePath === "u8" ||
    typePath === "u16" ||
    typePath === "u32" ||
    typePath === "u64" ||
    typePath === "usize" ||
    typePath === "i8" ||
    typePath === "i16" ||
    typePath === "i32" ||
    typePath === "i64" ||
    typePath === "isize"
  ) {
    return 0;
  }
  if (typePath === "bool") {
    return false;
  }
  if (
    typePath === "alloc::string::String" ||
    typePath === "std::string::String" ||
    typePath === "&str"
  ) {
    return "";
  }
  if (typePath.startsWith("core::option::Option<")) {
    return null;
  }

  const def = schema?.[typePath];
  if (!def) {
    return {};
  }

  if (def.kind === "Struct") {
    const out: Record<string, unknown> = {};
    const props = def.properties ?? {};
    for (const [fieldName, fieldRef] of Object.entries(props)) {
      const childPath = extractRefTypePath(fieldRef);
      out[fieldName] = childPath
        ? buildDefaultValueForType(childPath, schema, depth + 1)
        : null;
    }
    return out;
  }

  if (def.kind === "Enum" && Array.isArray(def.oneOf) && def.oneOf.length > 0) {
    const first = def.oneOf[0];
    if (typeof first === "string") {
      return first;
    }
    return buildDefaultValueForVariant(first, schema, depth + 1);
  }

  if (
    (def.kind === "Tuple" || def.kind === "TupleStruct") &&
    Array.isArray(def.prefixItems)
  ) {
    return def.prefixItems.map((itemRef) => {
      const childPath = extractRefTypePath(itemRef);
      return childPath
        ? buildDefaultValueForType(childPath, schema, depth + 1)
        : null;
    });
  }

  if (def.kind === "List" || def.kind === "Array") {
    return [];
  }

  const scalarType = Array.isArray(def.type) ? def.type[0] : def.type;
  switch (scalarType) {
    case "boolean":
      return false;
    case "number":
    case "integer":
      return 0;
    case "string":
      return "";
    case "array":
      return [];
    case "object":
      return {};
    default:
      return null;
  }
}

export function buildDefaultValueForVariant(
  variant: BrpSchemaType,
  schema: BrpRegistrySchema | null,
  depth = 0,
): unknown {
  const variantName = variant.shortPath ?? "Variant";
  if (!variant.kind) {
    return variantName;
  }
  if (variant.kind === "Struct") {
    const fields: Record<string, unknown> = {};
    for (const [fieldName, fieldRef] of Object.entries(variant.properties ?? {})) {
      const childPath = extractRefTypePath(fieldRef);
      fields[fieldName] = childPath
        ? buildDefaultValueForType(childPath, schema, depth + 1)
        : null;
    }
    return { [variantName]: fields };
  }
  if (variant.kind === "Tuple" && Array.isArray(variant.prefixItems)) {
    if (variant.prefixItems.length === 1) {
      const innerPath = extractRefTypePath(variant.prefixItems[0]);
      return {
        [variantName]: innerPath
          ? buildDefaultValueForType(innerPath, schema, depth + 1)
          : null,
      };
    }
    return {
      [variantName]: variant.prefixItems.map((itemRef) => {
        const innerPath = extractRefTypePath(itemRef);
        return innerPath
          ? buildDefaultValueForType(innerPath, schema, depth + 1)
          : null;
      }),
    };
  }
  return variantName;
}
