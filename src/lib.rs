//! A Bevy plugin that loads SoundFont (SF2/SF3) and MIDI files as assets and
//! plays them through a [firewheel](https://crates.io/crates/firewheel) audio
//! graph backed by [rustysynth](https://crates.io/crates/rustysynth-ext) (the
//! local fork in `rustysynth/`).
//!
//! Three ways to produce audio:
//!
//! 1. **MIDI events** — a [`TimedMidiEvent`]
//!    [`EntityEvent`](bevy_ecs::event::EntityEvent) triggered *on* a synth
//!    entity (an entity with a [`MidiSoundFont`]); `seconds` is a relative
//!    delay from the audio clock at trigger time (0 = immediate),
//!    sample-accurate:
//!    ```ignore
//!    commands.entity(synth).trigger(|e| {
//!        TimedMidiEvent::new(e, 0.0, MidiEventKind::NoteOn { channel: 0, key: 60, velocity: 100 })
//!    });
//!    ```
//!    or chainably via [`MidiEntityCommandsExt`]:
//!    ```ignore
//!    commands
//!        .entity(synth)
//!        .trigger_midi_event(MidiEventKind::NoteOn { channel: 0, key: 60, velocity: 100 })
//!        .trigger_timed_midi_event(0.5, MidiEventKind::NoteOff { channel: 0, key: 60 });
//!    ```
//! 2. **A series of MIDI events** — [`MidiPlayer(MidiSource::sequence(..))`](play::MidiPlayer)
//!    with timed [`midi::SequenceMidiEvent`]s (`seconds` from the start of the
//!    sequence).
//! 3. **A MIDI file** — [`MidiPlayer(MidiSource::file(handle))`](play::MidiPlayer).
//!
//! Requirements 2 and 3 are unified: both resolve to a shared `Arc<MidiFile>`
//! and are then polled on the main thread each frame, with every due MIDI
//! message scheduled sample-accurately through firewheel's `scheduled_events`
//! API. The synthesizer nodes are firewheel nodes behind the
//! [`MidiSynthNode`] *component* (one per entity, per its own SoundFont — the
//! font is not global), and the audio context itself lives in a `NonSend`
//! resource ([`MidiSynthEngine`]).
//!
//! # Example
//!
//! ```ignore
//! use bevy::prelude::*;
//! use bevy_soundfont_synth::{MidiPlayer, MidiSoundFont, SoundFontSynthPlugin};
//!
//! App::new()
//!     .add_plugins((DefaultPlugins, SoundFontSynthPlugin))
//!     .add_systems(Startup, setup)
//!     .run();
//! ```

pub mod assets;
pub mod engine;
pub mod events;
pub mod midi;
pub mod node;
pub mod play;
pub mod playback;

pub use assets::{MidiFileAsset, MidiFileLoader, SoundFontAsset, SoundFontLoader};
pub use engine::{MidiError, MidiSynthEngine, SoundFontSynthPlugin};
pub use events::{
    MidiEntityCommandsExt, MidiPlaybackFinished, MidiPlaybackRestarted, MidiStreamError,
    TimedMidiEvent,
};
pub use midi::{MidiEventKind, SequenceMidiEvent};
pub use node::MidiSynthNode;
pub use play::{
    FileSource, MidiPlaybackMode, MidiPlaybackSettings, MidiPlayer, MidiResolveError,
    MidiSoundFont, MidiSource, SequenceSource,
};
// Convenient passthrough of the local rustysynth fork's public API.
pub use rustysynth_ext::{MidiFile, MidiFileError, MidiMessage, SoundFont, SoundFontError};
// Firewheel re-exports used by the public API.
pub use firewheel::{FirewheelConfig, Volume};
