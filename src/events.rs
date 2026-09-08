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
//! - **MIDI messages** (requirement 1) are [`TimedMidiEvent`] — triggered by
//!   users via `Commands::entity(..).trigger(..)` or the chainable
//!   [`MidiEntityCommandsExt`] helpers. `seconds == 0` (or negative) fires as
//!   soon as possible; positive values fire `seconds` later,
//!   sample-accurately on the audio clock.
//! - **Playback notifications** ([`MidiPlaybackFinished`],
//!   [`MidiPlaybackRestarted`]) are triggered by the engine *on the playing
//!   entity*; users observe them with `On<..>` observers.
//!
//! The only [`Message`] left is [`MidiStreamError`] — a stream-level
//! notification that belongs to no entity.

use bevy_ecs::entity::Entity;
use bevy_ecs::event::EntityEvent;
use bevy_ecs::message::Message;
use bevy_ecs::system::EntityCommands;

pub use crate::midi::MidiEventKind;

/// A MIDI message targeted at an entity, firing at a relative delay `seconds`
/// on the audio clock, sample-accurately (requirement 1).
///
/// Unlike the [`SequenceMidiEvent`](crate::midi::SequenceMidiEvent) item type
/// (whose `seconds` counts from the start of a played sequence), `seconds` here
/// is a **relative delay**: it is anchored to the audio clock at the moment the
/// event is observed. `seconds == 0` (or a negative value) fires as soon as
/// possible — use [`MidiEntityCommandsExt::trigger_midi_event`] as the
/// immediate shorthand. Scheduling does not create a `MidiPlayer` and is not
/// affected by [`MidiPlaybackSettings`](crate::play::MidiPlaybackSettings).
///
/// Notes:
/// * If the audio stream has not started yet, the delay is anchored to the
///   stream start (the message fires `seconds` after the stream starts).
/// * The target entity must have a loaded
///   [`MidiSoundFont`](crate::play::MidiSoundFont); otherwise the message is
///   dropped with a warning.
///
/// Trigger it on the target entity:
///
/// ```
/// # use bevy_ecs::prelude::*;
/// # use bevy_soundfont_synth::events::{TimedMidiEvent, MidiEventKind};
/// fn echo(mut commands: Commands, target: Entity) {
///     // A note-off half a second after the matching note-on was triggered:
///     commands.entity(target).trigger(|entity| {
///         TimedMidiEvent::new(entity, 0.5, MidiEventKind::NoteOff { channel: 0, key: 60 })
///     });
/// }
/// ```
#[derive(EntityEvent, Debug, Clone, Copy, PartialEq)]
pub struct TimedMidiEvent {
    /// The target entity: used to route the message to the right node.
    pub entity: Entity,
    /// Delay in seconds, relative to the audio clock when the event is
    /// observed. Negative values fire immediately.
    pub seconds: f64,
    /// The message itself.
    pub kind: MidiEventKind,
}

impl TimedMidiEvent {
    /// Convenience constructor for `EntityCommands::trigger`:
    /// `commands.entity(e).trigger(|e| TimedMidiEvent::new(e, 0.5, kind))`.
    pub fn new(entity: Entity, seconds: f64, kind: MidiEventKind) -> Self {
        Self {
            entity,
            seconds,
            kind,
        }
    }
}

/// Extension methods on [`EntityCommands`] for triggering MIDI events on a
/// synth entity (requirement 1), chainable with the regular `EntityCommands`
/// methods.
///
/// ```
/// # use bevy_ecs::prelude::*;
/// # use bevy_soundfont_synth::events::{MidiEntityCommandsExt, MidiEventKind};
/// fn flourish(mut commands: Commands, synth: Entity) {
///     commands
///         .entity(synth)
///         .trigger_midi_event(MidiEventKind::NoteOn { channel: 0, key: 60, velocity: 100 })
///         .trigger_timed_midi_event(0.5, MidiEventKind::NoteOff { channel: 0, key: 60 });
/// }
/// ```
pub trait MidiEntityCommandsExt {
    /// Trigger an immediate [`TimedMidiEvent`] on the entity (zero delay: the
    /// message fires as soon as possible).
    fn trigger_midi_event(&mut self, kind: MidiEventKind) -> &mut Self;
    /// Trigger a timed [`TimedMidiEvent`] on the entity: the message fires
    /// `seconds` later, sample-accurately on the audio clock.
    fn trigger_timed_midi_event(&mut self, seconds: f64, kind: MidiEventKind) -> &mut Self;
}

impl MidiEntityCommandsExt for EntityCommands<'_> {
    fn trigger_midi_event(&mut self, kind: MidiEventKind) -> &mut Self {
        self.trigger(|entity| TimedMidiEvent::new(entity, 0.0, kind))
    }

    fn trigger_timed_midi_event(&mut self, seconds: f64, kind: MidiEventKind) -> &mut Self {
        self.trigger(move |entity| TimedMidiEvent::new(entity, seconds, kind))
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
    pub fn new(entity: Entity, loop_count: u32) -> Self {
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
    pub fn new(entity: Entity, loop_count: u32) -> Self {
        Self { entity, loop_count }
    }
}

/// Emitted when the audio stream reports an error (e.g. device unplugged).
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct MidiStreamError(pub String);

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::observer::On;
    use bevy_ecs::prelude::*;

    #[derive(Resource, Default)]
    struct Counts {
        events: u32,
        seconds: Vec<f64>,
    }

    fn count_timed(event: On<TimedMidiEvent>, mut counts: ResMut<Counts>) {
        counts.events += 1;
        counts.seconds.push(event.seconds);
    }

    #[test]
    fn ext_trait_triggers_timed_events() {
        let mut world = World::new();
        world.insert_resource(Counts::default());
        world.add_observer(count_timed);
        let entity = world.spawn_empty().id();

        world
            .commands()
            .entity(entity)
            .trigger_midi_event(MidiEventKind::NoteOn {
                channel: 0,
                key: 60,
                velocity: 100,
            })
            .trigger_timed_midi_event(
                0.5,
                MidiEventKind::NoteOff {
                    channel: 0,
                    key: 60,
                },
            );
        world.flush();

        let counts = world.resource::<Counts>();
        assert_eq!(counts.events, 2);
        // The immediate shorthand is a zero-delay timed event.
        assert_eq!(counts.seconds, [0.0, 0.5]);
    }
}
