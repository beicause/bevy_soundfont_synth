//! Entity events and messages for the synth plugin.
//!
//! Bevy 0.19 splits the old "event" API into two:
//! - [`Event`](bevy_ecs::event::Event) + [`EntityEvent`]: observer-driven
//!   triggers (no queue).
//! - [`Message`]: queued data read by systems through `MessageWriter` /
//!   `MessageReader`.
//!
//! Following that split, everything targeted at a synth entity is an
//! [`EntityEvent`] triggered *on that entity*:
//! - **Immediate MIDI events** (requirement 1) are [`MidiEvent`] — triggered
//!   by users via `Commands::entity(..).trigger(..)`.
//! - **Playback notifications** ([`MidiPlaybackFinished`],
//!   [`MidiPlaybackRestarted`]) are triggered by the engine *on the playing
//!   entity*; users observe them with `On<..>` observers.
//!
//! The only [`Message`] left is [`MidiStreamError`] — a stream-level
//! notification that belongs to no entity.

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

/// Emitted by the engine on the playing entity each time looping playback
/// wraps back to the start of the sequence or MIDI file.
///
/// `loop_count` is the 1-based number of restarts so far: the first wrap fires
/// with `loop_count == 1`, the second with `2`, and so on.
///
/// Observe it to react to loops (e.g. print the loop count):
///
/// ```
/// # use bevy_ecs::prelude::*;
/// # use bevy_ecs::observer::On;
/// # use bevy_soundfont_synth::events::MidiPlaybackRestarted;
/// fn on_restart(event: On<MidiPlaybackRestarted>) {
///     println!("looped {} times", event.loop_count);
/// }
/// ```
#[derive(EntityEvent, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiPlaybackRestarted {
    /// The entity whose playback restarted.
    pub entity: Entity,
    /// How many times looping playback has restarted since it began.
    pub loop_count: u32,
}

impl MidiPlaybackRestarted {
    /// Convenience constructor for `EntityCommands::trigger`.
    pub fn on(entity: Entity, loop_count: u32) -> Self {
        Self { entity, loop_count }
    }
}

/// Emitted by the engine on the playing entity when a sequence or MIDI file
/// finishes playing (non-looping playback only).
///
/// `loop_count` is the total number of loop restarts that occurred before the
/// playback ended (0 for plain non-looping playback).
///
/// Observe it with an `On<MidiPlaybackFinished>` observer like
/// [`MidiPlaybackRestarted`]; `MidiPlaybackMode::Despawn` / `Remove` already
/// handle the entity life cycle, so observers are optional.
#[derive(EntityEvent, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiPlaybackFinished {
    /// The entity whose playback finished.
    pub entity: Entity,
    /// Total loop restarts before the playback finished.
    pub loop_count: u32,
}

impl MidiPlaybackFinished {
    /// Convenience constructor for `EntityCommands::trigger`.
    pub fn on(entity: Entity, loop_count: u32) -> Self {
        Self { entity, loop_count }
    }
}

/// Emitted when the audio stream reports an error (e.g. device unplugged).
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct MidiStreamError(pub String);
