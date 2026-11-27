//! Bidirectional conversion between Rhai [`Dynamic`] values, [`serde_json::Value`],
//! [`ScriptPropertyValue`], and Bevy [`AppTypeRegistry`] reflected components.

use std::borrow::Cow;

use bevy_ecs::{
    entity::Entity,
    name::Name,
    reflect::{AppTypeRegistry, ReflectComponent},
    world::World,
};
use bevy_math::{Quat, Vec2, Vec3};
use bevy_reflect::{
    serde::{ReflectSerializer, TypedReflectDeserializer},
    std_traits::ReflectDefault,
    PartialReflect, ReflectMut, ReflectRef, TypeRegistration, TypeRegistry,
};
use engine_core::{AssetUuid, EntityGuid};
use rhai::{Array, Dynamic, ImmutableString, Map};
use serde::de::DeserializeSeed;
use serde_json::{Number, Value};

use crate::component::{ScriptProperties, ScriptPropertyValue};

/// Extracts an `f64` from a Rhai [`Dynamic`] accepting `FLOAT`, `INT`, or `f32`.
#[must_use]
pub fn dynamic_to_f64(value: &Dynamic) -> Option<f64> {
    if let Ok(v) = value.as_float() {
        Some(v)
    } else if let Ok(v) = value.as_int() {
        Some(v as f64)
    } else {
        value.clone().try_cast::<f32>().map(f64::from)
    }
}

/// Extracts an `f32` from a Rhai [`Dynamic`] accepting `FLOAT`, `INT`, or `f32`.
#[must_use]
pub fn dynamic_to_f32(value: &Dynamic) -> Option<f32> {
    dynamic_to_f64(value).map(|v| v as f32)
}

/// Extracts an `i64` from a Rhai [`Dynamic`] accepting `INT` or integral `FLOAT`.
#[must_use]
pub fn dynamic_to_i64(value: &Dynamic) -> Option<i64> {
    if let Ok(v) = value.as_int() {
        Some(v)
    } else if let Ok(v) = value.as_float() {
        if v.is_finite() && v.fract() == 0.0 && v >= (i64::MIN as f64) && v <= (i64::MAX as f64) {
            Some(v as i64)
        } else {
            None
        }
    } else {
        None
    }
}

/// Extracts a [`Vec2`] from a Rhai [`Dynamic`] (`Vec2`, `#{ x, y }`, or `[x, y]`).
#[must_use]
pub fn dynamic_to_vec2(value: &Dynamic) -> Option<Vec2> {
    if let Some(v) = value.clone().try_cast::<Vec2>() {
        return Some(v);
    }
    if let Some(map) = value.clone().try_cast::<Map>() {
        let x = map.get("x").and_then(dynamic_to_f32)?;
        let y = map.get("y").and_then(dynamic_to_f32)?;
        return Some(Vec2::new(x, y));
    }
    if let Some(arr) = value.clone().try_cast::<Array>()
        && arr.len() == 2
    {
        let x = dynamic_to_f32(&arr[0])?;
        let y = dynamic_to_f32(&arr[1])?;
        return Some(Vec2::new(x, y));
    }
    None
}

/// Extracts a [`Vec3`] from a Rhai [`Dynamic`] (`Vec3`, `#{ x, y, z }`, or `[x, y, z]`).
#[must_use]
pub fn dynamic_to_vec3(value: &Dynamic) -> Option<Vec3> {
    if let Some(v) = value.clone().try_cast::<Vec3>() {
        return Some(v);
    }
    if let Some(map) = value.clone().try_cast::<Map>() {
        let x = map.get("x").and_then(dynamic_to_f32)?;
        let y = map.get("y").and_then(dynamic_to_f32)?;
        let z = map.get("z").and_then(dynamic_to_f32)?;
        return Some(Vec3::new(x, y, z));
    }
    if let Some(arr) = value.clone().try_cast::<Array>()
        && arr.len() == 3
    {
        let x = dynamic_to_f32(&arr[0])?;
        let y = dynamic_to_f32(&arr[1])?;
        let z = dynamic_to_f32(&arr[2])?;
        return Some(Vec3::new(x, y, z));
    }
    None
}

/// Extracts a [`Quat`] from a Rhai [`Dynamic`] (`#{ x, y, z, w }` or `[x, y, z, w]`).
#[must_use]
pub fn dynamic_to_quat(value: &Dynamic) -> Option<Quat> {
    if let Some(q) = value.clone().try_cast::<Quat>() {
        return Some(q.normalize());
    }
    if let Some(map) = value.clone().try_cast::<Map>() {
        let x = map.get("x").and_then(dynamic_to_f32)?;
        let y = map.get("y").and_then(dynamic_to_f32)?;
        let z = map.get("z").and_then(dynamic_to_f32)?;
        let w = map.get("w").and_then(dynamic_to_f32)?;
        let q = Quat::from_xyzw(x, y, z, w);
        return (q.is_finite() && q.length_squared() > 1e-10).then(|| q.normalize());
    }
    if let Some(arr) = value.clone().try_cast::<Array>()
        && arr.len() == 4
    {
        let x = dynamic_to_f32(&arr[0])?;
        let y = dynamic_to_f32(&arr[1])?;
        let z = dynamic_to_f32(&arr[2])?;
        let w = dynamic_to_f32(&arr[3])?;
        let q = Quat::from_xyzw(x, y, z, w);
        return (q.is_finite() && q.length_squared() > 1e-10).then(|| q.normalize());
    }
    None
}

/// Converts a [`Quat`] into a Rhai `Map` (`#{ x, y, z, w }`).
pub fn quat_to_dynamic(q: Quat) -> Dynamic {
    let mut map = Map::new();
    map.insert("x".into(), Dynamic::from(f64::from(q.x)));
    map.insert("y".into(), Dynamic::from(f64::from(q.y)));
    map.insert("z".into(), Dynamic::from(f64::from(q.z)));
    map.insert("w".into(), Dynamic::from(f64::from(q.w)));
    Dynamic::from_map(map)
}

/// Converts a [`ScriptPropertyValue`] into a Rhai [`Dynamic`].
pub fn property_value_to_dynamic(value: &ScriptPropertyValue) -> Dynamic {
    match value {
        ScriptPropertyValue::Bool(v) => Dynamic::from(*v),
        ScriptPropertyValue::Int(v) => Dynamic::from(*v),
        ScriptPropertyValue::Float(v) => Dynamic::from(*v),
        ScriptPropertyValue::String(v) => Dynamic::from(v.clone()),
        ScriptPropertyValue::Vec2(v) => Dynamic::from(*v),
        ScriptPropertyValue::Vec3(v) => Dynamic::from(*v),
    }
}

/// Converts a Rhai [`Dynamic`] back to a [`ScriptPropertyValue`], preserving existing
/// numeric variants (for example keeping `Float` when assigned an integer literal in Rhai).
#[must_use]
pub fn dynamic_to_property_value(
    value: &Dynamic,
    existing: Option<&ScriptPropertyValue>,
) -> Option<ScriptPropertyValue> {
    if let Some(ScriptPropertyValue::Float(_)) = existing
        && let Some(f) = dynamic_to_f64(value)
    {
        return Some(ScriptPropertyValue::Float(f));
    }
    if let Some(ScriptPropertyValue::Int(_)) = existing
        && let Some(i) = dynamic_to_i64(value)
    {
        return Some(ScriptPropertyValue::Int(i));
    }
    if let Some(ScriptPropertyValue::Vec2(_)) = existing
        && let Some(v) = dynamic_to_vec2(value)
    {
        return Some(ScriptPropertyValue::Vec2(v));
    }
    if let Some(ScriptPropertyValue::Vec3(_)) = existing
        && let Some(v) = dynamic_to_vec3(value)
    {
        return Some(ScriptPropertyValue::Vec3(v));
    }

    if let Ok(v) = value.as_bool() {
        Some(ScriptPropertyValue::Bool(v))
    } else if let Ok(v) = value.as_int() {
        Some(ScriptPropertyValue::Int(v))
    } else if let Ok(v) = value.as_float() {
        Some(ScriptPropertyValue::Float(v))
    } else if let Ok(v) = value.clone().into_string() {
        Some(ScriptPropertyValue::String(v))
    } else if let Some(v) = value.clone().try_cast::<Vec2>() {
        Some(ScriptPropertyValue::Vec2(v))
    } else {
        value.clone().try_cast::<Vec3>().map(ScriptPropertyValue::Vec3)
    }
}

/// Builds a Rhai `Map` representing all entries in [`ScriptProperties`].
#[must_use]
pub fn properties_to_rhai_map(properties: &ScriptProperties) -> Map {
    let mut map = Map::new();
    for (key, value) in properties.iter() {
        map.insert(key.into(), property_value_to_dynamic(value));
    }
    map
}

/// Synchronizes changes from a Rhai `props` map back into [`ScriptProperties`].
///
/// Returns `true` if any property entry changed.
pub fn sync_rhai_map_into_properties(map: &Map, properties: &mut ScriptProperties) -> bool {
    let mut changed = false;

    let keys_to_remove: Vec<String> = properties
        .values
        .keys()
        .filter(|k| !map.contains_key(k.as_str()))
        .cloned()
        .collect();
    for key in keys_to_remove {
        properties.values.remove(&key);
        changed = true;
    }

    for (key, dyn_val) in map {
        let key_str = key.as_str();
        let existing = properties.values.get(key_str);
        if let Some(converted) = dynamic_to_property_value(dyn_val, existing)
            && existing != Some(&converted)
        {
            properties.values.insert(key_str.to_owned(), converted);
            changed = true;
        }
    }

    changed
}

/// Converts a Rhai [`Dynamic`] value into a [`serde_json::Value`].
#[must_use]
pub fn dynamic_to_json(value: &Dynamic) -> Value {
    if value.is_unit() {
        return Value::Null;
    }
    if let Ok(b) = value.as_bool() {
        return Value::Bool(b);
    }
    if let Ok(i) = value.as_int() {
        return Value::Number(Number::from(i));
    }
    if let Ok(f) = value.as_float() {
        return Number::from_f64(f).map_or(Value::Null, Value::Number);
    }
    if let Ok(s) = value.clone().into_string() {
        return Value::String(s);
    }
    if let Some(v) = value.clone().try_cast::<Vec2>() {
        return serde_json::json!([v.x, v.y]);
    }
    if let Some(v) = value.clone().try_cast::<Vec3>() {
        return serde_json::json!([v.x, v.y, v.z]);
    }
    if let Some(q) = value.clone().try_cast::<Quat>() {
        return serde_json::json!([q.x, q.y, q.z, q.w]);
    }
    if let Some(arr) = value.clone().try_cast::<Array>() {
        return Value::Array(arr.iter().map(dynamic_to_json).collect());
    }
    if let Some(map) = value.clone().try_cast::<Map>() {
        let mut obj = serde_json::Map::with_capacity(map.len());
        for (k, v) in map {
            obj.insert(k.to_string(), dynamic_to_json(&v));
        }
        return Value::Object(obj);
    }
    rhai::serde::from_dynamic::<Value>(value).unwrap_or_else(|_| Value::String(value.to_string()))
}

/// Converts a [`serde_json::Value`] into a Rhai [`Dynamic`].
pub fn json_to_dynamic(value: &Value) -> Dynamic {
    match value {
        Value::Null => Dynamic::UNIT,
        Value::Bool(b) => Dynamic::from(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Dynamic::from(i)
            } else if let Some(u) = n.as_u64() {
                i64::try_from(u).map_or_else(|_| Dynamic::from(u as f64), Dynamic::from)
            } else if let Some(f) = n.as_f64() {
                Dynamic::from(f)
            } else {
                Dynamic::UNIT
            }
        }
        Value::String(s) => Dynamic::from(s.clone()),
        Value::Array(items) => {
            let arr: Array = items.iter().map(json_to_dynamic).collect();
            Dynamic::from_array(arr)
        }
        Value::Object(entries) => {
            let mut map = Map::new();
            for (k, v) in entries {
                map.insert(k.as_str().into(), json_to_dynamic(v));
            }
            Dynamic::from_map(map)
        }
    }
}

/// Converts any reflected Bevy value into a Rhai [`Dynamic`].
pub fn reflect_to_dynamic(value: &dyn PartialReflect, type_registry: &TypeRegistry) -> Dynamic {
    // Fast-path concrete engine & primitive types first.
    if let Some(&v) = value.try_downcast_ref::<bool>() {
        return Dynamic::from(v);
    }
    if let Some(&v) = value.try_downcast_ref::<i8>() {
        return Dynamic::from(i64::from(v));
    }
    if let Some(&v) = value.try_downcast_ref::<i16>() {
        return Dynamic::from(i64::from(v));
    }
    if let Some(&v) = value.try_downcast_ref::<i32>() {
        return Dynamic::from(i64::from(v));
    }
    if let Some(&v) = value.try_downcast_ref::<i64>() {
        return Dynamic::from(v);
    }
    if let Some(&v) = value.try_downcast_ref::<isize>() {
        return Dynamic::from(v as i64);
    }
    if let Some(&v) = value.try_downcast_ref::<u8>() {
        return Dynamic::from(i64::from(v));
    }
    if let Some(&v) = value.try_downcast_ref::<u16>() {
        return Dynamic::from(i64::from(v));
    }
    if let Some(&v) = value.try_downcast_ref::<u32>() {
        return Dynamic::from(i64::from(v));
    }
    if let Some(&v) = value.try_downcast_ref::<u64>() {
        return i64::try_from(v).map_or_else(|_| Dynamic::from(v as f64), Dynamic::from);
    }
    if let Some(&v) = value.try_downcast_ref::<usize>() {
        return i64::try_from(v).map_or_else(|_| Dynamic::from(v as f64), Dynamic::from);
    }
    if let Some(&v) = value.try_downcast_ref::<f32>() {
        return Dynamic::from(f64::from(v));
    }
    if let Some(&v) = value.try_downcast_ref::<f64>() {
        return Dynamic::from(v);
    }
    if let Some(v) = value.try_downcast_ref::<String>() {
        return Dynamic::from(v.clone());
    }
    if let Some(v) = value.try_downcast_ref::<Cow<'static, str>>() {
        return Dynamic::from(v.as_ref().to_owned());
    }
    if let Some(&v) = value.try_downcast_ref::<Vec2>() {
        return Dynamic::from(v);
    }
    if let Some(&v) = value.try_downcast_ref::<Vec3>() {
        return Dynamic::from(v);
    }
    if let Some(&q) = value.try_downcast_ref::<Quat>() {
        return quat_to_dynamic(q);
    }
    if let Some(name) = value.try_downcast_ref::<Name>() {
        return Dynamic::from(name.as_str().to_owned());
    }
    if let Some(guid) = value.try_downcast_ref::<EntityGuid>() {
        return Dynamic::from(guid.to_string());
    }
    if let Some(uuid) = value.try_downcast_ref::<AssetUuid>() {
        return Dynamic::from(uuid.to_string());
    }
    if let Some(&entity) = value.try_downcast_ref::<Entity>() {
        return Dynamic::from(entity.to_bits() as i64);
    }

    match value.reflect_ref() {
        ReflectRef::Struct(s) => {
            let mut map = Map::new();
            for idx in 0..s.field_len() {
                if let (Some(name), Some(field)) = (s.name_at(idx), s.field_at(idx)) {
                    map.insert(name.into(), reflect_to_dynamic(field, type_registry));
                }
            }
            Dynamic::from_map(map)
        }
        ReflectRef::TupleStruct(ts) => {
            if ts.field_len() == 1
                && let Some(inner) = ts.field(0)
            {
                return reflect_to_dynamic(inner, type_registry);
            }
            let arr: Array = (0..ts.field_len())
                .filter_map(|i| ts.field(i))
                .map(|f| reflect_to_dynamic(f, type_registry))
                .collect();
            Dynamic::from_array(arr)
        }
        ReflectRef::Tuple(t) => {
            let arr: Array = (0..t.field_len())
                .filter_map(|i| t.field(i))
                .map(|f| reflect_to_dynamic(f, type_registry))
                .collect();
            Dynamic::from_array(arr)
        }
        ReflectRef::List(list) => {
            let arr: Array = list
                .iter()
                .map(|item| reflect_to_dynamic(item, type_registry))
                .collect();
            Dynamic::from_array(arr)
        }
        ReflectRef::Array(array) => {
            let arr: Array = array
                .iter()
                .map(|item| reflect_to_dynamic(item, type_registry))
                .collect();
            Dynamic::from_array(arr)
        }
        ReflectRef::Map(map) => {
            let mut out = Map::new();
            for (k, v) in map.iter() {
                let key = if let Some(s) = k.try_downcast_ref::<String>() {
                    s.clone()
                } else {
                    reflect_to_dynamic(k, type_registry).to_string()
                };
                out.insert(key.into(), reflect_to_dynamic(v, type_registry));
            }
            Dynamic::from_map(out)
        }
        ReflectRef::Set(set) => {
            let arr: Array = set
                .iter()
                .map(|item| reflect_to_dynamic(item, type_registry))
                .collect();
            Dynamic::from_array(arr)
        }
        ReflectRef::Enum(enum_val) => {
            let is_option = value
                .get_represented_type_info()
                .is_some_and(|ti| ti.type_path().starts_with("core::option::Option<"));
            if is_option && enum_val.variant_name() == "None" && enum_val.field_len() == 0 {
                Dynamic::UNIT
            } else if is_option
                && enum_val.variant_name() == "Some"
                && enum_val.field_len() == 1
                && let Some(inner) = enum_val.field_at(0)
            {
                reflect_to_dynamic(inner, type_registry)
            } else if enum_val.field_len() == 0 {
                Dynamic::from(enum_val.variant_name().to_owned())
            } else {
                let serializer = ReflectSerializer::new(value, type_registry);
                match serde_json::to_value(&serializer) {
                    Ok(Value::Object(obj)) => obj
                        .values()
                        .next()
                        .map_or(Dynamic::UNIT, json_to_dynamic),
                    Ok(other) => json_to_dynamic(&other),
                    Err(_) => Dynamic::from(enum_val.variant_name().to_owned()),
                }
            }
        }
        ReflectRef::Opaque(_) => {
            let serializer = ReflectSerializer::new(value, type_registry);
            match serde_json::to_value(&serializer) {
                Ok(Value::Object(obj)) if obj.len() == 1 => obj
                    .values()
                    .next()
                    .map_or(Dynamic::UNIT, json_to_dynamic),
                Ok(other) => json_to_dynamic(&other),
                Err(_) => Dynamic::UNIT,
            }
        }
        #[allow(unreachable_patterns)]
        _ => Dynamic::UNIT,
    }
}

/// Applies a Rhai [`Dynamic`] value directly onto a mutable reflected Bevy target.
pub fn dynamic_apply_to_reflect(
    target: &mut dyn PartialReflect,
    value: &Dynamic,
    type_registry: &TypeRegistry,
) -> Result<(), String> {
    if let Some(slot) = target.try_downcast_mut::<bool>() {
        *slot = value
            .as_bool()
            .map_err(|_| format!("expected bool, got {}", value.type_name()))?;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<f32>() {
        let v = dynamic_to_f32(value)
            .ok_or_else(|| format!("expected number for f32, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<f64>() {
        let v = dynamic_to_f64(value)
            .ok_or_else(|| format!("expected number for f64, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<i8>() {
        let v = dynamic_to_i64(value)
            .and_then(|i| i8::try_from(i).ok())
            .ok_or_else(|| format!("expected i8 integer, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<i16>() {
        let v = dynamic_to_i64(value)
            .and_then(|i| i16::try_from(i).ok())
            .ok_or_else(|| format!("expected i16 integer, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<i32>() {
        let v = dynamic_to_i64(value)
            .and_then(|i| i32::try_from(i).ok())
            .ok_or_else(|| format!("expected i32 integer, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<i64>() {
        let v = dynamic_to_i64(value)
            .ok_or_else(|| format!("expected i64 integer, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<isize>() {
        let v = dynamic_to_i64(value)
            .and_then(|i| isize::try_from(i).ok())
            .ok_or_else(|| format!("expected isize integer, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<u8>() {
        let v = dynamic_to_i64(value)
            .and_then(|i| u8::try_from(i).ok())
            .ok_or_else(|| format!("expected u8 integer, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<u16>() {
        let v = dynamic_to_i64(value)
            .and_then(|i| u16::try_from(i).ok())
            .ok_or_else(|| format!("expected u16 integer, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<u32>() {
        let v = dynamic_to_i64(value)
            .and_then(|i| u32::try_from(i).ok())
            .ok_or_else(|| format!("expected u32 integer, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<u64>() {
        let v = dynamic_to_i64(value)
            .and_then(|i| u64::try_from(i).ok())
            .ok_or_else(|| format!("expected u64 integer, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<usize>() {
        let v = dynamic_to_i64(value)
            .and_then(|i| usize::try_from(i).ok())
            .ok_or_else(|| format!("expected usize integer, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<String>() {
        let s = value
            .clone()
            .into_string()
            .map_err(|_| format!("expected string, got {}", value.type_name()))?;
        *slot = s;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<Vec2>() {
        let v = dynamic_to_vec2(value)
            .ok_or_else(|| format!("expected Vec2, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<Vec3>() {
        let v = dynamic_to_vec3(value)
            .ok_or_else(|| format!("expected Vec3, got {}", value.type_name()))?;
        *slot = v;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<Quat>() {
        let q = dynamic_to_quat(value)
            .ok_or_else(|| format!("expected Quat, got {}", value.type_name()))?;
        *slot = q;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<Name>() {
        let s = value
            .clone()
            .into_string()
            .map_err(|_| format!("expected string for Name, got {}", value.type_name()))?;
        *slot = Name::new(s);
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<AssetUuid>() {
        let s = value
            .clone()
            .into_string()
            .map_err(|_| format!("expected UUID string, got {}", value.type_name()))?;
        *slot = AssetUuid::parse_str(&s).map_err(|err| format!("invalid AssetUuid '{s}': {err}"))?;
        return Ok(());
    }
    if let Some(slot) = target.try_downcast_mut::<EntityGuid>() {
        let s = value
            .clone()
            .into_string()
            .map_err(|_| format!("expected UUID string, got {}", value.type_name()))?;
        *slot =
            EntityGuid::parse_str(&s).map_err(|err| format!("invalid EntityGuid '{s}': {err}"))?;
        return Ok(());
    }

    // Struct partial-map update support.
    if let Some(map) = value.clone().try_cast::<Map>()
        && let ReflectMut::Struct(struct_mut) = target.reflect_mut()
    {
        for (k, v) in &map {
            let field_name = k.as_str();
            let field = struct_mut
                .field_mut(field_name)
                .ok_or_else(|| format!("unknown struct field `{field_name}`"))?;
            dynamic_apply_to_reflect(field, v, type_registry)?;
        }
        return Ok(());
    }

    // Generic fallback via Bevy TypedReflectDeserializer.
    let type_info = target
        .get_represented_type_info()
        .ok_or_else(|| "target has no represented type info".to_owned())?;
    let registration = type_registry.get(type_info.type_id()).ok_or_else(|| {
        format!(
            "type `{}` is not registered in AppTypeRegistry",
            type_info.type_path()
        )
    })?;
    let json = dynamic_to_json(value);
    let deserialized = TypedReflectDeserializer::new(registration, type_registry)
        .deserialize(&json)
        .map_err(|err| {
            format!(
                "failed to deserialize `{}` from Rhai value: {err}",
                type_info.type_path()
            )
        })?;
    target
        .try_apply(deserialized.as_partial_reflect())
        .map_err(|err| format!("failed to apply value to `{}`: {err}", type_info.type_path()))
}

/// Resolves a component [`TypeRegistration`] and [`ReflectComponent`] by full type path,
/// short type path, or unambiguous suffix (for example `"Transform"` or `"Health"`).
pub fn resolve_component_registration<'a>(
    type_registry: &'a TypeRegistry,
    type_path: &str,
) -> Result<(&'a TypeRegistration, &'a ReflectComponent), String> {
    if let Some(reg) = type_registry.get_with_type_path(type_path) {
        let rc = reg.data::<ReflectComponent>().ok_or_else(|| {
            format!("type `{type_path}` is registered but does not reflect ReflectComponent")
        })?;
        return Ok((reg, rc));
    }

    if let Some(reg) = type_registry.get_with_short_type_path(type_path)
        && let Some(rc) = reg.data::<ReflectComponent>()
    {
        return Ok((reg, rc));
    }

    let suffix = format!("::{type_path}");
    let mut matches: Vec<(&TypeRegistration, &ReflectComponent)> = type_registry
        .iter()
        .filter_map(|reg| {
            let rc = reg.data::<ReflectComponent>()?;
            if reg.type_info().type_path().ends_with(&suffix) {
                Some((reg, rc))
            } else {
                None
            }
        })
        .collect();

    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(format!(
            "unknown reflected component type `{type_path}` in AppTypeRegistry"
        )),
        _ => {
            let names: Vec<&str> = matches
                .iter()
                .map(|(reg, _)| reg.type_info().type_path())
                .collect();
            Err(format!(
                "ambiguous component name `{type_path}`; matches: {}",
                names.join(", ")
            ))
        }
    }
}

fn resolve_nested_field_ref<'a>(
    root: &'a dyn PartialReflect,
    field_path: &str,
) -> Result<&'a dyn PartialReflect, String> {
    let mut current = root;
    for segment in field_path.split('.') {
        if segment.is_empty() {
            return Err(format!("invalid empty field segment in `{field_path}`"));
        }
        current = match current.reflect_ref() {
            ReflectRef::Struct(s) => s
                .field(segment)
                .ok_or_else(|| format!("unknown field `{segment}` in `{field_path}`"))?,
            ReflectRef::TupleStruct(ts) => {
                let idx: usize = segment
                    .parse()
                    .map_err(|_| format!("expected numeric index `{segment}` in `{field_path}`"))?;
                ts.field(idx)
                    .ok_or_else(|| format!("tuple index {idx} out of bounds in `{field_path}`"))?
            }
            ReflectRef::List(list) => {
                let idx: usize = segment
                    .parse()
                    .map_err(|_| format!("expected list index `{segment}` in `{field_path}`"))?;
                list.get(idx)
                    .ok_or_else(|| format!("list index {idx} out of bounds in `{field_path}`"))?
            }
            ReflectRef::Array(arr) => {
                let idx: usize = segment
                    .parse()
                    .map_err(|_| format!("expected array index `{segment}` in `{field_path}`"))?;
                arr.get(idx)
                    .ok_or_else(|| format!("array index {idx} out of bounds in `{field_path}`"))?
            }
            _ => {
                return Err(format!(
                    "cannot index field `{segment}` on non-struct/non-sequence value in `{field_path}`"
                ));
            }
        };
    }
    Ok(current)
}

fn resolve_nested_field_mut<'a>(
    root: &'a mut dyn PartialReflect,
    field_path: &str,
) -> Result<&'a mut dyn PartialReflect, String> {
    let mut current = root;
    for segment in field_path.split('.') {
        if segment.is_empty() {
            return Err(format!("invalid empty field segment in `{field_path}`"));
        }
        current = match current.reflect_mut() {
            ReflectMut::Struct(s) => s
                .field_mut(segment)
                .ok_or_else(|| format!("unknown field `{segment}` in `{field_path}`"))?,
            ReflectMut::TupleStruct(ts) => {
                let idx: usize = segment
                    .parse()
                    .map_err(|_| format!("expected numeric index `{segment}` in `{field_path}`"))?;
                ts.field_mut(idx)
                    .ok_or_else(|| format!("tuple index {idx} out of bounds in `{field_path}`"))?
            }
            ReflectMut::List(list) => {
                let idx: usize = segment
                    .parse()
                    .map_err(|_| format!("expected list index `{segment}` in `{field_path}`"))?;
                list.get_mut(idx)
                    .ok_or_else(|| format!("list index {idx} out of bounds in `{field_path}`"))?
            }
            ReflectMut::Array(arr) => {
                let idx: usize = segment
                    .parse()
                    .map_err(|_| format!("expected array index `{segment}` in `{field_path}`"))?;
                arr.get_mut(idx)
                    .ok_or_else(|| format!("array index {idx} out of bounds in `{field_path}`"))?
            }
            _ => {
                return Err(format!(
                    "cannot mutably index field `{segment}` on non-struct/non-sequence value in `{field_path}`"
                ));
            }
        };
    }
    Ok(current)
}

/// Checks whether `entity` currently carries the reflected component `type_path`.
pub fn has_reflected_component(
    world: &World,
    entity: Entity,
    type_path: &str,
) -> Result<bool, String> {
    let entity_ref = world
        .get_entity(entity)
        .map_err(|_| format!("entity {entity:?} does not exist"))?;
    let Some(registry) = world.get_resource::<AppTypeRegistry>() else {
        return Err("AppTypeRegistry resource is missing".to_owned());
    };
    let guard = registry.read();
    let (_, reflect_component) = resolve_component_registration(&guard, type_path)?;
    Ok(reflect_component.reflect(entity_ref).is_some())
}

/// Reads a reflected component on `entity` into a Rhai [`Dynamic`] (`None` if absent).
pub fn read_component_dynamic(
    world: &World,
    entity: Entity,
    type_path: &str,
) -> Result<Option<Dynamic>, String> {
    let entity_ref = world
        .get_entity(entity)
        .map_err(|_| format!("entity {entity:?} does not exist"))?;
    let Some(registry) = world.get_resource::<AppTypeRegistry>() else {
        return Err("AppTypeRegistry resource is missing".to_owned());
    };
    let guard = registry.read();
    let (_, reflect_component) = resolve_component_registration(&guard, type_path)?;
    let Some(reflected) = reflect_component.reflect(entity_ref) else {
        return Ok(None);
    };
    Ok(Some(reflect_to_dynamic(
        reflected.as_partial_reflect(),
        &guard,
    )))
}

/// Reads a single field (including dot-separated nested paths) of a reflected component on `entity`.
pub fn read_component_field_dynamic(
    world: &World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
) -> Result<Option<Dynamic>, String> {
    let entity_ref = world
        .get_entity(entity)
        .map_err(|_| format!("entity {entity:?} does not exist"))?;
    let Some(registry) = world.get_resource::<AppTypeRegistry>() else {
        return Err("AppTypeRegistry resource is missing".to_owned());
    };
    let guard = registry.read();
    let (_, reflect_component) = resolve_component_registration(&guard, type_path)?;
    let Some(reflected) = reflect_component.reflect(entity_ref) else {
        return Ok(None);
    };
    let field_ref = resolve_nested_field_ref(reflected.as_partial_reflect(), field_path)?;
    Ok(Some(reflect_to_dynamic(field_ref, &guard)))
}

/// Validates that `entity` has mutable reflected component `type_path` with `field_path`.
pub fn validate_component_field_target(
    world: &World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
) -> Result<String, String> {
    let entity_ref = world
        .get_entity(entity)
        .map_err(|_| format!("entity {entity:?} does not exist"))?;
    let Some(registry) = world.get_resource::<AppTypeRegistry>() else {
        return Err("AppTypeRegistry resource is missing".to_owned());
    };
    let guard = registry.read();
    let (reg, reflect_component) = resolve_component_registration(&guard, type_path)?;
    let type_id = reg.type_info().type_id();
    if let Some(comp_id) = world.components().get_id(type_id)
        && let Some(info) = world.components().get_info(comp_id)
        && !info.mutable()
    {
        return Err(format!(
            "component `{}` is immutable and cannot be modified from scripts",
            reg.type_info().type_path()
        ));
    }
    let Some(reflected) = reflect_component.reflect(entity_ref) else {
        return Err(format!(
            "entity {entity:?} does not have component `{}`",
            reg.type_info().type_path()
        ));
    };
    let _ = resolve_nested_field_ref(reflected.as_partial_reflect(), field_path)?;
    Ok(reg.type_info().type_path().to_owned())
}

/// Mutates a single field (including nested dot-separated paths) on a reflected component of `entity`.
pub fn write_component_field_dynamic(
    world: &mut World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
    value: &Dynamic,
) -> Result<(), String> {
    if world.get_entity(entity).is_err() {
        return Err(format!("entity {entity:?} does not exist"));
    }
    let Some(registry) = world.get_resource::<AppTypeRegistry>().cloned() else {
        return Err("AppTypeRegistry resource is missing".to_owned());
    };
    let guard = registry.read();
    let (reg, reflect_component) = resolve_component_registration(&guard, type_path)?;
    let type_id = reg.type_info().type_id();
    if let Some(comp_id) = world.components().get_id(type_id)
        && let Some(info) = world.components().get_info(comp_id)
        && !info.mutable()
    {
        return Err(format!(
            "component `{}` is immutable and cannot be modified from scripts",
            reg.type_info().type_path()
        ));
    }

    let mut entity_mut = world.entity_mut(entity);
    let Some(mut reflected) = reflect_component.reflect_mut(&mut entity_mut) else {
        return Err(format!(
            "entity {entity:?} does not have component `{}`",
            reg.type_info().type_path()
        ));
    };

    let field_mut = resolve_nested_field_mut(reflected.as_partial_reflect_mut(), field_path)?;
    dynamic_apply_to_reflect(field_mut, value, &guard)
}

/// Replaces or inserts a reflected component on `entity` from a Rhai [`Dynamic`].
pub fn write_component_dynamic(
    world: &mut World,
    entity: Entity,
    type_path: &str,
    value: &Dynamic,
) -> Result<(), String> {
    if world.get_entity(entity).is_err() {
        return Err(format!("entity {entity:?} does not exist"));
    }
    let Some(registry) = world.get_resource::<AppTypeRegistry>().cloned() else {
        return Err("AppTypeRegistry resource is missing".to_owned());
    };
    let guard = registry.read();
    let (reg, reflect_component) = resolve_component_registration(&guard, type_path)?;
    let type_id = reg.type_info().type_id();
    if let Some(comp_id) = world.components().get_id(type_id)
        && let Some(info) = world.components().get_info(comp_id)
        && !info.mutable()
    {
        return Err(format!(
            "component `{}` is immutable and cannot be modified from scripts",
            reg.type_info().type_path()
        ));
    }

    let exists = world
        .get_entity(entity)
        .ok()
        .is_some_and(|e| reflect_component.reflect(e).is_some());

    if exists {
        let mut entity_mut = world.entity_mut(entity);
        if let Some(mut reflected) = reflect_component.reflect_mut(&mut entity_mut) {
            return dynamic_apply_to_reflect(reflected.as_partial_reflect_mut(), value, &guard);
        }
    }

    // Component absent: construct from ReflectDefault and apply, or deserialize directly.
    if let Some(reflect_default) = reg.data::<ReflectDefault>() {
        let mut instance = reflect_default.default();
        dynamic_apply_to_reflect(instance.as_partial_reflect_mut(), value, &guard)?;
        let mut entity_mut = world.entity_mut(entity);
        reflect_component.insert(&mut entity_mut, instance.as_partial_reflect(), &guard);
        return Ok(());
    }

    let json = dynamic_to_json(value);
    let deserialized = TypedReflectDeserializer::new(reg, &guard)
        .deserialize(&json)
        .map_err(|err| {
            format!(
                "failed to deserialize component `{}`: {err}",
                reg.type_info().type_path()
            )
        })?;
    let mut entity_mut = world.entity_mut(entity);
    reflect_component.insert(&mut entity_mut, deserialized.as_partial_reflect(), &guard);
    Ok(())
}

/// Converts a Rhai [`ImmutableString`] or `Dynamic` into a `String` for logging.
#[must_use]
pub fn dynamic_to_display_string(value: &Dynamic) -> String {
    if let Some(s) = value.clone().try_cast::<ImmutableString>() {
        s.into_owned()
    } else {
        value.to_string()
    }
}
