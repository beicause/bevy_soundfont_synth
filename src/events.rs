//! Entity events and messages for the synth plugin.
//!
//! Bevy 0.19 splits the old "event" API into two:
//! - [`Event`] + [`EntityEvent`]: observer-driven triggers (no queue).
//! - [`Message`]: queued data read by systems through `MessageWriter` /
//!   `MessageReader`.
//!
//! Following that split:
//! - **Immediate MIDI events** (requirement 1) are [`MidiEvent`] — an
//!   [`EntityEvent`] triggered *on the target entity* via
//!   `Commands::entity(..).trigger(..)`. The plugin registers an observer that
//!   forwards them to the target entity's synthesizer node.
//! - **Playback notifications** ([`MidiPlaybackEnded`], [`MidiStreamError`])
//!   are [`Message`]s queued with `MessageWriter` and read by user systems.

use bevy_ecs::entity::Entity;
use bevy_ecs::event::EntityEvent;
use bevy_ecs::message::Message;

pub use crate::midi::MidiEventKind;

/// An immediate MIDI message targeted at an entity (requirement 1).
///
/// The target entity must have a [`MidiSoundFont`](crate::play::MidiSoundFont)
/// component (the engine then routes the message to that entity's synthesizer
/// node).
///
/// Trigger it on the target entity:
///
/// ```
/// # use bevy_ecs::prelude::*;
/// # use bevy_soundfont_synth::events::{MidiEvent, MidiEventKind};
/// fn play(mut commands: Commands, target: Entity) {
///     commands.entity(target).trigger(|entity| {
///         MidiEvent {
///             entity,
///             kind: MidiEventKind::NoteOn { channel: 0, key: 60, velocity: 100 },
///         }
///     });
/// }
/// ```
#[derive(EntityEvent, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiEvent {
    /// The target entity: used to route the message to the right node.
    pub entity: Entity,
    /// The message itself.
    pub kind: MidiEventKind,
}

impl MidiEvent {
    /// Convenience constructor usable with `EntityCommands::trigger`:
    /// `commands.entity(e).trigger(|e| MidiEvent::on(e, kind))`.
    pub fn on(entity: Entity, kind: MidiEventKind) -> Self {
        Self { entity, kind }
    }
}

/// Emitted once by the engine when a sequence or MIDI file finishes playing
/// (non-looping playback only).
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiPlaybackEnded(pub Entity);

/// Emitted when the audio stream reports an error (e.g. device unplugged).
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct MidiStreamError(pub String);