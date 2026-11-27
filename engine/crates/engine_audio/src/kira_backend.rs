//! Kira mixer implementation behind the backend contract.

use std::{collections::BTreeMap, time::Duration};

use kira::{
    backend::Backend,
    effect::{
        filter::{FilterBuilder, FilterHandle},
        reverb::{ReverbBuilder, ReverbHandle},
    },
    listener::ListenerHandle,
    sound::{
        static_sound::{StaticSoundData, StaticSoundHandle},
        streaming::{StreamingSoundData, StreamingSoundHandle},
        FromFileError, PlaybackState,
    },
    track::{
        SendTrackBuilder, SendTrackHandle, SpatialTrackBuilder, SpatialTrackHandle, TrackBuilder,
        TrackHandle,
    },
    AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Mix, Tween,
};

use crate::{
    sane, AudioBackend, AudioBus, AudioError, AudioSpace, BusSettings, PlaySound, Quat, Vec3,
    VoiceId,
};

const VOICE_LIMIT: usize = 64;

struct BusTrack {
    track: TrackHandle,
    filter: FilterHandle,
}

enum Playback {
    Static(StaticSoundHandle),
    Streaming(StreamingSoundHandle<FromFileError>),
}

impl Playback {
    fn state(&self) -> PlaybackState {
        match self {
            Self::Static(voice) => voice.state(),
            Self::Streaming(voice) => voice.state(),
        }
    }

    fn stop(&mut self) {
        match self {
            Self::Static(voice) => voice.stop(tween()),
            Self::Streaming(voice) => voice.stop(tween()),
        }
    }
}

struct Voice {
    playback: Playback,
    spatial: Option<(SpatialTrackHandle, AudioSpace)>,
}

/// Mixer backed by Kira. `B` can be Kira's mock backend for tests without an audio device.
pub struct KiraAudioBackend<B: Backend = DefaultBackend> {
    manager: AudioManager<B>,
    master_filter: FilterHandle,
    master_reverb: ReverbHandle,
    reverb_send: SendTrackHandle,
    buses: BTreeMap<AudioBus, BusTrack>,
    listener_2d: ListenerHandle,
    listener_3d: ListenerHandle,
    voices: BTreeMap<VoiceId, Voice>,
    next_id: u64,
}

impl<B> KiraAudioBackend<B>
where
    B: Backend,
    B::Settings: Default,
    B::Error: std::fmt::Debug,
{
    /// Starts Kira with the engine's standard mixer routing.
    pub fn new() -> Result<Self, AudioError> {
        Self::with_device_settings(B::Settings::default())
    }

    /// Starts Kira with an explicitly configured device or mock sample rate.
    pub fn with_device_settings(backend_settings: B::Settings) -> Result<Self, AudioError> {
        let mut settings = AudioManagerSettings::<B> {
            backend_settings,
            ..Default::default()
        };
        settings.main_track_builder = settings.main_track_builder.sound_capacity(VOICE_LIMIT);
        let master_filter = settings
            .main_track_builder
            .add_effect(FilterBuilder::new().mix(Mix(0.)));
        let master_reverb = settings
            .main_track_builder
            .add_effect(ReverbBuilder::new().mix(Mix(0.)));
        let mut manager = AudioManager::new(settings).map_err(backend_error)?;
        let reverb_send = manager
            .add_send_track(SendTrackBuilder::new().with_effect(ReverbBuilder::new().mix(Mix(1.))))
            .map_err(backend_error)?;
        let mut buses = BTreeMap::new();
        for bus in AudioBus::ALL
            .into_iter()
            .filter(|&bus| bus != AudioBus::Master)
        {
            let mut builder = TrackBuilder::new()
                .sound_capacity(VOICE_LIMIT)
                .sub_track_capacity(VOICE_LIMIT)
                .with_send(&reverb_send, Decibels::SILENCE);
            let filter = builder.add_effect(FilterBuilder::new().mix(Mix(0.)));
            let track = manager.add_sub_track(builder).map_err(backend_error)?;
            buses.insert(bus, BusTrack { track, filter });
        }
        let listener_2d = manager
            .add_listener(position(Vec3::ZERO), orientation(Quat::IDENTITY))
            .map_err(backend_error)?;
        let listener_3d = manager
            .add_listener(position(Vec3::ZERO), orientation(Quat::IDENTITY))
            .map_err(backend_error)?;
        Ok(Self {
            manager,
            master_filter,
            master_reverb,
            reverb_send,
            buses,
            listener_2d,
            listener_3d,
            voices: BTreeMap::new(),
            next_id: 0,
        })
    }
}

impl<B: Backend + Send + Sync + 'static> AudioBackend for KiraAudioBackend<B> {
    fn play(&mut self, sound: &PlaySound) -> Result<VoiceId, AudioError> {
        self.collect_finished();
        if self.voices.len() >= VOICE_LIMIT {
            return Err(AudioError::Capacity);
        }
        let mut spatial =
            if let Some(spatial) = sound.spatial.filter(|s| s.space != AudioSpace::Global) {
                let min = sane(spatial.min_distance, 1., 0., 1e8);
                let max = sane(spatial.max_distance, 100., 0., 1e9)
                    .max(min + 16. * f32::EPSILON * min.max(1.));
                let listener = if spatial.space == AudioSpace::Scene2D {
                    &self.listener_2d
                } else {
                    &self.listener_3d
                };
                let builder = SpatialTrackBuilder::new()
                    .sound_capacity(1)
                    .distances([min, max]);
                let pos = position(in_space(spatial.position, spatial.space));
                let track = if let Some(bus) = self.buses.get_mut(&sound.bus) {
                    bus.track.add_spatial_sub_track(listener, pos, builder)
                } else {
                    self.manager.add_spatial_sub_track(listener, pos, builder)
                }
                .map_err(|_| AudioError::Capacity)?;
                Some((track, spatial.space))
            } else {
                None
            };
        let route = self.buses.get_mut(&sound.bus);
        let volume = gain_db(sane(sound.gain, 1., 0., 1.));
        let pitch = f64::from(sane(sound.pitch, 1., 0.125, 4.));
        let playback = if sound.streaming {
            let mut data = StreamingSoundData::from_file(&sound.path)
                .map_err(backend_error)?
                .volume(volume)
                .playback_rate(pitch);
            if sound.looping {
                data = data.loop_region(..);
            }
            Playback::Streaming(
                match (&mut spatial, route) {
                    (Some((track, _)), _) => track.play(data),
                    (None, Some(bus)) => bus.track.play(data),
                    (None, None) => self.manager.play(data),
                }
                .map_err(backend_error)?,
            )
        } else {
            let mut data = StaticSoundData::from_file(&sound.path)
                .map_err(backend_error)?
                .volume(volume)
                .playback_rate(pitch);
            if sound.looping {
                data = data.loop_region(..);
            }
            Playback::Static(
                match (&mut spatial, route) {
                    (Some((track, _)), _) => track.play(data),
                    (None, Some(bus)) => bus.track.play(data),
                    (None, None) => self.manager.play(data),
                }
                .map_err(backend_error)?,
            )
        };
        let id = VoiceId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        self.voices.insert(id, Voice { playback, spatial });
        Ok(id)
    }

    fn stop(&mut self, id: VoiceId) {
        if let Some(voice) = self.voices.get_mut(&id) {
            voice.playback.stop();
        }
    }

    fn set_bus(&mut self, bus: AudioBus, settings: BusSettings) {
        let settings = settings.sanitized();
        let volume = gain_db(settings.gain);
        let filter = if let Some(route) = self.buses.get_mut(&bus) {
            route.track.set_volume(volume, tween());
            let _ = route
                .track
                .set_send(&self.reverb_send, gain_db(settings.reverb), tween());
            &mut route.filter
        } else {
            self.manager.main_track().set_volume(volume, tween());
            self.master_reverb.set_mix(Mix(settings.reverb), tween());
            &mut self.master_filter
        };
        filter.set_cutoff(f64::from(settings.low_pass_hz), tween());
        filter.set_mix(
            Mix(if settings.low_pass_hz < 20_000. {
                1.
            } else {
                0.
            }),
            tween(),
        );
    }

    fn set_listener(&mut self, center: Vec3, rotation: Quat) {
        let rotation = if rotation.is_finite() && rotation.length_squared() > 1e-8 {
            rotation.normalize()
        } else {
            Quat::IDENTITY
        };
        self.listener_2d
            .set_position(position(in_space(center, AudioSpace::Scene2D)), tween());
        self.listener_2d.set_orientation(
            orientation(Quat::from_rotation_z(
                rotation.to_euler(bevy_math::EulerRot::ZYX).0,
            )),
            tween(),
        );
        self.listener_3d.set_position(position(center), tween());
        self.listener_3d
            .set_orientation(orientation(rotation), tween());
    }

    fn set_position(&mut self, id: VoiceId, center: Vec3) {
        if let Some((track, space)) = self
            .voices
            .get_mut(&id)
            .and_then(|voice| voice.spatial.as_mut())
        {
            track.set_position(position(in_space(center, *space)), tween());
        }
    }

    fn collect_finished(&mut self) {
        self.voices
            .retain(|_, voice| voice.playback.state() != PlaybackState::Stopped);
    }
}

fn tween() -> Tween {
    Tween {
        duration: Duration::from_millis(35),
        ..Default::default()
    }
}

fn gain_db(gain: f32) -> Decibels {
    if gain <= 0.0001 {
        Decibels::SILENCE
    } else {
        Decibels(20. * bevy_math::ops::log10(gain))
    }
}

fn backend_error(error: impl std::fmt::Debug) -> AudioError {
    AudioError::Backend(format!("Kira: {error:?}"))
}

fn position(v: Vec3) -> mint::Vector3<f32> {
    if v.is_finite() {
        v.to_array().into()
    } else {
        [0.; 3].into()
    }
}
fn orientation(q: Quat) -> mint::Quaternion<f32> {
    mint::Quaternion {
        v: [q.x, q.y, q.z].into(),
        s: q.w,
    }
}
fn in_space(v: Vec3, space: AudioSpace) -> Vec3 {
    if space == AudioSpace::Scene2D {
        v.truncate().extend(0.)
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Write, sync::Mutex};

    use kira::backend::{mock::MockBackendSettings, Renderer};

    use super::*;

    /// Kira's real renderer with the output buffer exposed to assertions.
    struct CaptureBackend {
        renderer: Option<Mutex<Renderer>>,
        output: Vec<f32>,
    }

    impl Backend for CaptureBackend {
        type Settings = MockBackendSettings;
        type Error = ();

        fn setup(settings: Self::Settings, buffer_size: usize) -> Result<(Self, u32), ()> {
            Ok((
                Self {
                    renderer: None,
                    output: vec![0.; buffer_size * 2],
                },
                settings.sample_rate,
            ))
        }

        fn start(&mut self, renderer: Renderer) -> Result<(), ()> {
            self.renderer = Some(Mutex::new(renderer));
            Ok(())
        }
    }

    fn render(backend: &mut KiraAudioBackend<CaptureBackend>) -> f32 {
        let mock = backend.manager.backend_mut();
        let renderer = mock.renderer.as_mut().unwrap().get_mut().unwrap();
        for _ in 0..20 {
            renderer.on_start_processing();
            renderer.process(&mut mock.output, 2);
        }
        mock.output.iter().map(|v| v.abs()).fold(0., f32::max)
    }

    #[test]
    fn bus_gain_changes_rendered_output_of_live_2d_and_3d_voices() {
        let mut backend =
            KiraAudioBackend::<CaptureBackend>::with_device_settings(MockBackendSettings {
                sample_rate: 48_000,
            })
            .unwrap();
        let path =
            std::env::temp_dir().join(format!("bevy9-audio-test-{}.wav", std::process::id()));
        let mut file = std::fs::File::create(&path).unwrap();
        let header = b"RIFF\x24\x20\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\xbb\0\0\0\x77\x01\0\x02\0\x10\0data\0\x20\0\0";
        file.write_all(header).unwrap();
        for i in 0..4096 {
            let sample = if i % 100 < 50 { 8192i16 } else { -8192 };
            file.write_all(&sample.to_le_bytes()).unwrap();
        }
        drop(file);
        let mut sound = PlaySound {
            path: path.clone(),
            bus: AudioBus::Sfx,
            spatial: None,
            gain: 1.,
            pitch: 1.,
            streaming: false,
            looping: true,
        };
        backend.set_listener(Vec3::ZERO, Quat::IDENTITY);
        for (space, bus) in [
            (AudioSpace::Scene2D, AudioBus::Sfx),
            (AudioSpace::Scene3D, AudioBus::Master),
        ] {
            sound.bus = bus;
            sound.spatial = Some(crate::SpatialSound {
                space,
                position: Vec3::ZERO,
                min_distance: 1.,
                max_distance: 30.,
            });
            let voice = backend.play(&sound).unwrap();
            let full = render(&mut backend);
            assert!(full > 0.05, "{space:?} must produce output: {full}");
            backend.set_bus(
                bus,
                BusSettings {
                    gain: 0.25,
                    ..Default::default()
                },
            );
            let quiet = render(&mut backend);
            assert!(
                (quiet / full - 0.25).abs() < 0.02,
                "{space:?}: {quiet} / {full}"
            );
            backend.set_position(voice, Vec3::X * 50.);
            assert!(
                render(&mut backend) < 0.001,
                "{space:?} must attenuate with distance"
            );
            assert_eq!(
                backend.voices[&voice].playback.state(),
                PlaybackState::Playing
            );
            backend.stop(voice);
            render(&mut backend);
            backend.collect_finished();
            assert!(backend.voices.is_empty());
            backend.set_bus(bus, BusSettings::default());
        }
        std::fs::remove_file(path).unwrap();
    }
}
