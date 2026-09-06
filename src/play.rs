//! Data-driven playback components (requirements 2 and 3), mirroring
//! `bevy_audio::AudioPlayer` + `bevy_audio::PlaybackSettings`.
//!
//! Every playable source implements the [`MidiSource`] trait and resolves to a
//! shared `Arc<MidiFile>`; the engine then plays all of them through the exact
//! same scheduling state machine. Two built-in implementations:
//!
//! * [`FileSource`] — a [`MidiFileAsset`] (requirement 3), resolved lazily.
//! * [`SequenceSource`] — a custom list of timed [`TimedMidiEvent`]s
//!   (requirement 2), converted eagerly.
//!
//! [`MidiPlayer`] is the spawned component holding a `Box<dyn MidiSource>` —
//! you can plug in your own source by implementing [`MidiSource`].
//!
//! Spawn an entity with a [`MidiSoundFont`] (its synthesizer node) plus a
//! [`MidiPlayer`] source (the settings component is auto-added via
//! `#[require]`):
//!
//! ```
//! # use bevy_asset::Handle;
//! # use bevy_ecs::prelude::*;
//! # use bevy_soundfont_synth::assets::{MidiFileAsset, SoundFontAsset};
//! # use bevy_soundfont_synth::play::{MidiPlayer, MidiPlaybackSettings, MidiSoundFont};
//! # fn example(mut commands: Commands) {
//! # let font: Handle<SoundFontAsset> = Handle::default();
//! # let midi: Handle<MidiFileAsset> = Handle::default();
//! // A synth entity that loops a MIDI file (requirement 3):
//! commands.spawn((
//!     MidiSoundFont(font),
//!     MidiPlayer::file(midi),
//!     MidiPlaybackSettings::LOOP,
//! ));
//! # }
//! ```
//!
//! In-place control (pause / speed / volume) is done by mutating
//! `MidiPlaybackSettings`; removing `MidiPlayer` (or despawning the entity)
//! stops playback. The synthesizer node itself lives as long as
//! `MidiSoundFont`.

use std::sync::Arc;

use bevy_asset::{Assets, Handle};
use bevy_ecs::prelude::Component;
use firewheel::Volume;
use rustysynth_ext::MidiFile;

use crate::assets::{MidiFileAsset, SoundFontAsset};
use crate::midi::{TimedMidiEvent, build_midi_file};

/// Declares the SoundFont bank this entity's synthesizer uses (per entity, not
/// global).
///
/// Spawn an entity with this component to give it a live synthesizer node; MIDI
/// events triggered on that entity (see
/// [`crate::events::MidiEvent`](crate::events::MidiEvent)) are routed to it,
/// and [`MidiPlayer`] sources play through it.
///
/// ```
/// # use bevy_asset::Handle;
/// # use bevy_ecs::prelude::*;
/// # use bevy_soundfont_synth::assets::SoundFontAsset;
/// # use bevy_soundfont_synth::play::MidiSoundFont;
/// # fn example(mut commands: Commands) {
/// # let font: Handle<SoundFontAsset> = Handle::default();
/// commands.spawn(MidiSoundFont(font));
/// # }
/// ```
#[derive(Component, Clone, Debug)]
pub struct MidiSoundFont(pub Handle<SoundFontAsset>);

/// A playable source. Implementations resolve themselves (asynchronously or
/// eagerly) to a shared `Arc<MidiFile>`, the common input of the playback
/// scheduler.
pub trait MidiSource: Send + Sync + 'static {
    /// Resolve this source into a parsed `MidiFile`, or fail if it cannot be
    /// played. Returning [`MidiResolveError::AssetNotLoaded`] keeps the player
    /// pending — the engine retries automatically.
    fn resolve(&self, midis: &Assets<MidiFileAsset>) -> Result<Arc<MidiFile>, MidiResolveError>;
}

/// A [`MidiSource`] backed by a [`MidiFileAsset`] (requirement 3).
///
/// Resolved lazily: playback starts once the asset is loaded.
#[derive(Clone, Debug)]
pub struct FileSource(pub Handle<MidiFileAsset>);

impl MidiSource for FileSource {
    fn resolve(&self, midis: &Assets<MidiFileAsset>) -> Result<Arc<MidiFile>, MidiResolveError> {
        midis
            .get(&self.0)
            .map(|asset| Arc::clone(&asset.midi))
            .ok_or(MidiResolveError::AssetNotLoaded)
    }
}

/// A [`MidiSource`] backed by a custom list of timed MIDI events
/// (requirement 2). Converted to `Arc<MidiFile>` when the player is added;
/// `MidiFile::new_with_events` requires non-decreasing times.
#[derive(Clone, Debug, Default)]
pub struct SequenceSource(pub Vec<TimedMidiEvent>);

impl MidiSource for SequenceSource {
    fn resolve(&self, _midis: &Assets<MidiFileAsset>) -> Result<Arc<MidiFile>, MidiResolveError> {
        build_midi_file(self.0.clone())
            .map_err(|err| MidiResolveError::InvalidSequence(err.to_string()))
    }
}

/// Why a [`MidiSource`] could not be resolved (yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiResolveError {
    /// A source whose asset is not loaded yet (e.g. a file still streaming
    /// in). The engine retries automatically.
    AssetNotLoaded,
    /// A sequence source with invalid event times (must be non-decreasing).
    InvalidSequence(String),
}

/// The playback source component (requirements 2 and 3).
///
/// Mirrors `bevy_audio::AudioPlayer`: spawn it on an entity that also has a
/// [`MidiSoundFont`] (optionally with [`MidiPlaybackSettings`]) and the engine
/// starts playback as soon as the source can be resolved.
#[derive(Component)]
#[require(MidiPlaybackSettings)]
pub struct MidiPlayer(pub Box<dyn MidiSource>);

impl MidiPlayer {
    /// A player for a MIDI file asset.
    pub fn file(handle: Handle<MidiFileAsset>) -> Self {
        Self(Box::new(FileSource(handle)))
    }

    /// A player for a list of timed MIDI events.
    pub fn sequence(events: Vec<TimedMidiEvent>) -> Self {
        Self(Box::new(SequenceSource(events)))
    }

    /// A player wrapping a custom source implementation.
    pub fn new(source: impl MidiSource) -> Self {
        Self(Box::new(source))
    }
}

/// Initial playback settings, mirrors `bevy_audio::PlaybackSettings`.
///
/// `paused`, `speed` and `volume` are applied live when mutated; `mode` is
/// read when playback finishes (for its `Despawn`/`Remove` behavior).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct MidiPlaybackSettings {
    /// What happens when the sequence / file finishes.
    pub mode: MidiPlaybackMode,
    /// Per-player master volume in firewheel [`Volume`] units
    /// (default: [`Volume::UNITY_GAIN`], 0 dB). Applied live when mutated.
    pub volume: Volume,
    /// Playback speed multiplier (applied live).
    pub speed: f32,
    /// Pause playback (applied live).
    pub paused: bool,
    /// Start playback at this offset (seconds into the sequence).
    pub start_position: Option<f64>,
}

/// What happens when playback finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MidiPlaybackMode {
    /// Play once, then go silent (the entity keeps its components).
    #[default]
    Once,
    /// Loop until the player is removed or the entity is despawned.
    Loop,
    /// Play once, then despawn the entity.
    Despawn,
    /// Play once, then remove the player components from the entity.
    Remove,
}

impl Default for MidiPlaybackSettings {
    fn default() -> Self {
        Self::ONCE
    }
}

impl MidiPlaybackSettings {
    /// Play the source once.
    pub const ONCE: Self = Self {
        mode: MidiPlaybackMode::Once,
        volume: Volume::UNITY_GAIN,
        speed: 1.0,
        paused: false,
        start_position: None,
    };

    /// Loop the source.
    pub const LOOP: Self = Self {
        mode: MidiPlaybackMode::Loop,
        volume: Volume::UNITY_GAIN,
        speed: 1.0,
        paused: false,
        start_position: None,
    };

    /// Play once, then despawn the entity.
    pub const DESPAWN: Self = Self {
        mode: MidiPlaybackMode::Despawn,
        volume: Volume::UNITY_GAIN,
        speed: 1.0,
        paused: false,
        start_position: None,
    };

    /// Play once, then remove the player components.
    pub const REMOVE: Self = Self {
        mode: MidiPlaybackMode::Remove,
        volume: Volume::UNITY_GAIN,
        speed: 1.0,
        paused: false,
        start_position: None,
    };

    /// Start in a paused state.
    pub const fn paused(self) -> Self {
        Self {
            paused: true,
            ..self
        }
    }

    /// Set the playback speed multiplier.
    pub const fn with_speed(self, speed: f32) -> Self {
        Self { speed, ..self }
    }

    /// Set the master volume in firewheel [`Volume`] units (e.g.
    /// [`Volume::Linear`] for slider-style values, [`Volume::Decibels`] for
    /// dB). Values above unity amplify.
    pub const fn with_volume(self, volume: Volume) -> Self {
        Self { volume, ..self }
    }

    /// Set the master volume as a percentage, where `0.0` is silence and
    /// `100.0` is unity gain.
    pub const fn with_volume_percent(self, percent: f32) -> Self {
        Self {
            volume: Volume::from_percent(percent),
            ..self
        }
    }

    /// Start playback at an offset (seconds into the sequence).
    pub const fn with_start_position(self, seconds: f64) -> Self {
        Self {
            start_position: Some(seconds),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::MidiEventKind;

    fn timed(seconds: f64, key: u8) -> TimedMidiEvent {
        TimedMidiEvent {
            seconds,
            kind: MidiEventKind::NoteOn {
                channel: 0,
                key,
                velocity: 100,
            },
        }
    }

    #[test]
    fn sequence_source_resolves_to_arc_file() {
        let assets = Assets::<MidiFileAsset>::default();
        let source = SequenceSource(vec![timed(0.0, 60), timed(0.5, 62), timed(1.0, 64)]);
        let midi = source.resolve(&assets).expect("sequence resolves");
        assert_eq!(midi.get_times().len(), 3);
        assert!((midi.get_length() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn sequence_source_sorts_input() {
        let assets = Assets::<MidiFileAsset>::default();
        let source = SequenceSource(vec![timed(1.0, 60), timed(0.5, 62)]);
        let midi = source.resolve(&assets).expect("sorted internally");
        assert_eq!(midi.get_times()[0], 0.5);
        assert_eq!(midi.get_times()[1], 1.0);
    }

    #[test]
    fn file_source_waiting_for_asset() {
        let assets = Assets::<MidiFileAsset>::default();
        let source = FileSource(Handle::default());
        assert!(matches!(
            source.resolve(&assets),
            Err(MidiResolveError::AssetNotLoaded)
        ));
    }

    #[test]
    fn file_source_resolves_loaded_asset() {
        let mut assets = Assets::<MidiFileAsset>::default();
        let midi = Arc::new(
            MidiFile::new_with_events([(
                0.0,
                MidiEventKind::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 100,
                }
                .into_message(),
            )])
            .unwrap(),
        );
        let handle: Handle<MidiFileAsset> = assets.add(MidiFileAsset { midi: midi.clone() });
        let source = FileSource(handle);
        let resolved = source.resolve(&assets).expect("loaded asset resolves");
        assert!(Arc::ptr_eq(&resolved, &midi));
    }

    #[test]
    fn dyn_source_polymorphism() {
        // Both concrete sources are usable behind `Box<dyn MidiSource>`, which
        // is what the `MidiPlayer` component stores.
        let assets = Assets::<MidiFileAsset>::default();
        let players: Vec<Box<dyn MidiSource>> = vec![
            Box::new(SequenceSource(vec![timed(0.0, 60)])),
            Box::new(FileSource(Handle::default())),
        ];
        assert!(players[0].resolve(&assets).is_ok());
        assert!(matches!(
            players[1].resolve(&assets),
            Err(MidiResolveError::AssetNotLoaded)
        ));
    }
}
#[cfg(test)]
mod volume_tests {
    use super::*;
    use firewheel::Volume;

    #[test]
    fn defaults_are_unity_gain() {
        let s = MidiPlaybackSettings::default();
        assert_eq!(s.volume, Volume::UNITY_GAIN);
        assert_eq!(s.volume.amp(), 1.0);
        assert_eq!(MidiPlaybackSettings::LOOP.volume, Volume::UNITY_GAIN);
        assert_eq!(MidiPlaybackSettings::ONCE.volume, Volume::UNITY_GAIN);
    }

    #[test]
    fn percent_helper_maps_to_linear() {
        let s = MidiPlaybackSettings::default().with_volume_percent(50.0);
        assert_eq!(s.volume, Volume::Linear(0.5));
        let s = s.with_volume_percent(100.0);
        assert_eq!(s.volume, Volume::Linear(1.0));
    }

    #[test]
    fn decibel_amplification_headroom() {
        let s = MidiPlaybackSettings::default().with_volume(Volume::Decibels(6.0));
        // +6 dB ≈ amplitude 2.0 (scaled up from unity), as used for louder
        // playback.
        assert!(s.volume.amp() > 1.0);
    }

    #[test]
    fn settings_remain_copy_and_partial_eq() {
        let a = MidiPlaybackSettings::LOOP.with_volume(Volume::Linear(2.0));
        let b = a;
        assert_eq!(a, b);
    }
}
