//! Declarative input mappings, user overrides and frame/fixed-step action states.

use std::collections::{BTreeMap, BTreeSet};

use bevy_app::{App, FixedUpdate, Plugin, PreUpdate};
use bevy_asset::{io::Reader, Asset, AssetApp, AssetLoader, Assets, Handle, LoadContext};
use bevy_ecs::{prelude::*, reflect::ReflectResource};
use bevy_input::{
    gamepad::{Gamepad, GamepadAxis, GamepadButton},
    keyboard::KeyCode,
    mouse::MouseButton,
    ButtonInput, InputSystems,
};
use bevy_math::{Vec2, Vec3};
use bevy_reflect::{prelude::ReflectDefault, Reflect, TypePath};
use bevy_settings::{ReflectSettingsGroup, SettingsGroup};
use bevy_time::{Real, Time};
use engine_core::EngineFixedSet;
use serde::{Deserialize, Serialize};

/// Shape of the value returned by an action.
#[derive(Debug, Clone, Copy, Default, Reflect, Serialize, Deserialize)]
pub enum ActionAxis {
    /// Digital action (any nonzero contribution counts as active).
    #[default]
    Bool,
    /// Scalar on X.
    Axis1D,
    /// Vector on XY.
    Axis2D,
    /// Vector on XYZ.
    Axis3D,
}

/// Named action shared by one or more bindings.
#[derive(Debug, Clone, Reflect, Serialize, Deserialize)]
pub struct InputAction {
    /// Stable action name used by gameplay code.
    pub id: String,
    /// Output shape.
    pub axis: ActionAxis,
}

/// Raw input source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Reflect, Serialize, Deserialize)]
pub enum InputSource {
    /// Physical keyboard key.
    Key(KeyCode),
    /// Mouse button.
    Mouse(MouseButton),
    /// Gamepad button on any connected gamepad.
    GamepadButton(GamepadButton),
    /// Signed axis, sampled from the gamepad with greatest deflection.
    GamepadAxis(GamepadAxis),
}

/// Activation rule for a binding.
#[derive(Debug, Clone, Default, Reflect, Serialize, Deserialize)]
pub enum InputTrigger {
    /// Active while the source actuates; edges are reported separately.
    #[default]
    Pressed,
    /// Pulse on release.
    Released,
    /// Activate after a minimum duration in seconds.
    Hold(f32),
    /// Pulse on release within the specified seconds.
    Tap(f32),
    /// Require another source to be held.
    Chord(InputSource),
}

/// Scalar value transformations applied in order.
#[derive(Debug, Clone, Reflect, Serialize, Deserialize)]
pub enum InputModifier {
    /// Suppress input at or below this magnitude.
    Deadzone(f32),
    /// Multiply by a scalar.
    Scale(f32),
    /// Negate.
    Invert,
}

/// Destination for a scalar contribution.
#[derive(Debug, Clone, Copy, Default, Reflect, Serialize, Deserialize)]
pub enum AxisChannel {
    /// First dimension.
    #[default]
    X,
    /// Second dimension.
    Y,
    /// Third dimension.
    Z,
}

/// One binding in a context; IDs are stable across edits and reordering.
#[derive(Debug, Clone, Reflect, Serialize, Deserialize)]
pub struct InputBinding {
    /// ID unique across the mapping file.
    pub id: String,
    /// Target action ID.
    pub action: String,
    /// Default hardware source.
    pub source: InputSource,
    /// Direction of the output contribution.
    #[serde(default)]
    pub channel: AxisChannel,
    /// Activation rule.
    #[serde(default)]
    pub trigger: InputTrigger,
    /// Ordered modifiers.
    #[serde(default)]
    pub modifiers: Vec<InputModifier>,
}

/// Context group controlled by the host's edit/game mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Reflect, Serialize, Deserialize)]
pub enum InputDomain {
    /// Editor-only actions.
    Editor,
    /// Gameplay-only actions.
    #[default]
    Game,
}

/// A set of bindings with priority over lower contexts defining the same action.
#[derive(Debug, Clone, Reflect, Serialize, Deserialize)]
pub struct InputMappingContext {
    /// Unique context ID.
    pub id: String,
    /// Higher values take precedence (ties use context ID order).
    pub priority: i32,
    /// Which mode activates this context.
    #[serde(default)]
    pub domain: InputDomain,
    /// Entries belonging to this context.
    pub bindings: Vec<InputBinding>,
}

/// Authored `.input.ron` asset.
#[derive(Asset, Debug, Clone, Default, Reflect, Serialize, Deserialize)]
pub struct InputMappingAsset {
    /// Output declarations.
    pub actions: Vec<InputAction>,
    /// Available contexts.
    pub contexts: Vec<InputMappingContext>,
}

impl InputMappingAsset {
    /// Validates stable IDs and binding references.
    pub fn validate(&self) -> Result<(), String> {
        let mut actions = BTreeSet::new();
        let mut contexts = BTreeSet::new();
        let mut bindings = BTreeSet::new();
        for action in &self.actions {
            if action.id.is_empty() || !actions.insert(&action.id) {
                return Err(format!("invalid or duplicate action: {}", action.id));
            }
        }
        for context in &self.contexts {
            if context.id.is_empty() || !contexts.insert(&context.id) {
                return Err(format!("invalid or duplicate context: {}", context.id));
            }
            for binding in &context.bindings {
                if binding.id.is_empty()
                    || !bindings.insert(&binding.id)
                    || !actions.contains(&binding.action)
                {
                    return Err(format!(
                        "invalid binding or action reference: {}",
                        binding.id
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Default, TypePath)]
struct MappingLoader;

#[derive(Debug, thiserror::Error)]
enum MappingError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Parse(#[from] ron::error::SpannedError),
    #[error("{0}")]
    Invalid(String),
}

impl AssetLoader for MappingLoader {
    type Asset = InputMappingAsset;
    type Settings = ();
    type Error = MappingError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _: &(),
        _: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let mapping: InputMappingAsset = ron::de::from_bytes(&bytes)?;
        mapping.validate().map_err(MappingError::Invalid)?;
        Ok(mapping)
    }

    fn extensions(&self) -> &[&str] {
        &["input.ron"]
    }
}

/// Active project asset handle.
#[derive(Resource)]
pub struct ProjectInputMapping(pub Handle<InputMappingAsset>);

/// User key overrides saved by `bevy_settings` in the project-specific preferences.
#[derive(Resource, SettingsGroup, Reflect, Default)]
#[reflect(Resource, SettingsGroup, Default)]
pub struct InputOverrides {
    /// Stable binding ID to serialized hardware source (stored as strings in TOML).
    pub bindings: BTreeMap<String, String>,
}

impl InputOverrides {
    /// Returns the effective source, falling back to the asset if an override is invalid.
    pub fn source(&self, binding: &InputBinding) -> InputSource {
        self.bindings
            .get(&binding.id)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(binding.source)
    }

    /// Rebinds the input source or restores the authored value with `None`.
    pub fn set(&mut self, id: String, source: Option<InputSource>) {
        if let Some(source) = source {
            self.bindings.insert(
                id,
                serde_json::to_string(&source).expect("serializable input source"),
            );
        } else {
            self.bindings.remove(&id);
        }
    }
}

/// Context selection; editor and gameplay domains are mutually exclusive.
#[derive(Resource, Reflect, Default)]
#[reflect(Resource)]
pub struct InputContexts {
    /// Domain currently receiving input.
    pub domain: InputDomain,
    /// Context IDs disabled within this domain.
    pub disabled: BTreeSet<String>,
}

/// Optional virtual keyboard and mouse for embedded viewports, separate from native input.
#[derive(Resource, Default)]
pub struct ForwardedInput {
    /// Keys held in the embedded viewport.
    pub keys: ButtonInput<KeyCode>,
    /// Mouse buttons held in the embedded viewport.
    pub mouse: ButtonInput<MouseButton>,
}

/// System set that samples hardware and forwarded input before the fixed loop.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputSamplingSet;

/// Observer event raised once when a context enters or leaves the active set.
#[derive(Event, Debug)]
pub struct InputContextChanged {
    /// Context ID.
    pub id: String,
    /// Whether it entered the active set.
    pub active: bool,
}

/// Per-action state in the current schedule.
#[derive(Debug, Clone, Copy, Default, Reflect)]
pub struct ActionSample {
    /// Signed scalar or vector; digital actions use X equal to 0 or 1.
    pub value: Vec3,
    /// Actuated this sampling interval (including release/tap pulses).
    pub just_pressed: bool,
    /// Deactuated this sampling interval.
    pub just_released: bool,
}

/// State sampled in `PreUpdate`, available to `Update` systems.
#[derive(Resource, Default, Reflect)]
#[reflect(Resource)]
pub struct ActionState(BTreeMap<String, ActionSample>);

impl ActionState {
    /// Reads a named action. Missing actions are neutral.
    pub fn get(&self, action: &str) -> ActionSample {
        self.0.get(action).copied().unwrap_or_default()
    }
}

impl ActionSample {
    /// Whether the action is active.
    pub fn pressed(self) -> bool {
        self.value != Vec3::ZERO
    }

    /// Reads the XY axis pair.
    pub fn axis_2d(self) -> Vec2 {
        self.value.truncate()
    }
}

/// State for `FixedUpdate`; edges are buffered across render frames and consumed once.
#[derive(Resource, Default)]
pub struct FixedActionState(BTreeMap<String, ActionSample>);

impl FixedActionState {
    /// Reads the latest fixed sample for an action.
    pub fn get(&self, action: &str) -> ActionSample {
        self.0.get(action).copied().unwrap_or_default()
    }
}

#[derive(Default)]
struct BindingMemory {
    source: Option<InputSource>,
    blocked_until_release: bool,
    down: bool,
    elapsed: f32,
    last_value: f32,
}

#[derive(Resource, Default)]
struct Resolver {
    active: BTreeSet<String>,
    domain: InputDomain,
    bindings: BTreeMap<String, BindingMemory>,
    pending: BTreeMap<String, ActionSample>,
}

impl BindingMemory {
    fn evaluate(
        &mut self,
        binding: &InputBinding,
        source: InputSource,
        sample: &impl Fn(InputSource) -> f32,
        dt: f32,
    ) -> f32 {
        if self.source != Some(source) {
            *self = Self {
                source: Some(source),
                ..Default::default()
            };
        }
        let mut value = sample(source);
        for modifier in &binding.modifiers {
            match *modifier {
                InputModifier::Deadzone(n) if value.abs() <= n => value = 0.0,
                InputModifier::Scale(n) => value *= n,
                InputModifier::Invert => value = -value,
                InputModifier::Deadzone(_) => {}
            }
        }
        if !value.is_finite() {
            value = 0.0;
        }
        let down = value != 0.0;
        if self.blocked_until_release {
            self.blocked_until_release = down;
            return 0.;
        }
        let released = self.down && !down;
        let duration = if self.down { self.elapsed + dt } else { 0.0 };
        let output = match binding.trigger {
            InputTrigger::Pressed => value,
            InputTrigger::Released if released => self.last_value,
            InputTrigger::Tap(max) if released && duration <= max => self.last_value,
            InputTrigger::Hold(min) if down && duration >= min => value,
            InputTrigger::Chord(other) if sample(other) != 0.0 => value,
            _ => 0.0,
        };
        self.down = down;
        self.elapsed = if down { duration } else { 0.0 };
        self.last_value = value;
        output
    }
}

/// Installs input assets and the frame and fixed-step action resolvers.
pub struct EngineInputPlugin;

impl Plugin for EngineInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<InputMappingAsset>()
            .init_asset_loader::<MappingLoader>()
            .register_type::<InputMappingAsset>()
            .register_type::<InputContexts>()
            .register_type::<InputOverrides>()
            .register_type::<ActionState>()
            .init_resource::<InputContexts>()
            .init_resource::<InputOverrides>()
            .init_resource::<ActionState>()
            .init_resource::<FixedActionState>()
            .init_resource::<Resolver>()
            .add_systems(
                PreUpdate,
                sample_input.in_set(InputSamplingSet).after(InputSystems),
            )
            .add_systems(FixedUpdate, sample_fixed.in_set(EngineFixedSet::Input));
    }
}

#[derive(bevy_ecs::system::SystemParam)]
struct Hardware<'w, 's> {
    keys: Option<Res<'w, ButtonInput<KeyCode>>>,
    mouse: Option<Res<'w, ButtonInput<MouseButton>>>,
    gamepads: Query<'w, 's, &'static Gamepad>,
    forwarded: Option<Res<'w, ForwardedInput>>,
}

fn sample_input(
    hardware: Hardware,
    time: Res<Time<Real>>,
    selection: Res<InputContexts>,
    mapping: (
        Res<Assets<InputMappingAsset>>,
        Option<Res<ProjectInputMapping>>,
    ),
    overrides: Res<InputOverrides>,
    mut states: (ResMut<Resolver>, ResMut<ActionState>),
    mut commands: Commands,
) {
    let sample = |source| match source {
        InputSource::Key(key) => f32::from(
            hardware.keys.as_ref().is_some_and(|keys| keys.pressed(key))
                || hardware
                    .forwarded
                    .as_ref()
                    .is_some_and(|input| input.keys.pressed(key)),
        ),
        InputSource::Mouse(button) => f32::from(
            hardware
                .mouse
                .as_ref()
                .is_some_and(|mouse| mouse.pressed(button))
                || hardware
                    .forwarded
                    .as_ref()
                    .is_some_and(|input| input.mouse.pressed(button)),
        ),
        InputSource::GamepadButton(button) => {
            u8::from(hardware.gamepads.iter().any(|pad| pad.pressed(button))) as f32
        }
        InputSource::GamepadAxis(axis) => hardware
            .gamepads
            .iter()
            .filter_map(|pad| pad.get(axis))
            .max_by(|a, b| a.abs().total_cmp(&b.abs()))
            .unwrap_or_default(),
    };
    let (assets, handle) = mapping;
    let mapping = handle.and_then(|handle| assets.get(&handle.0));
    for event in states.0.resolve(
        &mut states.1,
        selection.as_ref(),
        mapping,
        &overrides,
        sample,
        time.delta_secs(),
    ) {
        commands.trigger(event);
    }
}

impl Resolver {
    fn resolve(
        &mut self,
        state: &mut ActionState,
        selection: &InputContexts,
        mapping: Option<&InputMappingAsset>,
        overrides: &InputOverrides,
        sample: impl Fn(InputSource) -> f32,
        dt: f32,
    ) -> Vec<InputContextChanged> {
        let contexts = mapping
            .into_iter()
            .flat_map(|m| &m.contexts)
            .filter(|c| c.domain == selection.domain && !selection.disabled.contains(&c.id));
        let mut contexts: Vec<_> = contexts.collect();
        contexts.sort_unstable_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
        let active: BTreeSet<_> = contexts.iter().map(|c| c.id.clone()).collect();
        let events = self
            .active
            .difference(&active)
            .map(|id| InputContextChanged {
                id: id.clone(),
                active: false,
            })
            .chain(
                active
                    .difference(&self.active)
                    .map(|id| InputContextChanged {
                        id: id.clone(),
                        active: true,
                    }),
            )
            .collect();
        self.active = active;
        let switched_domain = self.domain != selection.domain;
        if switched_domain {
            self.domain = selection.domain;
            self.pending.clear();
            state.0.clear();
            self.bindings.clear();
        }

        let mut sampled = BTreeMap::new();
        let mut used = BTreeSet::new();
        for context in contexts {
            for binding in &context.bindings {
                let (winner, value) = sampled
                    .entry(binding.action.as_str())
                    .or_insert((context.id.as_str(), Vec3::ZERO));
                if *winner == context.id {
                    used.insert(binding.id.as_str());
                    let source = overrides.source(binding);
                    let memory = self.bindings.entry(binding.id.clone()).or_default();
                    if switched_domain {
                        memory.source = Some(source);
                        memory.blocked_until_release = true;
                    }
                    let contribution = memory.evaluate(binding, source, &sample, dt);
                    value[binding.channel as usize] += contribution;
                }
            }
        }
        self.bindings.retain(|id, _| used.contains(id.as_str()));

        let mut previous = std::mem::take(&mut state.0);
        for action in mapping.into_iter().flat_map(|m| &m.actions) {
            let value = sampled
                .get(action.id.as_str())
                .map_or(Vec3::ZERO, |(_, value)| *value);
            let value = match action.axis {
                ActionAxis::Bool => Vec3::X * f32::from(value != Vec3::ZERO),
                ActionAxis::Axis1D => Vec3::X * value.x,
                ActionAxis::Axis2D => value.truncate().extend(0.0),
                ActionAxis::Axis3D => value,
            }
            .clamp(Vec3::NEG_ONE, Vec3::ONE);
            let was_pressed = previous
                .remove(&action.id)
                .is_some_and(ActionSample::pressed);
            state.0.insert(
                action.id.clone(),
                ActionSample {
                    value,
                    just_pressed: !was_pressed && value != Vec3::ZERO,
                    just_released: was_pressed && value == Vec3::ZERO,
                },
            );
        }
        for (id, old) in previous.into_iter().filter(|(_, old)| old.pressed()) {
            state.0.insert(
                id,
                ActionSample {
                    just_released: old.pressed(),
                    ..Default::default()
                },
            );
        }
        self.pending.retain(|id, sample| {
            state.0.contains_key(id) || sample.just_released || sample.just_pressed
        });
        for (id, &sample) in &state.0 {
            let pending = self.pending.entry(id.clone()).or_default();
            pending.value = sample.value;
            pending.just_pressed |= sample.just_pressed;
            pending.just_released |= sample.just_released;
        }
        events
    }
}

fn sample_fixed(mut resolver: ResMut<Resolver>, mut fixed: ResMut<FixedActionState>) {
    fixed.0.clone_from(&resolver.pending);
    for sample in resolver.pending.values_mut() {
        sample.just_pressed = false;
        sample.just_released = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_tap_chord_and_modifiers() {
        let source = InputSource::Key(KeyCode::KeyA);
        let chord = InputSource::Key(KeyCode::ShiftLeft);
        let mut binding = InputBinding {
            id: "a".into(),
            action: "act".into(),
            source,
            channel: AxisChannel::X,
            trigger: InputTrigger::Hold(0.2),
            modifiers: vec![
                InputModifier::Deadzone(0.1),
                InputModifier::Scale(0.5),
                InputModifier::Invert,
            ],
        };
        let mut memory = BindingMemory::default();
        let mut step = |binding: &InputBinding, main, modifier| {
            memory.evaluate(
                binding,
                source,
                &|which| if which == source { main } else { modifier },
                0.1,
            )
        };
        assert_eq!(step(&binding, 0.05, 0.0), 0.0);
        assert_eq!(step(&binding, 1.0, 0.0), 0.0);
        assert_eq!(step(&binding, 1.0, 0.0), 0.0);
        assert_eq!(step(&binding, 1.0, 0.0), -0.5);
        assert_eq!(step(&binding, 0.0, 0.0), 0.0);
        binding.trigger = InputTrigger::Tap(0.15);
        assert_eq!(step(&binding, 1.0, 0.0), 0.0);
        assert_eq!(step(&binding, 0.0, 0.0), -0.5);
        binding.trigger = InputTrigger::Chord(chord);
        assert_eq!(step(&binding, 1.0, 0.0), 0.0);
        assert_eq!(step(&binding, 1.0, 1.0), -0.5);
    }

    #[test]
    fn contexts_override_actions_and_fixed_edges_are_consumed_once() {
        let mapping: InputMappingAsset = ron::from_str(r#"(actions:[(id:"jump",axis:Bool)],contexts:[
            (id:"base",priority:0,bindings:[(id:"jump.default", action:"jump",source:Key(Space))]),
            (id:"menu",priority:10,bindings:[(id:"jump.menu", action:"jump",source:Key(Enter))])])"#).unwrap();
        mapping.validate().unwrap();
        let mut resolver = Resolver::default();
        let mut state = ActionState::default();
        let mut contexts = InputContexts::default();
        let overrides = InputOverrides::default();
        let mut sample = |ctx: &InputContexts, down| {
            resolver.resolve(
                &mut state,
                ctx,
                Some(&mapping),
                &overrides,
                |source| f32::from(down && source == InputSource::Key(KeyCode::Space)),
                0.01,
            )
        };
        assert_eq!(sample(&contexts, true).len(), 2);
        contexts.disabled.insert("menu".into());
        assert!(!sample(&contexts, true)[0].active);
        sample(&contexts, false);
        let mut app = App::new();
        app.insert_resource(resolver)
            .init_resource::<FixedActionState>()
            .add_systems(FixedUpdate, sample_fixed);
        app.world_mut().run_schedule(FixedUpdate);
        let jump = app.world().resource::<FixedActionState>().get("jump");
        assert!(jump.just_pressed && jump.just_released && !jump.pressed());
        app.world_mut().run_schedule(FixedUpdate);
        assert!(
            !app.world()
                .resource::<FixedActionState>()
                .get("jump")
                .just_pressed
        );
    }

    #[test]
    fn mode_change_does_not_forward_held_editor_keys_into_game() {
        let mapping: InputMappingAsset = ron::from_str(r#"(actions:[(id:"jump",axis:Bool),(id:"focus",axis:Bool)],contexts:[
            (id:"play",priority:0,bindings:[(id:"jump.default",action:"jump",source:Key(Space))]),
            (id:"edit",priority:0,domain:Editor,bindings:[(id:"focus.default",action:"focus",source:Key(Space))])])"#).unwrap();
        let mut resolver = Resolver::default();
        let mut state = ActionState::default();
        let mut contexts = InputContexts {
            domain: InputDomain::Editor,
            ..Default::default()
        };
        let overrides = InputOverrides::default();
        let step =
            |resolver: &mut Resolver, state: &mut ActionState, contexts: &InputContexts, down| {
                resolver.resolve(
                    state,
                    contexts,
                    Some(&mapping),
                    &overrides,
                    |_| f32::from(down),
                    0.016,
                )
            };
        step(&mut resolver, &mut state, &contexts, false);
        step(&mut resolver, &mut state, &contexts, true);
        assert!(state.get("focus").just_pressed);
        contexts.domain = InputDomain::Game;
        let events = step(&mut resolver, &mut state, &contexts, true);
        assert_eq!(events.len(), 2);
        assert!(!events[0].active && events[1].active);
        assert!(!state.get("jump").pressed());
        assert!(!resolver.pending["focus"].just_pressed);
        step(&mut resolver, &mut state, &contexts, false);
        step(&mut resolver, &mut state, &contexts, true);
        assert!(state.get("jump").just_pressed);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn overrides_persist_and_reload_through_bevy_settings() {
        use bevy_settings::{SaveSettingsSync, SettingsPlugin};
        let path = std::env::temp_dir().join(format!(
            "bevy9-test-input-{}",
            engine_core::EntityGuid::new().as_uuid()
        ));
        let configure = || {
            let mut app = App::new();
            app.register_type::<InputOverrides>()
                .init_resource::<InputOverrides>()
                .add_plugins(SettingsPlugin::new(path.to_str().unwrap()));
            app
        };
        let mut app = configure();
        let rebound = InputSource::Key(KeyCode::KeyJ);
        app.world_mut()
            .resource_mut::<InputOverrides>()
            .set("move.left".into(), Some(rebound));
        SaveSettingsSync::Always.apply(app.world_mut());
        let mut app = configure();
        let binding = InputBinding {
            id: "move.left".into(),
            action: "move".into(),
            source: InputSource::Key(KeyCode::KeyA),
            channel: AxisChannel::X,
            trigger: InputTrigger::Pressed,
            modifiers: vec![],
        };
        assert_eq!(
            app.world().resource::<InputOverrides>().source(&binding),
            rebound
        );
        app.world_mut()
            .resource_mut::<InputOverrides>()
            .set(binding.id.clone(), None);
        SaveSettingsSync::Always.apply(app.world_mut());
        assert_eq!(
            configure()
                .world()
                .resource::<InputOverrides>()
                .source(&binding),
            binding.source
        );
        std::fs::remove_dir_all(path).unwrap();
    }
}
