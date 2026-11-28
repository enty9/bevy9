//! Comprehensive integration tests for `engine_scripting` (Stages 6.2, 6.3, 6.4).

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use bevy_app::{App, FixedUpdate, PostUpdate, PreUpdate, Update};
use bevy_asset::Assets;
use bevy_ecs::{
    component::Component,
    entity::Entity,
    hierarchy::ChildOf,
    name::Name,
    observer::On,
    reflect::{AppTypeRegistry, ReflectComponent},
    resource::Resource,
    system::ResMut,
};
use bevy_input::keyboard::KeyCode;
use bevy_math::{Quat, Vec2, Vec3};
use bevy_reflect::{
    serde::{ReflectSerializer, TypedReflectDeserializer},
    std_traits::ReflectDefault,
    Reflect,
};
use bevy_time::{Fixed, Time, TimePlugin};
use bevy_transform::{components::Transform, TransformPlugin};
use engine_assets::AssetRegistry;
use engine_audio::{
    AudioBackend, AudioBus, AudioEmitter, AudioEngine, AudioError, AudioSpace, BusSettings,
    PlaySound, VoiceId,
};
use engine_core::{AssetUuid, EngineCorePlugin, EntityGuid, ProjectManifest, WorldMode};
use engine_input::{
    ActionAxis, AxisChannel, EngineInputPlugin, ForwardedInput, InputAction, InputBinding,
    InputContexts, InputDomain, InputMappingAsset, InputMappingContext, InputModifier,
    InputSource, ProjectInputMapping,
};
use engine_physics::{
    BodyKind, CharacterController2D, CharacterController3D, Collider2D, Collider3D,
    EnginePhysicsPlugin, Gravity, PhysicsMaterial, PhysicsWorld2D, PhysicsWorld3D, RigidBody2D,
    RigidBody3D, TriggerVolume3D,
};
use engine_scene::{
    load_level_from_str, save_level_to_string, save_prefab_to_file, EngineScenePlugin,
    LevelMetadata, SceneEntity,
};
use engine_scripting::{
    api::ScriptCallContext,
    backend::{
        ScriptBackend, ScriptCompiledInfo, ScriptError, ScriptHook, ScriptHookArgs, ScriptHookSet,
        ScriptSandboxConfig, ScriptSourceKey,
    },
    diagnostics::{ScriptDiagnostic, ScriptDiagnosticStage},
    hot_reload::{reload_modified_scripts, ScriptHotReloadConfig, ScriptReloadedEvent},
    prelude::*,
};
use serde::de::DeserializeSeed;
use uuid::Uuid;

#[derive(Component, Reflect, Debug, Clone, PartialEq)]
#[reflect(Component, Default)]
struct PlayerStats {
    health: f32,
    stamina: i32,
    invulnerable: bool,
    title: String,
    offset: Vec3,
    nickname: Option<String>,
    bonus_tag: Option<String>,
}

impl Default for PlayerStats {
    fn default() -> Self {
        Self {
            health: 100.0,
            stamina: 50,
            invulnerable: false,
            title: "Hero".to_owned(),
            offset: Vec3::new(1.0, 2.0, 3.0),
            nickname: None,
            bonus_tag: Some("Ace".to_owned()),
        }
    }
}

#[derive(Resource, Default)]
struct RecordedEvents {
    custom: Vec<ScriptCustomEvent>,
    diagnostics: Vec<ScriptDiagnostic>,
    reloads: Vec<ScriptReloadedEvent>,
}

fn build_test_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        TimePlugin,
        TransformPlugin,
        EngineCorePlugin,
        EngineScenePlugin,
        EnginePhysicsPlugin,
        EngineScriptingPlugin,
    ));
    app.register_type::<PlayerStats>();
    app.init_resource::<RecordedEvents>();
    app.insert_resource(ScriptHotReloadConfig::immediate());
    app.add_observer(
        |event: On<ScriptCustomEvent>, mut rec: ResMut<RecordedEvents>| {
            rec.custom.push(event.event().clone());
        },
    );
    app.add_observer(
        |event: On<ScriptDiagnosticEvent>, mut rec: ResMut<RecordedEvents>| {
            rec.diagnostics.push(event.event().0.clone());
        },
    );
    app.add_observer(
        |event: On<ScriptReloadedEvent>, mut rec: ResMut<RecordedEvents>| {
            rec.reloads.push(event.event().clone());
        },
    );
    app
}

fn step_update(app: &mut App, dt_secs: f32) {
    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(Duration::from_secs_f32(dt_secs));
    app.world_mut().run_schedule(Update);
    app.world_mut().run_schedule(PostUpdate);
}

fn step_fixed(app: &mut App, count: usize) {
    for _ in 0..count {
        app.world_mut()
            .resource_mut::<Time<Fixed>>()
            .advance_by(Duration::from_secs_f64(1.0 / 60.0));
        app.world_mut().run_schedule(FixedUpdate);
        app.world_mut().run_schedule(PostUpdate);
    }
}

struct TempTestDir {
    path: PathBuf,
}

impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!("bevy9-{prefix}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create temp test dir");
        Self { path }
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// =============================================================================
// 1. Sandbox protection (infinite loop, memory limits, call depth)
// =============================================================================

#[test]
fn sandbox_terminates_infinite_loop_and_emits_diagnostic() {
    let mut app = build_test_app();
    app.insert_resource(ScriptSandboxConfig {
        max_operations: 5_000,
        ..Default::default()
    });

    let bad_uuid = app
        .world_mut()
        .resource_mut::<ScriptRuntime>()
        .register_inline_script(
            "infinite_loop.rhai",
            r#"
            fn on_update(dt) {
                let i = 0;
                loop {
                    i += 1;
                }
            }
            "#,
        )
        .expect("script should compile");

    let healthy_uuid = app
        .world_mut()
        .resource_mut::<ScriptRuntime>()
        .register_inline_script(
            "healthy_peer.rhai",
            r#"
            fn on_update(dt) {
                transform.translate(1.0, 0.0, 0.0);
            }
            "#,
        )
        .expect("healthy script should compile");

    let bad_guid = EntityGuid::new();
    let bad_entity = app
        .world_mut()
        .spawn((
            Name::new("BadLoopEntity"),
            bad_guid,
            Transform::default(),
            ScriptComponent::from_uuid(bad_uuid),
        ))
        .id();

    let healthy_entity = app
        .world_mut()
        .spawn((
            Name::new("HealthyEntity"),
            Transform::default(),
            ScriptComponent::from_uuid(healthy_uuid),
        ))
        .id();

    let start = Instant::now();
    step_update(&mut app, 0.016);
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "infinite loop must terminate quickly via max_operations (took {elapsed:?})"
    );

    // Host continues running healthy peer scripts cleanly!
    assert!(
        (app.world().get::<Transform>(healthy_entity).unwrap().translation.x - 1.0).abs() < 1e-4
    );

    // Diagnostic recorded & observer triggered with entity attribution + line number.
    let diagnostics = app.world().resource::<ScriptDiagnostics>();
    let entity_diags = diagnostics.for_entity(bad_entity);
    assert_eq!(entity_diags.len(), 1);
    let diag = entity_diags[0];
    assert_eq!(diag.stage, ScriptDiagnosticStage::Runtime);
    assert_eq!(diag.file, "infinite_loop.rhai");
    assert_eq!(diag.entity_name.as_deref(), Some("BadLoopEntity"));
    assert_eq!(diag.entity_guid, Some(bad_guid));
    assert_eq!(diag.hook.as_deref(), Some("on_update"));
    assert!(diag.line.is_some(), "expected source line in diagnostic");
    assert!(
        diag.message.to_lowercase().contains("operation"),
        "expected operation limit message, got: {}",
        diag.message
    );

    assert_eq!(app.world().resource::<RecordedEvents>().diagnostics.len(), 1);
}

#[test]
fn sandbox_enforces_while_true_and_allocation_and_recursion_limits() {
    let mut app = build_test_app();
    app.insert_resource(ScriptSandboxConfig {
        max_operations: 5_000,
        max_array_size: 32,
        max_string_size: 64,
        max_call_levels: 8,
        ..Default::default()
    });

    let scripts = {
        let mut rt = app.world_mut().resource_mut::<ScriptRuntime>();
        let s1 = rt
            .register_inline_script(
                "while_true.rhai",
                "fn on_update(dt) { while true {} }",
            )
            .unwrap();
        let s2 = rt
            .register_inline_script(
                "huge_array.rhai",
                "fn on_update(dt) { let a = []; for i in 0..100 { a.push(i); } }",
            )
            .unwrap();
        let s3 = rt
            .register_inline_script(
                "deep_recurse.rhai",
                "fn boom(n) { boom(n + 1) }\nfn on_update(dt) { boom(0); }",
            )
            .unwrap();
        [
            ("while_true.rhai", s1),
            ("huge_array.rhai", s2),
            ("deep_recurse.rhai", s3),
        ]
    };

    for (name, uuid) in scripts {
        app.world_mut()
            .spawn((Name::new(name), ScriptComponent::from_uuid(uuid)));
    }

    step_update(&mut app, 0.016);

    let diags = app.world().resource::<ScriptDiagnostics>().entries();
    assert_eq!(diags.len(), 3, "all three sandbox violations must be captured");
}

// =============================================================================
// 2. ScriptComponent + ScriptProperties reflection, serde, and .level.ron round-trip
// =============================================================================

#[test]
fn script_component_and_properties_reflection_serde_and_scene_roundtrip() {
    let mut app = build_test_app();
    let script_uuid = AssetUuid::new();

    let original = ScriptComponent::from_uuid(script_uuid)
        .with_enabled(true)
        .with_properties(
            ScriptProperties::new()
                .with("active", true)
                .with("lives", 3_i64)
                .with("speed", 7.5_f64)
                .with("tag", "player_one")
                .with("move_dir", Vec2::new(0.5, -0.25))
                .with("spawn_offset", Vec3::new(1.0, 2.5, -4.0)),
        );

    // 1. JSON serde round-trip
    let json_text = serde_json::to_string(&original).expect("json serialize");
    let json_back: ScriptComponent = serde_json::from_str(&json_text).expect("json deserialize");
    assert_eq!(json_back, original);

    // 2. RON serde round-trip
    let ron_text = ron::to_string(&original).expect("ron serialize");
    let ron_back: ScriptComponent = ron::from_str(&ron_text).expect("ron deserialize");
    assert_eq!(ron_back, original);

    // 3. Bevy AppTypeRegistry ReflectSerializer / TypedReflectDeserializer round-trip
    {
        let registry = app.world().resource::<AppTypeRegistry>().read();
        let registration = registry
            .get_with_short_type_path("ScriptComponent")
            .expect("ScriptComponent must be registered in AppTypeRegistry");
        assert!(registration.data::<ReflectComponent>().is_some());

        let serializer = ReflectSerializer::new(&original, &registry);
        let value = serde_json::to_value(&serializer).expect("reflect serialize");
        let inner = value
            .as_object()
            .and_then(|map| map.values().next())
            .expect("reflected object wrapper");

        let reflected = TypedReflectDeserializer::new(registration, &registry)
            .deserialize(inner)
            .expect("reflect deserialize");
        let concrete = <ScriptComponent as bevy_reflect::FromReflect>::from_reflect(
            reflected.as_partial_reflect(),
        )
        .expect("FromReflect ScriptComponent");
        assert_eq!(concrete, original);
    }

    // 4. Full .level.ron scene save & load round-trip via engine_scene
    let entity_guid = EntityGuid::new();
    app.world_mut().spawn((
        SceneEntity,
        entity_guid,
        Name::new("ScriptedHero"),
        Transform::from_xyz(2.0, 3.0, 4.0),
        original.clone(),
    ));

    let metadata = LevelMetadata {
        name: "ScriptRoundtrip".to_owned(),
        world_mode: WorldMode::Mode3D,
    };
    let saved_ron = save_level_to_string(app.world(), &metadata).expect("save_level_to_string");
    load_level_from_str(app.world_mut(), &saved_ron).expect("load_level_from_str");

    let mut found = None;
    for archetype in app.world().archetypes().iter() {
        for ae in archetype.entities() {
            let e = ae.id();
            if app.world().get::<EntityGuid>(e) == Some(&entity_guid) {
                found = app.world().get::<ScriptComponent>(e).cloned();
            }
        }
    }
    assert_eq!(
        found,
        Some(original),
        "ScriptComponent + ScriptProperties must survive .level.ron round-trip"
    );
}

// =============================================================================
// 3. Lifecycle hooks (on_start, on_update, on_fixed_update, on_collision,
//    on_trigger_enter, on_trigger_exit) + Scope isolation + PIE Pause/Stop reset
// =============================================================================

#[test]
fn lifecycle_hooks_scope_isolation_and_pie_reset() {
    let mut app = build_test_app();
    app.insert_resource(ScriptExecutionOptions::editor()); // Starts disabled!

    let counter_uuid = app
        .world_mut()
        .resource_mut::<ScriptRuntime>()
        .register_inline_script(
            "counter.rhai",
            r#"
            let starts = 0;
            let updates = 0;
            let fixed_ticks = 0;

            fn on_start() {
                starts += 1;
                props.starts = starts;
                events.emit("started", #{ tag: props.tag, starts: starts });
            }

            fn on_update(dt) {
                updates += 1;
                props.updates = updates;
                transform.translate(props.speed * dt, 0.0, 0.0);
            }

            fn on_fixed_update(dt) {
                fixed_ticks += 1;
                props.fixed_ticks = fixed_ticks;
            }
            "#,
        )
        .unwrap();

    let e1 = app
        .world_mut()
        .spawn((
            Name::new("RunnerA"),
            Transform::default(),
            ScriptComponent::from_uuid(counter_uuid)
                .with_property("tag", "A")
                .with_property("speed", 10.0_f64)
                .with_property("starts", 0_i64)
                .with_property("updates", 0_i64)
                .with_property("fixed_ticks", 0_i64),
        ))
        .id();

    let e2 = app
        .world_mut()
        .spawn((
            Name::new("RunnerB"),
            Transform::default(),
            ScriptComponent::from_uuid(counter_uuid)
                .with_property("tag", "B")
                .with_property("speed", -5.0_f64)
                .with_property("starts", 0_i64)
                .with_property("updates", 0_i64)
                .with_property("fixed_ticks", 0_i64),
        ))
        .id();

    // While in Editor mode (`enabled == false`), scripts MUST NOT execute.
    step_update(&mut app, 0.1);
    step_fixed(&mut app, 2);
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(e1)
            .unwrap()
            .properties
            .get_int("starts"),
        Some(0)
    );
    assert!(app.world().resource::<RecordedEvents>().custom.is_empty());

    // Enter Play mode (`enabled = true`).
    app.world_mut()
        .resource_mut::<ScriptExecutionOptions>()
        .enabled = true;

    step_update(&mut app, 0.5);
    step_update(&mut app, 0.5);
    step_fixed(&mut app, 3);

    // on_start ran exactly once per entity; scopes are isolated; transforms moved independently.
    let c1 = app.world().get::<ScriptComponent>(e1).unwrap();
    let c2 = app.world().get::<ScriptComponent>(e2).unwrap();
    assert_eq!(c1.properties.get_int("starts"), Some(1));
    assert_eq!(c1.properties.get_int("updates"), Some(2));
    assert_eq!(c1.properties.get_int("fixed_ticks"), Some(3));
    assert_eq!(c2.properties.get_int("starts"), Some(1));
    assert_eq!(c2.properties.get_int("updates"), Some(2));
    assert_eq!(c2.properties.get_int("fixed_ticks"), Some(3));
    assert!((app.world().get::<Transform>(e1).unwrap().translation.x - 10.0).abs() < 1e-3);
    assert!((app.world().get::<Transform>(e2).unwrap().translation.x - (-5.0)).abs() < 1e-3);

    // Pause (`enabled = false`): does NOT reset runtime state; resuming continues counters!
    app.world_mut()
        .resource_mut::<ScriptExecutionOptions>()
        .enabled = false;
    step_update(&mut app, 0.5);
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(e1)
            .unwrap()
            .properties
            .get_int("updates"),
        Some(2)
    );

    app.world_mut()
        .resource_mut::<ScriptExecutionOptions>()
        .enabled = true;
    step_update(&mut app, 0.5);
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(e1)
            .unwrap()
            .properties
            .get_int("starts"),
        Some(1),
        "resuming from Pause must NOT re-run on_start"
    );
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(e1)
            .unwrap()
            .properties
            .get_int("updates"),
        Some(3)
    );

    // PIE Stop: disable + reset_runtime_state() clears per-entity scopes so next Play runs on_start fresh.
    app.world_mut()
        .resource_mut::<ScriptExecutionOptions>()
        .enabled = false;
    reset_runtime_state(app.world_mut());
    app.world_mut().resource_mut::<RecordedEvents>().custom.clear();

    // Next Play: on_start fires again cleanly and local scope counters restart from 0!
    app.world_mut()
        .resource_mut::<ScriptExecutionOptions>()
        .enabled = true;
    step_update(&mut app, 0.25);

    let c1_after = app.world().get::<ScriptComponent>(e1).unwrap();
    assert_eq!(c1_after.properties.get_int("starts"), Some(1));
    assert_eq!(c1_after.properties.get_int("updates"), Some(1));
    assert_eq!(
        app.world().resource::<RecordedEvents>().custom.len(),
        2,
        "both entities should fire on_start again after PIE reset"
    );
}

#[test]
fn physics_collision_and_trigger_enter_exit_hooks_fire_from_rapier_observers() {
    let mut app = build_test_app();

    let ball_hooks_uuid = app
        .world_mut()
        .resource_mut::<ScriptRuntime>()
        .register_inline_script(
            "ball_hooks.rhai",
            r#"
            fn on_collision(other) {
                events.emit("collided", #{ other_name: other.name, dim: other.dimension, phase: other.phase });
            }
            fn on_trigger_enter(other) {
                events.emit("trigger_enter", #{ other_name: other.name, dim: other.dimension });
            }
            fn on_trigger_exit(other) {
                events.emit("trigger_exit", #{ other_name: other.name, dim: other.dimension });
            }
            "#,
        )
        .unwrap();

    // Floor at y = -0.5, sensor trigger at y = 2.0, dynamic ball dropped from y = 4.0.
    app.world_mut().spawn((
        Name::new("Floor3D"),
        Transform::from_xyz(0.0, -0.5, 0.0),
        Collider3D::Cuboid(Vec3::new(10.0, 0.5, 10.0)),
        PhysicsMaterial {
            restitution: 0.2,
            ..Default::default()
        },
    ));
    app.world_mut().spawn((
        Name::new("SensorGate"),
        Transform::from_xyz(0.0, 2.0, 0.0),
        Collider3D::Cuboid(Vec3::new(2.0, 0.3, 2.0)),
        TriggerVolume3D,
    ));
    app.world_mut().spawn((
        Name::new("FallingBall"),
        EntityGuid::new(),
        Transform::from_xyz(0.0, 4.0, 0.0),
        Collider3D::Ball(0.4),
        RigidBody3D {
            ccd: true,
            ..Default::default()
        },
        ScriptComponent::from_uuid(ball_hooks_uuid),
    ));

    step_fixed(&mut app, 90);

    let events = &app.world().resource::<RecordedEvents>().custom;
    assert!(
        events
            .iter()
            .any(|e| e.name == "trigger_enter" && e.payload["other_name"] == "SensorGate"),
        "expected on_trigger_enter for SensorGate, got: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e.name == "trigger_exit" && e.payload["other_name"] == "SensorGate"),
        "expected on_trigger_exit for SensorGate, got: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e.name == "collided" && e.payload["other_name"] == "Floor3D"),
        "expected on_collision for Floor3D, got: {events:?}"
    );
}

// =============================================================================
// 4. ECS Reflect Bridge (Transform + custom reflected components + error safety)
// =============================================================================

#[test]
fn reflect_bridge_reads_and_mutates_transform_and_custom_reflected_components() {
    let mut app = build_test_app();

    let reflect_uuid = app
        .world_mut()
        .resource_mut::<ScriptRuntime>()
        .register_inline_script(
            "reflect_test.rhai",
            r#"
            fn on_start() {
                // Transform API
                transform.translate(2.0, 3.0, -1.0);
                transform.rotate_y(1.5707963);
                transform.scale = vec3(2.0, 2.0, 2.0);

                // Read custom reflected component via short name
                let stats = entity.get_component("PlayerStats");
                let hp = entity.get_component_field("PlayerStats", "health");
                entity.set_component_field("PlayerStats", "health", hp - 25);
                entity.set_field("PlayerStats", "stamina", stats.stamina + 15);
                entity.set_field("PlayerStats", "invulnerable", true);
                entity.set_field("PlayerStats", "title", "Champion");
                entity.set_field("PlayerStats", "offset.y", 9.5);

                // Option::None must read as unit `()` (not `"None"`), and Option::Some unwraps cleanly.
                let nick = entity.get_field("PlayerStats", "nickname");
                let bonus = entity.get_field("PlayerStats", "bonus_tag");

                // Same-hook staged read should immediately observe updated field!
                let updated_hp = entity.get_field("PlayerStats", "health");
                events.emit("reflect_checked", #{
                    hp: updated_hp,
                    pos_x: transform.translation.x,
                    nick_is_unit: nick == (),
                    stats_nick_is_unit: stats.nickname == (),
                    bonus: bonus,
                });
            }
            "#,
        )
        .unwrap();

    let hero = app
        .world_mut()
        .spawn((
            Name::new("Hero"),
            Transform::from_xyz(1.0, 0.0, 0.0),
            PlayerStats::default(),
            ScriptComponent::from_uuid(reflect_uuid),
        ))
        .id();

    step_update(&mut app, 0.016);

    assert!(
        app.world().resource::<ScriptDiagnostics>().is_empty(),
        "unexpected diagnostics: {:?}",
        app.world().resource::<ScriptDiagnostics>().entries()
    );

    let tfm = app.world().get::<Transform>(hero).unwrap();
    assert!((tfm.translation - Vec3::new(3.0, 3.0, -1.0)).length() < 1e-4);
    assert!((tfm.scale - Vec3::splat(2.0)).length() < 1e-4);

    let stats = app.world().get::<PlayerStats>(hero).unwrap();
    assert!((stats.health - 75.0).abs() < 1e-4);
    assert_eq!(stats.stamina, 65);
    assert!(stats.invulnerable);
    assert_eq!(stats.title, "Champion");
    assert!((stats.offset.y - 9.5).abs() < 1e-4);

    let ev = &app.world().resource::<RecordedEvents>().custom[0];
    assert_eq!(ev.name, "reflect_checked");
    assert_eq!(ev.payload["hp"], 75.0);
    assert_eq!(ev.payload["pos_x"], 3.0);
    assert_eq!(ev.payload["nick_is_unit"], true);
    assert_eq!(ev.payload["stats_nick_is_unit"], true);
    assert_eq!(ev.payload["bonus"], "Ace");
}

#[test]
fn reflect_bridge_rejects_immutable_component_and_bad_field_without_panicking() {
    let mut app = build_test_app();

    let bad_reflect_uuid = app
        .world_mut()
        .resource_mut::<ScriptRuntime>()
        .register_inline_script(
            "bad_reflect.rhai",
            r#"
            fn on_update(dt) {
                entity.set_field("ChildOf", "0", 123);
            }
            "#,
        )
        .unwrap();

    let parent = app.world_mut().spawn(Name::new("Parent")).id();
    let child = app
        .world_mut()
        .spawn((
            Name::new("Child"),
            ChildOf(parent),
            ScriptComponent::from_uuid(bad_reflect_uuid),
        ))
        .id();

    step_update(&mut app, 0.016);

    let diags = app.world().resource::<ScriptDiagnostics>().for_entity(child);
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].message.contains("immutable"),
        "expected immutable error diagnostic, got: {}",
        diags[0].message
    );
}

// =============================================================================
// 5. Gameplay APIs: Input, Physics (3D & 2D), Audio, World, Events
// =============================================================================

#[test]
fn input_api_reads_enhanced_input_actions_and_axes() {
    let mut app = build_test_app();
    app.add_plugins((
        bevy_app::TaskPoolPlugin::default(),
        bevy_asset::AssetPlugin::default(),
        EngineInputPlugin,
    ));

    let mapping = InputMappingAsset {
        actions: vec![
            InputAction {
                id: "jump".to_owned(),
                axis: ActionAxis::Bool,
            },
            InputAction {
                id: "move".to_owned(),
                axis: ActionAxis::Axis2D,
            },
            InputAction {
                id: "throttle".to_owned(),
                axis: ActionAxis::Axis1D,
            },
        ],
        contexts: vec![InputMappingContext {
            id: "gameplay".to_owned(),
            priority: 0,
            domain: InputDomain::Game,
            bindings: vec![
                InputBinding {
                    id: "jump.space".to_owned(),
                    action: "jump".to_owned(),
                    source: InputSource::Key(KeyCode::Space),
                    channel: AxisChannel::X,
                    trigger: Default::default(),
                    modifiers: vec![],
                },
                InputBinding {
                    id: "move.right".to_owned(),
                    action: "move".to_owned(),
                    source: InputSource::Key(KeyCode::KeyD),
                    channel: AxisChannel::X,
                    trigger: Default::default(),
                    modifiers: vec![],
                },
                InputBinding {
                    id: "move.up".to_owned(),
                    action: "move".to_owned(),
                    source: InputSource::Key(KeyCode::KeyW),
                    channel: AxisChannel::Y,
                    trigger: Default::default(),
                    modifiers: vec![],
                },
                InputBinding {
                    id: "throttle.up".to_owned(),
                    action: "throttle".to_owned(),
                    source: InputSource::Key(KeyCode::KeyE),
                    channel: AxisChannel::X,
                    trigger: Default::default(),
                    modifiers: vec![InputModifier::Scale(0.5)],
                },
            ],
        }],
    };
    mapping.validate().unwrap();

    let mut assets = Assets::<InputMappingAsset>::default();
    let handle = assets.add(mapping);
    app.insert_resource(assets)
        .insert_resource(ProjectInputMapping(handle))
        .init_resource::<ForwardedInput>()
        .insert_resource(InputContexts {
            domain: InputDomain::Game,
            ..Default::default()
        });

    let input_reader_uuid = app
        .world_mut()
        .resource_mut::<ScriptRuntime>()
        .register_inline_script(
            "input_reader.rhai",
            r#"
            fn on_update(dt) {
                let mv = input.axis_2d("move");
                events.emit("input_frame", #{
                    jump_pressed: input.action_pressed("jump"),
                    jump_just_pressed: input.action_just_pressed("jump"),
                    jump_just_released: input.action_just_released("jump"),
                    move_x: mv.x,
                    move_y: mv.y,
                    throttle: input.axis_1d("throttle"),
                    axis3_y: input.axis_3d("move").y,
                });
            }
            "#,
        )
        .unwrap();

    app.world_mut()
        .spawn((Name::new("InputProbe"), ScriptComponent::from_uuid(input_reader_uuid)));

    // Frame 1: Press Space, D, W, E
    {
        let mut forwarded = app.world_mut().resource_mut::<ForwardedInput>();
        forwarded.keys.press(KeyCode::Space);
        forwarded.keys.press(KeyCode::KeyD);
        forwarded.keys.press(KeyCode::KeyW);
        forwarded.keys.press(KeyCode::KeyE);
    }
    app.world_mut().run_schedule(PreUpdate);
    step_update(&mut app, 0.016);

    // Frame 2: Release Space
    {
        let mut forwarded = app.world_mut().resource_mut::<ForwardedInput>();
        forwarded.keys.release(KeyCode::Space);
    }
    app.world_mut().run_schedule(PreUpdate);
    step_update(&mut app, 0.016);

    let events = &app.world().resource::<RecordedEvents>().custom;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].payload["jump_pressed"], true);
    assert_eq!(events[0].payload["jump_just_pressed"], true);
    assert_eq!(events[0].payload["jump_just_released"], false);
    assert_eq!(events[0].payload["move_x"], 1.0);
    assert_eq!(events[0].payload["move_y"], 1.0);
    assert_eq!(events[0].payload["throttle"], 0.5);
    assert_eq!(events[0].payload["axis3_y"], 1.0);

    assert_eq!(events[1].payload["jump_pressed"], false);
    assert_eq!(events[1].payload["jump_just_pressed"], false);
    assert_eq!(events[1].payload["jump_just_released"], true);
}

#[test]
fn physics_api_raycast_impulse_and_character_controller_3d_and_2d() {
    let mut app = build_test_app();
    app.insert_resource(Gravity {
        xy: Vec2::ZERO,
        xyz: Vec3::ZERO,
    });

    let (phys_3d_uuid, char_3d_uuid, phys_2d_uuid, char_2d_uuid) = {
        let mut rt = app.world_mut().resource_mut::<ScriptRuntime>();
        let p3 = rt
            .register_inline_script(
                "physics_3d.rhai",
                r#"
            let pulsed = false;
            fn on_fixed_update(dt) {
                let hit = physics.raycast_3d(0.0, 5.0, 0.0, 0.0, -1.0, 0.0, 20.0);
                if hit != () {
                    props.ray3d_hit = hit.name;
                    props.ray3d_dist = hit.distance;
                }
                if !pulsed {
                    physics.apply_impulse_3d(0.0, 12.0, 0.0);
                    pulsed = true;
                }
            }
            "#,
            )
            .unwrap();

        let c3 = rt
            .register_inline_script(
                "char_3d.rhai",
                r#"
            fn on_fixed_update(dt) {
                physics.move_character_3d(6.0, 0.0, 0.0);
            }
            "#,
            )
            .unwrap();

        let p2 = rt
            .register_inline_script(
                "physics_2d.rhai",
                r#"
            let pulsed = false;
            fn on_fixed_update(dt) {
                let hit = physics.raycast_2d(0.0, 100.0, 0.0, -1.0, 200.0);
                if hit != () {
                    props.ray2d_hit = hit.name;
                }
                if !pulsed {
                    physics.apply_impulse_2d(50.0, 0.0);
                    pulsed = true;
                }
            }
            "#,
            )
            .unwrap();

        let c2 = rt
            .register_inline_script(
                "char_2d.rhai",
                r#"
            fn on_fixed_update(dt) {
                physics.move_character_2d(60.0, 0.0);
            }
            "#,
            )
            .unwrap();
        (p3, c3, p2, c2)
    };

    // 3D static floor + dynamic body + character controller
    app.world_mut().spawn((
        Name::new("Ground3D"),
        Transform::from_xyz(0.0, -0.5, 0.0),
        Collider3D::Cuboid(Vec3::new(10.0, 0.5, 10.0)),
    ));
    let body3d = app
        .world_mut()
        .spawn((
            Name::new("Body3D"),
            Transform::from_xyz(0.0, 5.0, 0.0),
            Collider3D::Ball(0.5),
            RigidBody3D::default(),
            ScriptComponent::from_uuid(phys_3d_uuid)
                .with_property("ray3d_hit", "")
                .with_property("ray3d_dist", 0.0_f64),
        ))
        .id();
    let char3d = app
        .world_mut()
        .spawn((
            Name::new("Char3D"),
            Transform::from_xyz(5.0, 2.0, 0.0),
            Collider3D::Capsule(0.5, 0.3),
            RigidBody3D {
                kind: BodyKind::Kinematic,
                ..Default::default()
            },
            CharacterController3D::default(),
            ScriptComponent::from_uuid(char_3d_uuid),
        ))
        .id();

    // 2D static floor + dynamic body + character controller (top-down mode)
    app.world_mut().spawn((
        Name::new("Ground2D"),
        Transform::from_xyz(0.0, -10.0, 0.0),
        Collider2D::Cuboid(Vec2::new(200.0, 10.0)),
    ));
    let body2d = app
        .world_mut()
        .spawn((
            Name::new("Body2D"),
            Transform::from_xyz(0.0, 100.0, 0.0),
            Collider2D::Ball(10.0),
            RigidBody2D::default(),
            ScriptComponent::from_uuid(phys_2d_uuid).with_property("ray2d_hit", ""),
        ))
        .id();
    let char2d = app
        .world_mut()
        .spawn((
            Name::new("Char2D"),
            Transform::from_xyz(50.0, 50.0, 0.0),
            Collider2D::Cuboid(Vec2::new(8.0, 8.0)),
            RigidBody2D {
                kind: BodyKind::Kinematic,
                ..Default::default()
            },
            CharacterController2D {
                platformer: false,
                ..Default::default()
            },
            ScriptComponent::from_uuid(char_2d_uuid),
        ))
        .id();

    // First step synchronizes colliders into Rapier; subsequent steps run raycast/impulse/motion.
    step_fixed(&mut app, 10);

    let b3_props = &app.world().get::<ScriptComponent>(body3d).unwrap().properties;
    assert_eq!(b3_props.get_string("ray3d_hit"), Some("Ground3D"));
    assert!((b3_props.get_float("ray3d_dist").unwrap() - 5.0).abs() < 0.05);
    assert!(
        app.world()
            .resource::<PhysicsWorld3D>()
            .linear_velocity(body3d)
            .unwrap()
            .y
            > 1.0,
        "apply_impulse_3d must accelerate RigidBody3D upward"
    );
    assert!(
        app.world().get::<Transform>(char3d).unwrap().translation.x > 5.5,
        "move_character_3d must move CharacterController3D along +X"
    );

    let b2_props = &app.world().get::<ScriptComponent>(body2d).unwrap().properties;
    assert_eq!(b2_props.get_string("ray2d_hit"), Some("Ground2D"));
    assert!(
        app.world()
            .resource::<PhysicsWorld2D>()
            .linear_velocity(body2d)
            .unwrap()
            .x
            > 0.01,
        "apply_impulse_2d must accelerate RigidBody2D along +X"
    );
    assert!(
        app.world().get::<Transform>(char2d).unwrap().translation.x > 55.0,
        "move_character_2d must move CharacterController2D along +X"
    );
}

#[derive(Default)]
struct MockAudioBackend {
    played: Arc<Mutex<Vec<PlaySound>>>,
    stopped: Arc<Mutex<Vec<VoiceId>>>,
    next_voice: u64,
}

impl AudioBackend for MockAudioBackend {
    fn play(&mut self, sound: &PlaySound) -> Result<VoiceId, AudioError> {
        self.next_voice += 1;
        self.played.lock().unwrap().push(sound.clone());
        Ok(VoiceId(self.next_voice))
    }
    fn stop(&mut self, voice: VoiceId) {
        self.stopped.lock().unwrap().push(voice);
    }
    fn set_bus(&mut self, _bus: AudioBus, _settings: BusSettings) {}
    fn set_listener(&mut self, _position: Vec3, _orientation: Quat) {}
    fn set_position(&mut self, _voice: VoiceId, _position: Vec3) {}
    fn collect_finished(&mut self) {}
}

#[test]
fn audio_world_and_events_apis_work_end_to_end() {
    let dir = TempTestDir::new("audio-world");
    let assets_dir = dir.path.join("assets");
    let sounds_dir = assets_dir.join("sounds");
    let prefabs_dir = assets_dir.join("prefabs");
    fs::create_dir_all(&sounds_dir).unwrap();
    fs::create_dir_all(&prefabs_dir).unwrap();

    // Dummy audio file indexed in AssetRegistry
    let wav_path = sounds_dir.join("coin.wav");
    fs::write(&wav_path, b"RIFF0000WAVE").unwrap();

    let mut registry = AssetRegistry::new(assets_dir.clone(), None);
    registry.scan_all().unwrap();
    let wav_uuid = registry
        .get_by_path("sounds/coin.wav")
        .expect("coin.wav indexed")
        .uuid;

    let played = Arc::new(Mutex::new(Vec::<PlaySound>::new()));
    let stopped = Arc::new(Mutex::new(Vec::<VoiceId>::new()));
    let mock_audio = MockAudioBackend {
        played: played.clone(),
        stopped: stopped.clone(),
        next_voice: 0,
    };

    let mut app = build_test_app();
    let manifest = ProjectManifest::new("ScriptingTest", WorldMode::Mode3D);
    let project_paths = manifest.resolve_paths(&dir.path);
    app.insert_resource(project_paths);
    app.insert_resource(registry);
    app.insert_resource(AudioEngine::with_backend(mock_audio));

    // Author a real `.prefab.ron` file on disk
    let prefab_template = app
        .world_mut()
        .spawn((
            SceneEntity,
            EntityGuid::new(),
            Name::new("SpawnedCrate"),
            Transform::from_xyz(0.0, 0.0, 0.0),
        ))
        .id();
    let prefab_abs = prefabs_dir.join("crate.prefab.ron");
    save_prefab_to_file(
        app.world_mut(),
        prefab_template,
        &prefab_abs,
        "prefabs/crate.prefab.ron",
        None,
    )
    .expect("save prefab");
    app.world_mut().despawn(prefab_template);

    let doomed_target = app
        .world_mut()
        .spawn((Name::new("DoomedTarget"), Transform::default()))
        .id();

    let director_uuid = app
        .world_mut()
        .resource_mut::<ScriptRuntime>()
        .register_inline_script(
            "director.rhai",
            r#"
            fn on_start() {
                audio.play("sounds/coin.wav");
                audio.play_emitter();
                world.spawn_prefab("prefabs/crate.prefab.ron", 4.0, 5.0, 6.0);
                let target = world.find_by_name("DoomedTarget");
                if target != () {
                    world.despawn(target);
                }
                events.emit("director_done", #{ found_target: target != () });
                log("director finished");
                world.despawn_self();
            }
            "#,
        )
        .unwrap();

    let director = app
        .world_mut()
        .spawn((
            Name::new("Director"),
            EntityGuid::new(),
            Transform::from_xyz(1.0, 2.0, 3.0),
            AudioEmitter {
                clip: Some(wav_uuid),
                bus: AudioBus::Music,
                space: AudioSpace::Scene3D,
                gain: 0.75,
                pitch: 1.2,
                ..Default::default()
            },
            ScriptComponent::from_uuid(director_uuid),
        ))
        .id();

    step_update(&mut app, 0.016);

    assert!(
        app.world().resource::<ScriptDiagnostics>().is_empty(),
        "unexpected diagnostics: {:?}",
        app.world().resource::<ScriptDiagnostics>().entries()
    );

    // Both audio.play and audio.play_emitter invoked the AudioBackend!
    let sounds = played.lock().unwrap();
    assert_eq!(sounds.len(), 2);
    assert_eq!(sounds[0].bus, AudioBus::Sfx);
    assert_eq!(sounds[0].path, wav_path);
    assert_eq!(sounds[1].bus, AudioBus::Music);
    assert!((sounds[1].gain - 0.75).abs() < 1e-4);
    assert!(sounds[1].spatial.is_some());

    // Target and director were despawned cleanly.
    assert!(app.world().get_entity(doomed_target).is_err());
    assert!(app.world().get_entity(director).is_err());

    // Prefab was instantiated at (4, 5, 6)!
    let mut crate_pos = None;
    for archetype in app.world().archetypes().iter() {
        for ae in archetype.entities() {
            let e = ae.id();
            if app
                .world()
                .get::<Name>(e)
                .is_some_and(|n| n.as_str() == "SpawnedCrate")
            {
                crate_pos = app.world().get::<Transform>(e).map(|t| t.translation);
            }
        }
    }
    assert_eq!(crate_pos, Some(Vec3::new(4.0, 5.0, 6.0)));

    // Custom event emitted cleanly.
    let custom_events = &app.world().resource::<RecordedEvents>().custom;
    assert_eq!(custom_events.len(), 1);
    assert_eq!(custom_events[0].name, "director_done");
    assert_eq!(custom_events[0].payload["found_target"], true);

    // PIE Stop reset must stop all tracked script-triggered audio voices cleanly!
    assert_eq!(app.world().resource::<ActiveScriptVoices>().len(), 2);
    assert!(stopped.lock().unwrap().is_empty());
    reset_runtime_state(app.world_mut());
    assert!(app.world().resource::<ActiveScriptVoices>().is_empty());
    assert_eq!(*stopped.lock().unwrap(), vec![VoiceId(1), VoiceId(2)]);
}

#[test]
fn deferred_reset_clears_impulses_contacts_and_voices_and_paused_never_flushes_impulses() {
    let played = Arc::new(Mutex::new(Vec::<PlaySound>::new()));
    let stopped = Arc::new(Mutex::new(Vec::<VoiceId>::new()));
    let mock_audio = MockAudioBackend {
        played: played.clone(),
        stopped: stopped.clone(),
        next_voice: 0,
    };

    let mut app = build_test_app();
    app.insert_resource(Gravity {
        xy: Vec2::ZERO,
        xyz: Vec3::ZERO,
    });
    app.insert_resource(AudioEngine::with_backend(mock_audio));
    app.insert_resource(ScriptExecutionOptions::editor()); // paused/disabled

    let body = app
        .world_mut()
        .spawn((
            Name::new("PausedBody"),
            Transform::from_xyz(0.0, 2.0, 0.0),
            Collider3D::Ball(0.5),
            RigidBody3D::default(),
        ))
        .id();

    // Sync body into Rapier while scripts are disabled.
    step_fixed(&mut app, 1);

    // Queue a pending impulse and an active voice, then step FixedUpdate while disabled.
    app.world_mut()
        .resource_mut::<PendingScriptImpulses>()
        .impulses_3d
        .push((body, Vec3::new(0.0, 25.0, 0.0)));
    app.world_mut()
        .resource_mut::<ActiveScriptVoices>()
        .push(VoiceId(42));

    step_fixed(&mut app, 2);
    assert!(
        app.world()
            .resource::<PhysicsWorld3D>()
            .linear_velocity(body)
            .unwrap()
            .length()
            < 1e-4,
        "paused/disabled scripting must NOT flush PendingScriptImpulses"
    );
    assert_eq!(
        app.world().resource::<PendingScriptImpulses>().impulses_3d.len(),
        1
    );

    // Request deferred reset (for example on PIE Stop) and tick FixedUpdate:
    // impulses must be cleared (NOT applied!) and active voices must be stopped!
    app.world_mut()
        .resource_mut::<ScriptExecutionOptions>()
        .request_reset();
    step_fixed(&mut app, 1);

    assert!(
        !app.world()
            .resource::<ScriptExecutionOptions>()
            .reset_requested
    );
    assert!(
        app.world()
            .resource::<PendingScriptImpulses>()
            .impulses_3d
            .is_empty()
    );
    assert!(app.world().resource::<ActiveScriptVoices>().is_empty());
    assert_eq!(*stopped.lock().unwrap(), vec![VoiceId(42)]);
    assert!(
        app.world()
            .resource::<PhysicsWorld3D>()
            .linear_velocity(body)
            .unwrap()
            .length()
            < 1e-4,
        "deferred reset must clear stale impulses without applying them"
    );
}

// =============================================================================
// 6. Hot-reload on disk + ScriptProperties preservation + syntax error recovery
// =============================================================================

#[test]
fn hot_reload_recompiles_preserves_properties_and_recovers_from_syntax_errors() {
    let dir = TempTestDir::new("hot-reload");
    let assets_dir = dir.path.join("assets");
    fs::create_dir_all(&assets_dir).unwrap();
    let script_file = assets_dir.join("mover.rhai");

    // Version 1: move along +X using props.speed
    fs::write(
        &script_file,
        r#"
        fn on_start() {
            props.start_count += 1;
        }
        fn on_update(dt) {
            transform.translate(props.speed * dt, 0.0, 0.0);
        }
        "#,
    )
    .unwrap();

    let mut registry = AssetRegistry::new(assets_dir, None);
    registry.scan_all().unwrap();
    let mover_uuid = registry
        .get_by_path("mover.rhai")
        .expect("mover.rhai indexed")
        .uuid;

    let mut app = build_test_app();
    let manifest = ProjectManifest::new("HotReloadTest", WorldMode::Mode3D);
    app.insert_resource(manifest.resolve_paths(&dir.path));
    app.insert_resource(registry);

    let entity_guid = EntityGuid::new();
    let entity = app
        .world_mut()
        .spawn((
            Name::new("HotReloadMover"),
            entity_guid,
            Transform::default(),
            ScriptComponent::from_uuid(mover_uuid)
                .with_property("speed", 4.0_f64)
                .with_property("label", "preserved")
                .with_property("start_count", 0_i64),
        ))
        .id();

    // Run v1
    step_update(&mut app, 0.5);
    let tfm1 = app.world().get::<Transform>(entity).unwrap().translation;
    assert!((tfm1 - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-4);
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(entity)
            .unwrap()
            .properties
            .get_int("start_count"),
        Some(1)
    );

    // Edit file on disk -> Version 2: move along +Y using props.speed
    fs::write(
        &script_file,
        r#"
        fn on_start() {
            props.start_count += 1;
        }
        fn on_update(dt) {
            transform.translate(0.0, props.speed * dt, 0.0);
        }
        "#,
    )
    .unwrap();

    step_update(&mut app, 0.5);
    let tfm2 = app.world().get::<Transform>(entity).unwrap().translation;
    assert!(
        (tfm2 - Vec3::new(2.0, 2.0, 0.0)).length() < 1e-4,
        "expected hot-reloaded +Y motion, got {tfm2:?}"
    );
    let props_after_v2 = &app.world().get::<ScriptComponent>(entity).unwrap().properties;
    assert_eq!(props_after_v2.get_float("speed"), Some(4.0));
    assert_eq!(props_after_v2.get_string("label"), Some("preserved"));
    assert_eq!(
        props_after_v2.get_int("start_count"),
        Some(1),
        "hot-reload must NOT re-run on_start on already started entities"
    );
    assert_eq!(app.world().resource::<RecordedEvents>().reloads.len(), 1);

    // Introduce syntax error on line 3 -> must NOT crash, emits file+line diagnostic, preserves properties!
    fs::write(
        &script_file,
        "fn on_update(dt) {\n    let broken = ;\n}\n",
    )
    .unwrap();

    step_update(&mut app, 0.5);
    step_update(&mut app, 0.5); // Second frame must NOT spam duplicate syntax error!

    let diags = app.world().resource::<ScriptDiagnostics>().entries();
    assert_eq!(
        diags.len(),
        1,
        "syntax error should be reported once without per-frame spam, got: {diags:?}"
    );
    assert_eq!(diags[0].stage, ScriptDiagnosticStage::Reload);
    assert_eq!(diags[0].file, "mover.rhai");
    assert_eq!(diags[0].line, Some(2));
    assert_eq!(diags[0].entity_name.as_deref(), Some("HotReloadMover"));
    assert_eq!(diags[0].entity_guid, Some(entity_guid));

    // Transform unchanged while broken; ScriptProperties untouched.
    let tfm_broken = app.world().get::<Transform>(entity).unwrap().translation;
    assert!((tfm_broken - tfm2).length() < 1e-4);
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(entity)
            .unwrap()
            .properties
            .get_string("label"),
        Some("preserved")
    );

    // Fix syntax error -> Version 3: move along +Z using props.speed!
    fs::write(
        &script_file,
        r#"
        fn on_update(dt) {
            transform.translate(0.0, 0.0, props.speed * dt);
        }
        "#,
    )
    .unwrap();

    step_update(&mut app, 0.5);

    let tfm3 = app.world().get::<Transform>(entity).unwrap().translation;
    assert!(
        (tfm3 - Vec3::new(2.0, 2.0, 2.0)).length() < 1e-4,
        "expected recovered +Z motion after fixing syntax error, got {tfm3:?}"
    );
    let props_after_v3 = &app.world().get::<ScriptComponent>(entity).unwrap().properties;
    assert_eq!(props_after_v3.get_float("speed"), Some(4.0));
    assert_eq!(props_after_v3.get_string("label"), Some("preserved"));
    assert_eq!(app.world().resource::<RecordedEvents>().reloads.len(), 2);

    // Also verify reverting a broken edit back to exact previous valid source recovers!
    fs::write(&script_file, "fn on_update(dt) { let oops = ; }").unwrap();
    assert_eq!(reload_modified_scripts(app.world_mut()), 0);
    fs::write(
        &script_file,
        r#"
        fn on_update(dt) {
            transform.translate(0.0, 0.0, props.speed * dt);
        }
        "#,
    )
    .unwrap();
    assert_eq!(reload_modified_scripts(app.world_mut()), 1);
    step_update(&mut app, 0.5);
    let tfm4 = app.world().get::<Transform>(entity).unwrap().translation;
    assert!((tfm4 - Vec3::new(2.0, 2.0, 4.0)).length() < 1e-4);
}

#[test]
fn asset_uuid_script_resolution_and_hot_reload() {
    let dir = TempTestDir::new("uuid-script");
    let assets_dir = dir.path.join("assets");
    fs::create_dir_all(&assets_dir).unwrap();
    let script_path = assets_dir.join("spinner.rhai");
    fs::write(&script_path, "fn on_update(dt) { props.ticks += 1; }").unwrap();

    let mut registry = AssetRegistry::new(assets_dir, None);
    registry.scan_all().unwrap();
    let script_uuid = registry
        .get_by_path("spinner.rhai")
        .expect("spinner.rhai indexed")
        .uuid;

    let mut app = build_test_app();
    app.insert_resource(registry);

    let entity = app
        .world_mut()
        .spawn((
            Name::new("UuidScripted"),
            ScriptComponent::from_uuid(script_uuid).with_property("ticks", 0_i64),
        ))
        .id();

    step_update(&mut app, 0.016);
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(entity)
            .unwrap()
            .properties
            .get_int("ticks"),
        Some(1)
    );

    // Hot-reload UUID-referenced script to increment by 10 instead of 1
    fs::write(&script_path, "fn on_update(dt) { props.ticks += 10; }").unwrap();
    step_update(&mut app, 0.016);
    assert_eq!(
        app.world()
            .get::<ScriptComponent>(entity)
            .unwrap()
            .properties
            .get_int("ticks"),
        Some(11)
    );
}

// =============================================================================
// 7. Pluggable ScriptBackend trait smoke test
// =============================================================================

struct CountingMockBackend {
    sandbox: ScriptSandboxConfig,
    compiled: Option<ScriptCompiledInfo>,
    started: bool,
    calls: Arc<Mutex<Vec<ScriptHook>>>,
}

impl ScriptBackend for CountingMockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }
    fn sandbox_config(&self) -> &ScriptSandboxConfig {
        &self.sandbox
    }
    fn set_sandbox_config(&mut self, config: ScriptSandboxConfig) {
        self.sandbox = config;
    }
    fn compile_source(
        &mut self,
        key: &ScriptSourceKey,
        source_label: &str,
        source_code: &str,
        mtime: Option<SystemTime>,
    ) -> Result<ScriptCompiledInfo, ScriptError> {
        let info = ScriptCompiledInfo {
            key: key.clone(),
            source_label: source_label.to_owned(),
            content_hash: blake3::hash(source_code.as_bytes()).to_hex().to_string(),
            mtime,
            revision: 1,
            hooks: ScriptHookSet {
                on_start: true,
                on_update: true,
                ..Default::default()
            },
        };
        self.compiled = Some(info.clone());
        Ok(info)
    }
    fn compiled_info(&self, key: &ScriptSourceKey) -> Option<ScriptCompiledInfo> {
        self.compiled
            .as_ref()
            .filter(|info| info.key == *key)
            .cloned()
    }
    fn invalidate(&mut self, _key: &ScriptSourceKey) {
        self.compiled = None;
    }
    fn clear_cache(&mut self) {
        self.compiled = None;
    }
    fn has_entity_instance(&self, _entity: Entity) -> bool {
        self.started
    }
    fn entity_started(&self, _entity: Entity) -> bool {
        self.started
    }
    fn remove_entity_instance(&mut self, _entity: Entity) {
        self.started = false;
    }
    fn reset_all_instances(&mut self) {
        self.started = false;
    }
    fn sync_entity_properties(&mut self, _entity: Entity, _properties: &ScriptProperties) {}
    fn invoke_hook(
        &mut self,
        _entity: Entity,
        _key: &ScriptSourceKey,
        _properties: &mut ScriptProperties,
        hook: ScriptHook,
        _args: ScriptHookArgs,
        _call_context: &ScriptCallContext,
    ) -> Result<bool, ScriptError> {
        if hook == ScriptHook::OnStart {
            self.started = true;
        }
        self.calls.lock().unwrap().push(hook);
        Ok(true)
    }
}

#[test]
fn pluggable_custom_script_backend_runs_through_script_runtime() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let backend = CountingMockBackend {
        sandbox: ScriptSandboxConfig::default(),
        compiled: None,
        started: false,
        calls: calls.clone(),
    };

    let mut app = App::new();
    app.insert_resource(ScriptRuntime::with_backend(
        backend,
        ScriptCallContext::new(),
    ));
    app.add_plugins((TimePlugin, EngineCorePlugin, EngineScriptingPlugin));
    let mock_uuid = app
        .world_mut()
        .resource_mut::<ScriptRuntime>()
        .register_inline_script("mock.script", "// noop")
        .unwrap();

    app.world_mut()
        .spawn(ScriptComponent::from_uuid(mock_uuid));

    step_update(&mut app, 0.016);
    step_update(&mut app, 0.016);

    assert_eq!(
        *calls.lock().unwrap(),
        vec![ScriptHook::OnStart, ScriptHook::OnUpdate, ScriptHook::OnUpdate]
    );
}
