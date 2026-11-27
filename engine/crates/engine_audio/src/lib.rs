//! Kira-backed game audio.

mod kira_backend;
pub use kira_backend::KiraAudioBackend;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use bevy_app::{App, Plugin, PostUpdate, Startup, Update};
use bevy_ecs::{
    prelude::*,
    reflect::{ReflectComponent, ReflectResource},
};
use bevy_math::{Quat, Vec3};
use bevy_reflect::{prelude::ReflectDefault, Reflect};
use bevy_transform::{
    components::{GlobalTransform, Transform},
    TransformSystems,
};
use engine_assets::AssetRegistry;
use engine_core::{AssetUuid, EngineSet};
use engine_protocol::AssetKind;
use serde::{Deserialize, Serialize};

/// Mixer routing for a playback voice.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Reflect, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AudioBus {
    /// Overall output; can also be used for direct playback.
    Master,
    /// Sound effects.
    #[default]
    Sfx,
    /// Background music.
    Music,
    /// UI and asset preview.
    Ui,
    /// Dialog and other voice audio.
    Voice,
}

impl AudioBus {
    /// Buses in display order.
    pub const ALL: [Self; 5] = [Self::Master, Self::Sfx, Self::Music, Self::Ui, Self::Voice];
}

/// Editable mixer controls for one bus.
#[derive(Debug, Clone, Copy, PartialEq, Reflect, Serialize, Deserialize)]
pub struct BusSettings {
    /// Linear volume, from silence (0) to unity (1).
    pub gain: f32,
    /// Low-pass cutoff in Hz; 20000 bypasses the filter.
    pub low_pass_hz: f32,
    /// Reverb level (0–1).
    pub reverb: f32,
}

impl Default for BusSettings {
    fn default() -> Self {
        Self {
            gain: 1.,
            low_pass_hz: 20_000.,
            reverb: 0.,
        }
    }
}

/// Live per-bus settings. Changes are sent to the backend without restarting existing voices.
#[derive(Resource, Default, Reflect, Serialize, Deserialize)]
#[reflect(Resource, Default)]
pub struct AudioMixer {
    /// Master controls.
    pub master: BusSettings,
    /// SFX bus controls.
    pub sfx: BusSettings,
    /// Music bus controls.
    pub music: BusSettings,
    /// UI bus controls.
    pub ui: BusSettings,
    /// Voice bus controls.
    pub voice: BusSettings,
}

impl AudioMixer {
    /// Reads one bus.
    pub fn get(&self, bus: AudioBus) -> BusSettings {
        match bus {
            AudioBus::Master => self.master,
            AudioBus::Sfx => self.sfx,
            AudioBus::Music => self.music,
            AudioBus::Ui => self.ui,
            AudioBus::Voice => self.voice,
        }
    }

    /// Updates one bus, clamping all controls to their supported ranges.
    pub fn set(&mut self, bus: AudioBus, settings: BusSettings) {
        let target = match bus {
            AudioBus::Master => &mut self.master,
            AudioBus::Sfx => &mut self.sfx,
            AudioBus::Music => &mut self.music,
            AudioBus::Ui => &mut self.ui,
            AudioBus::Voice => &mut self.voice,
        };
        *target = settings.sanitized();
    }
}

impl BusSettings {
    fn sanitized(self) -> Self {
        Self {
            gain: sane(self.gain, 1., 0., 1.),
            low_pass_hz: sane(self.low_pass_hz, 20_000., 20., 20_000.),
            reverb: sane(self.reverb, 0., 0., 1.),
        }
    }
}

fn sane(value: f32, fallback: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

/// Coordinate space used for positioning.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Reflect)]
pub enum AudioSpace {
    /// Non-positional playback.
    #[default]
    Global,
    /// Use XY; ignore depth and listener pitch/roll.
    Scene2D,
    /// Use world XYZ and full listener rotation.
    Scene3D,
}

/// Audio playback settings authored on a scene entity.
#[derive(Component, Debug, Clone, PartialEq, Reflect)]
#[reflect(Component, Default)]
#[require(Transform)]
pub struct AudioEmitter {
    /// Stable asset reference, resolved at the moment playback starts.
    pub clip: Option<AssetUuid>,
    /// Output bus.
    pub bus: AudioBus,
    /// Spatial mode.
    pub space: AudioSpace,
    /// Start when this entity enters the running game.
    pub autoplay: bool,
    /// Repeat the entire clip.
    pub looping: bool,
    /// Stream audio from disk instead of predecoding.
    pub streaming: bool,
    /// Base gain, from 0 to 1.
    pub gain: f32,
    /// Random +/- variation in linear gain.
    pub gain_variation: f32,
    /// Base playback rate multiplier.
    pub pitch: f32,
    /// Random +/- variation in playback rate multiplier.
    pub pitch_variation: f32,
    /// Distance in scene units inside which gain does not attenuate.
    pub min_distance: f32,
    /// Distance in scene units at which the sound is silent.
    pub max_distance: f32,
}

impl Default for AudioEmitter {
    fn default() -> Self {
        Self {
            clip: None,
            bus: AudioBus::Sfx,
            space: AudioSpace::Global,
            autoplay: false,
            looping: false,
            streaming: false,
            gain: 1.,
            gain_variation: 0.,
            pitch: 1.,
            pitch_variation: 0.,
            min_distance: 1.,
            max_distance: 100.,
        }
    }
}

impl AudioEmitter {
    fn sound(&self, path: PathBuf, position: Vec3) -> PlaySound {
        PlaySound {
            path,
            bus: self.bus,
            spatial: (self.space != AudioSpace::Global).then_some(SpatialSound {
                space: self.space,
                position,
                min_distance: self.min_distance,
                max_distance: self.max_distance,
            }),
            gain: self.gain,
            pitch: self.pitch,
            streaming: self.streaming,
            looping: self.looping,
        }
    }
}

/// Selects the spatial listener. The highest priority enabled listener wins.
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component, Default)]
#[require(Transform)]
pub struct AudioListener {
    /// Whether this listener is a candidate.
    pub enabled: bool,
    /// Priority; use a larger value to select a camera's listener.
    pub priority: i32,
}

impl Default for AudioListener {
    fn default() -> Self {
        Self {
            enabled: true,
            priority: 0,
        }
    }
}

/// A positional sound description sent to a backend.
#[derive(Debug, Clone, Copy)]
pub struct SpatialSound {
    /// Spatial mode (2D or 3D).
    pub space: AudioSpace,
    /// World position in scene units.
    pub position: Vec3,
    /// Full-volume distance.
    pub min_distance: f32,
    /// Silent distance.
    pub max_distance: f32,
}

/// Playback request addressed to a concrete decoded/streamable audio file.
#[derive(Debug, Clone)]
pub struct PlaySound {
    /// Source on disk.
    pub path: PathBuf,
    /// Route.
    pub bus: AudioBus,
    /// Optional positional emitter data.
    pub spatial: Option<SpatialSound>,
    /// Linear gain.
    pub gain: f32,
    /// Pitch multiplier.
    pub pitch: f32,
    /// Stream instead of predecoding.
    pub streaming: bool,
    /// Repeat.
    pub looping: bool,
}

/// Opaque per-backend voice ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VoiceId(pub u64);

/// An audio backend or source error.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// No physical or injected backend is active.
    #[error("audio device unavailable")]
    Unavailable,
    /// The asset is missing or not recognized as audio.
    #[error("audio asset {0} not found")]
    MissingAsset(AssetUuid),
    /// Playback capacity is exhausted.
    #[error("audio voice capacity reached")]
    Capacity,
    /// The backend or decoder rejected an operation.
    #[error("{0}")]
    Backend(String),
}

/// Playback and mixing, decoupled from the ECS and from the particular audio library.
pub trait AudioBackend: Send + Sync + 'static {
    /// Starts a voice and returns its ID.
    fn play(&mut self, sound: &PlaySound) -> Result<VoiceId, AudioError>;
    /// Stops a voice if still playing.
    fn stop(&mut self, voice: VoiceId);
    /// Applies updated settings to the bus, including already active voices.
    fn set_bus(&mut self, bus: AudioBus, settings: BusSettings);
    /// Moves the spatial listener in world coordinates.
    fn set_listener(&mut self, position: Vec3, orientation: Quat);
    /// Moves an active emitter.
    fn set_position(&mut self, voice: VoiceId, position: Vec3);
    /// Releases finished voices.
    fn collect_finished(&mut self);
}

/// Runtime backend and preview session.
#[derive(Resource, Default)]
pub struct AudioEngine {
    backend: Option<Box<dyn AudioBackend>>,
    preview: Option<(VoiceId, Option<Entity>)>,
}

impl AudioEngine {
    /// Uses an injected backend (for example a headless implementation).
    pub fn with_backend(backend: impl AudioBackend) -> Self {
        Self {
            backend: Some(Box::new(backend)),
            preview: None,
        }
    }

    /// Whether an audio backend is active.
    pub fn available(&self) -> bool {
        self.backend.is_some()
    }

    /// Plays through the active backend.
    pub fn play(&mut self, sound: &PlaySound) -> Result<VoiceId, AudioError> {
        self.backend
            .as_mut()
            .ok_or(AudioError::Unavailable)?
            .play(sound)
    }

    /// Stops a voice through the active backend.
    pub fn stop(&mut self, voice: VoiceId) {
        if let Some(backend) = &mut self.backend {
            backend.stop(voice);
        }
    }

    /// Streams one non-spatial preview via UI. Only one preview can play at a time.
    pub fn preview(&mut self, path: PathBuf) -> Result<(), AudioError> {
        self.stop_preview();
        let voice = self.play(&PlaySound {
            path,
            bus: AudioBus::Ui,
            spatial: None,
            gain: 0.8,
            pitch: 1.,
            streaming: true,
            looping: false,
        })?;
        self.preview = Some((voice, None));
        Ok(())
    }

    /// Previews an authored emitter using its bus, spatial settings, and current scene position.
    pub fn preview_emitter(
        &mut self,
        entity: Entity,
        emitter: &AudioEmitter,
        path: PathBuf,
        position: Vec3,
    ) -> Result<(), AudioError> {
        self.stop_preview();
        let voice = self.play(&emitter.sound(path, position))?;
        self.preview = Some((voice, Some(entity)));
        Ok(())
    }

    /// Stops any current preview.
    pub fn stop_preview(&mut self) {
        if let Some((voice, _)) = self.preview.take() {
            self.stop(voice);
        }
    }
}

/// Resolves an indexed audio clip. Arbitrary paths are never accepted by editor preview.
pub fn resolve_clip(registry: &AssetRegistry, uuid: AssetUuid) -> Result<PathBuf, AudioError> {
    registry
        .get(uuid)
        .filter(|record| record.kind == AssetKind::Audio)
        .and_then(|_| registry.resolve_abs_path(uuid))
        .ok_or(AudioError::MissingAsset(uuid))
}

/// Independently control hardware initialization and scene autoplay.
#[derive(Resource, Debug, Clone, Copy)]
pub struct AudioOptions {
    /// Attempt to initialize the system device on startup.
    pub device: bool,
    /// Activate gameplay audio emitters.
    pub gameplay: bool,
}

impl Default for AudioOptions {
    fn default() -> Self {
        Self {
            device: true,
            gameplay: true,
        }
    }
}

/// Installs mixer and emitter/listener integration.
pub struct EngineAudioPlugin;

impl Plugin for EngineAudioPlugin {
    fn build(&self, app: &mut App) {
        register_components(app);
        app.init_resource::<AudioOptions>()
            .init_resource::<AudioEngine>()
            .init_resource::<AudioMixer>()
            .init_resource::<ActiveEmitters>()
            .register_type::<AudioMixer>()
            .add_systems(Startup, open_device)
            .add_systems(Update, update_mixer.in_set(EngineSet::Audio))
            .add_systems(PostUpdate, sync_emitters.after(TransformSystems::Propagate));
    }
}

/// Registers authored components for serialization without opening a device.
pub fn register_components(app: &mut App) {
    app.register_type::<AudioEmitter>()
        .register_type::<AudioListener>();
}

fn open_device(options: Res<AudioOptions>, mut audio: ResMut<AudioEngine>) {
    if options.device && !audio.available() {
        match KiraAudioBackend::<kira::DefaultBackend>::new() {
            Ok(backend) => *audio = AudioEngine::with_backend(backend),
            Err(err) => tracing::warn!(%err, "failed to open audio output"),
        }
    }
}

fn update_mixer(mixer: Res<AudioMixer>, mut audio: ResMut<AudioEngine>) {
    if let Some(backend) = &mut audio.backend {
        if mixer.is_changed() {
            for bus in AudioBus::ALL {
                backend.set_bus(bus, mixer.get(bus).sanitized());
            }
        }
        backend.collect_finished();
    }
}

#[derive(Resource)]
struct ActiveEmitters {
    voices: BTreeMap<Entity, (AudioEmitter, Option<VoiceId>)>,
    rng: u64,
}

impl Default for ActiveEmitters {
    fn default() -> Self {
        Self {
            voices: BTreeMap::new(),
            rng: 0xa194_1a07_34dc_1e57,
        }
    }
}

fn variation(state: &mut u64, spread: f32) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    ((*state >> 40) as f32 / 8_388_608. - 1.) * sane(spread, 0., 0., 4.)
}

fn sync_emitters(
    options: Res<AudioOptions>,
    mut audio: ResMut<AudioEngine>,
    registry: Option<Res<AssetRegistry>>,
    mut active: ResMut<ActiveEmitters>,
    emitters: Query<(Entity, &AudioEmitter, &GlobalTransform)>,
    listeners: Query<(Entity, &AudioListener, &GlobalTransform)>,
) {
    let audio = audio.as_mut();
    let Some(backend) = &mut audio.backend else {
        return;
    };
    if options.gameplay || audio.preview.is_some_and(|(_, entity)| entity.is_some()) {
        let listener = listeners
            .iter()
            .filter(|(_, l, _)| l.enabled)
            .max_by_key(|(e, l, _)| (l.priority, *e));
        if let Some((_, _, transform)) = listener {
            backend.set_listener(transform.translation(), transform.rotation());
        } else {
            backend.set_listener(Vec3::ZERO, Quat::IDENTITY);
        }
    }
    if let Some((voice, Some(entity))) = audio.preview {
        if let Ok((_, _, transform)) = emitters.get(entity) {
            backend.set_position(voice, transform.translation());
        } else {
            backend.stop(voice);
            audio.preview = None;
        }
    }
    if !options.gameplay {
        for (_, (_, voice)) in std::mem::take(&mut active.voices) {
            if let Some(voice) = voice {
                backend.stop(voice);
            }
        }
        return;
    }

    let mut present = BTreeSet::new();
    for (entity, emitter, transform) in &emitters {
        let Some(uuid) = emitter.clip.filter(|_| emitter.autoplay) else {
            continue;
        };
        present.insert(entity);
        let position = transform.translation();
        if let Some((old, voice)) = active.voices.get(&entity)
            && old == emitter
        {
            if let Some(voice) = voice {
                backend.set_position(*voice, position);
            }
            continue;
        }
        if let Some((_, Some(voice))) = active.voices.remove(&entity) {
            backend.stop(voice);
        }
        let Some(path) = registry
            .as_ref()
            .and_then(|registry| resolve_clip(registry, uuid).ok())
        else {
            continue;
        };
        let mut sound = emitter.sound(path, position);
        sound.gain += variation(&mut active.rng, emitter.gain_variation);
        sound.pitch += variation(&mut active.rng, emitter.pitch_variation);
        let voice = match backend.play(&sound) {
            Ok(voice) => Some(voice),
            Err(AudioError::Capacity) => continue,
            Err(err) => {
                tracing::warn!(%err, %entity, "failed to play audio emitter");
                None
            }
        };
        active.voices.insert(entity, (emitter.clone(), voice));
    }
    active.voices.retain(|entity, (_, voice)| {
        if present.contains(entity) {
            return true;
        }
        if let Some(voice) = voice {
            backend.stop(*voice);
        }
        false
    });
}
