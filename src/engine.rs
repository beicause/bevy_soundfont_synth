//! Plugin, per-entity synthesizer nodes and playback orchestration.
//!
//! Core ideas:
//! * The firewheel node is a *component* ([`MidiSynthNode`]): the engine builds
//!   one per entity that has a [`MidiSoundFont`], and a component hook tears it
//!   down when the font is replaced or the entity despawns.
//! * The SoundFont is *per entity* — nothing here is global.
//! * Sequences and MIDI files are unified behind
//!   [`MidiSource`](crate::play::MidiSource) → `Arc<MidiFile>`.
//!   Playback is polled every frame on the main thread and each due MIDI
//!   message is *scheduled* individually through firewheel's `scheduled_events`
//!   API, so notes land sample-accurately on the audio thread.
//! * The audio context ([`FirewheelContext`] + [`CpalStream`]) is a `NonSend`
//!   resource.

use std::sync::Arc;

use bevy_app::{App, Last, Plugin, Update};
use bevy_asset::{AssetApp, Assets};
use bevy_ecs::message::MessageWriter;
use bevy_ecs::observer::On;
use bevy_ecs::prelude::*;
use bevy_log::{error, info, warn};
use firewheel::{
    FirewheelConfig, FirewheelContext,
    clock::EventInstant,
    cpal::{CpalConfig, CpalStream},
    event::NodeEventType,
    node::NodeID,
};
use rustysynth_ext::{MidiMessage, SoundFont};

use crate::assets::{MidiFileAsset, MidiFileLoader, SoundFontAsset, SoundFontLoader};
use crate::events::{MidiPlaybackFinished, MidiPlaybackRestarted, MidiStreamError, TimedMidiEvent};
use crate::node::{MidiSynthNode, SynthMsg, SynthNode, at_seconds, register_node_cleanup_hook};
use crate::play::{
    MidiPlaybackMode, MidiPlaybackSettings, MidiPlayer, MidiResolveError, MidiSoundFont,
};
use crate::playback::MidiPlaybackState;

/// Errors surfaced by the engine. Most end-user operations are
/// component-driven, so failures are logged rather than returned; this type is
/// used for engine setup and API helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiError {
    /// No audio stream could be started (no output device, etc.).
    StreamNotStarted,
    /// The audio stream failed to start.
    FailedToStartStream(String),
    /// A synthesizer node could not be built.
    NodeBuild(String),
}

impl std::fmt::Display for MidiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MidiError::StreamNotStarted => f.write_str("audio stream is not started"),
            MidiError::FailedToStartStream(msg) => write!(f, "failed to start audio stream: {msg}"),
            MidiError::NodeBuild(msg) => write!(f, "failed to build synthesizer node: {msg}"),
        }
    }
}

impl std::error::Error for MidiError {}

// --- Engine (NonSend audio context) ----------------------------------------

/// The realtime audio context, stored as a `NonSend` resource.
///
/// Holds the firewheel graph and the CPAL stream. Order of fields matters for
/// `Drop`: the stream is dropped *before* the context.
pub struct MidiSynthEngine {
    /// CPAL output stream. Dropped first on engine teardown.
    stream: Option<CpalStream>,
    /// The firewheel audio graph.
    cx: Option<FirewheelContext>,
    /// Firewheel graph configuration (user-configurable via
    /// [`SoundFontSynthPlugin::with_config`]).
    config: FirewheelConfig,
    /// Maximum polyphony per synthesizer node.
    polyphony: usize,
    /// Commands to flush to the audio thread on the next tick.
    pending: Vec<(NodeID, Option<EventInstant>, SynthMsg)>,
    /// MIDI events buffered by the observer. The second element is the
    /// absolute audio-clock instant to fire at.
    live: Vec<(Entity, f64, MidiMessage)>,
    /// Set once the stream fails to start (or later reports an error).
    error: Option<String>,
    /// Whether the stream failure has already been logged.
    error_logged: bool,
}

impl Default for MidiSynthEngine {
    fn default() -> Self {
        Self::with_config(FirewheelConfig::default())
    }
}

impl MidiSynthEngine {
    /// Create the engine with a custom firewheel graph configuration.
    pub fn with_config(config: FirewheelConfig) -> Self {
        Self {
            stream: None,
            cx: None,
            config,
            polyphony: 64,
            pending: Vec::new(),
            live: Vec::new(),
            error: None,
            error_logged: false,
        }
    }

    /// Set the maximum polyphony used by subsequently created nodes.
    pub fn set_polyphony(&mut self, polyphony: usize) {
        self.polyphony = polyphony.clamp(8, 256);
    }

    /// The last stream error, if any.
    pub fn last_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Lazily create the firewheel context and start the CPAL stream.
    pub(crate) fn ensure_started(&mut self) -> Result<(), MidiError> {
        if self.cx.is_some() {
            return Ok(());
        }
        if let Some(err) = &self.error {
            if !self.error_logged {
                error!("bevy_soundfont_synth: {err}");
                self.error_logged = true;
            }
            return Err(MidiError::FailedToStartStream(err.clone()));
        }

        let mut cx = FirewheelContext::new(self.config);
        match CpalStream::new(&mut cx, CpalConfig::default()) {
            Ok(stream) => {
                info!("bevy_soundfont_synth: audio stream started");
                self.stream = Some(stream);
                self.cx = Some(cx);
                Ok(())
            }
            Err(e) => {
                let msg = format!("failed to start audio stream: {e}");
                self.error = Some(msg.clone());
                error!("bevy_soundfont_synth: {msg}");
                Err(MidiError::FailedToStartStream(msg))
            }
        }
    }

    /// Build a synthesizer node for the given font and connect it to the
    /// master output.
    pub(crate) fn start_node(&mut self, font: &Arc<SoundFont>) -> Result<NodeID, MidiError> {
        self.ensure_started()?;
        let cx = self.cx.as_mut().ok_or(MidiError::StreamNotStarted)?;
        let id = cx
            .add_node(
                SynthNode {
                    font: Arc::clone(font),
                    polyphony: self.polyphony,
                },
                None,
            )
            .map_err(|e| MidiError::NodeBuild(e.to_string()))?;
        if let Err(e) = cx.connect_stereo(id, cx.graph_out_node_id(), false) {
            let _ = cx.remove_node(id);
            return Err(MidiError::NodeBuild(e.to_string()));
        }
        Ok(id)
    }

    /// Remove a synthesizer node from the graph (idempotent).
    pub(crate) fn remove_node(&mut self, id: NodeID) {
        let Some(cx) = &mut self.cx else {
            return;
        };
        if cx.contains_node(id) {
            let _ = cx.remove_node(id).inspect_err(|e| {
                warn!("bevy_soundfont_synth: failed to remove node: {e}");
            });
        }
    }

    /// Queue a message for a node, either immediately or at an absolute
    /// audio-clock instant.
    pub(crate) fn enqueue(&mut self, node: NodeID, time: Option<EventInstant>, msg: SynthMsg) {
        self.pending.push((node, time, msg));
    }

    /// Whether the audio stream is running.
    pub fn is_started(&self) -> bool {
        self.cx.is_some()
    }

    /// Read access to the live firewheel graph (after the stream has started).
    ///
    /// Use this to add gain/effect nodes and reroute edges, e.g. to insert a
    /// [`firewheel::nodes::volume::VolumeNode`] between a `MidiSynthNode`'s
    /// output and the graph master.
    pub fn context(&self) -> Option<&FirewheelContext> {
        self.cx.as_ref()
    }

    /// Mutable access to the live firewheel graph (after the stream has
    /// started). Engine bookkeeping is unaffected by user graph edits.
    pub fn context_mut(&mut self) -> Option<&mut FirewheelContext> {
        self.cx.as_mut()
    }

    /// The current audio clock reading in seconds, or `None` if the stream is
    /// not running.
    pub fn audio_clock_seconds(&self) -> Option<f64> {
        self.cx.as_ref().map(|cx| cx.audio_clock().seconds.0)
    }

    /// Flush all queued commands to the audio thread and pump firewheel's
    /// message channel.
    pub(crate) fn tick(&mut self) {
        let Some(cx) = self.cx.as_mut() else {
            self.pending.clear();
            return;
        };
        for (node, time, msg) in self.pending.drain(..) {
            cx.schedule_event_for(node, NodeEventType::custom(msg), time);
        }
        if let Err(e) = cx.update() {
            warn!("bevy_soundfont_synth: firewheel update error: {e}");
        }
    }

    /// Collect any async stream errors reported since the last call.
    pub(crate) fn poll_stream_error(&mut self) -> Option<String> {
        let stream = self.stream.as_mut()?;
        stream
            .poll_status()
            .next()
            .map(|err| format!("audio stream error: {err}"))
    }
}

// --- Plugin -----------------------------------------------------------------

/// Bevy plugin for MIDI/SoundFont playback through firewheel.
///
/// Registers the asset loaders, the `NonSend` [`MidiSynthEngine`], the
/// observer for [`TimedMidiEvent`]s, and the playback systems.
///
/// The firewheel graph configuration is user-configurable:
///
/// ```
/// # use bevy_soundfont_synth::{SoundFontSynthPlugin, FirewheelConfig};
/// let mut config = FirewheelConfig::default();
/// config.scheduled_event_capacity = 4096;
/// let plugin = SoundFontSynthPlugin::with_config(config);
/// ```
#[derive(Default)]
pub struct SoundFontSynthPlugin {
    /// Firewheel graph configuration passed to the engine.
    pub config: FirewheelConfig,
}

impl SoundFontSynthPlugin {
    /// Create the plugin with a custom firewheel graph configuration
    /// (defaults are suited for a stereo synthesizer output).
    pub fn with_config(config: FirewheelConfig) -> Self {
        Self { config }
    }
}

impl Plugin for SoundFontSynthPlugin {
    fn build(&self, app: &mut App) {
        // Assets.
        app.init_asset::<SoundFontAsset>()
            .init_asset::<MidiFileAsset>()
            .init_asset_loader::<SoundFontLoader>()
            .init_asset_loader::<MidiFileLoader>();

        // Playback notifications are entity events triggered on the playing
        // entity (no registration needed); only the stream-level error is a
        // Message.
        app.add_message::<MidiStreamError>();

        // NonSend audio context.
        app.insert_non_send(MidiSynthEngine::with_config(self.config));

        // MIDI events (requirement 1): route via an observer.
        app.add_observer(timed_midi_event_observer);

        // Node teardown hook.
        register_node_cleanup_hook(app);

        // Playback systems.
        app.add_systems(
            Update,
            (
                synth_node_sync,
                player_sources_sync,
                live_events,
                poll_playbacks,
                cleanup_stale_states,
            )
                .chain(),
        );
        app.add_systems(Last, engine_tick);
    }
}

// --- Observer (requirement 1: MIDI events) -----------------------------------

/// Forward every triggered [`TimedMidiEvent`] into the engine's live buffer,
/// anchored to the current audio clock: the message fires `seconds` later
/// (`seconds == 0` as soon as possible), sample-accurately. When the stream
/// has not started yet the delay is anchored to the stream start (audio clock
/// zero).
fn timed_midi_event_observer(event: On<TimedMidiEvent>, mut engine: NonSendMut<MidiSynthEngine>) {
    let now = engine.audio_clock_seconds().unwrap_or(0.0);
    let at = now + event.seconds.max(0.0);
    engine
        .live
        .push((event.entity, at, event.kind.into_message()));
}

fn live_events(mut engine: NonSendMut<MidiSynthEngine>, nodes: Query<&MidiSynthNode>) {
    let live = std::mem::take(&mut engine.live);
    for (entity, at, message) in live {
        match nodes.get(entity) {
            Ok(node) => engine.enqueue(node.0, Some(at_seconds(at)), SynthMsg::Midi(message)),
            Err(_) => warn!(
                "bevy_soundfont_synth: MIDI event targeted an entity without a loaded MidiSoundFont; event dropped"
            ),
        }
    }
}

// --- Systems: synthesizer nodes per MidiSoundFont ---------------------------

/// Synthesizer-node bookkeeping query: entities that need a node built
/// (added font, changed font, or node missing entirely).
type SynthNodes<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static MidiSoundFont,
        Option<&'static MidiSynthNode>,
    ),
    Or<(
        Added<MidiSoundFont>,
        Changed<MidiSoundFont>,
        Without<MidiSynthNode>,
    )>,
>;

/// Build / rebuild / retry synthesizer nodes for entities with
/// [`MidiSoundFont`]. The loop covers:
/// * `Added<MidiSoundFont>` — build the node (once the asset is loaded).
/// * `Changed<MidiSoundFont>` — replace the node (the `on_discard` hook drops
///   the old firewheel node when the `MidiSynthNode` component is replaced).
/// * entities still waiting for their font asset to load — retried every
///   frame.
fn synth_node_sync(
    mut commands: Commands,
    fonts: Res<Assets<SoundFontAsset>>,
    nodes: SynthNodes,
    mut engine: NonSendMut<MidiSynthEngine>,
) {
    for (entity, font, node) in &nodes {
        match fonts.get(&font.0) {
            None => {
                // New handle not loaded yet: if the entity still has an old
                // node, drop it so it is rebuilt once the font arrives.
                if node.is_some() {
                    commands.entity(entity).remove::<MidiSynthNode>();
                }
            }
            Some(asset) => match engine.start_node(&asset.font) {
                Ok(id) => {
                    commands.entity(entity).insert(MidiSynthNode(id));
                }
                Err(err) => warn!("bevy_soundfont_synth: failed to create synthesizer node: {err}"),
            },
        }
    }
}

// --- Systems: playback sources (requirements 2 and 3) -----------------------

/// Start / restart playback when a [`MidiPlayer`] is added or changed, and
/// keep retrying entities whose source could not be resolved yet (e.g. a file
/// asset that is still loading).
///
/// Every [`MidiSource`](crate::play::MidiSource) implementation (built-in
/// [`FileSource`](crate::play::FileSource),
/// [`SequenceSource`](crate::play::SequenceSource), or user-defined) resolves
/// to the same `Arc<MidiFile>`
/// and then uses the exact same playback state machine.
type Players<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        Ref<'static, MidiPlayer>,
        &'static MidiPlaybackSettings,
        Option<&'static MidiSynthNode>,
        Has<MidiPlaybackState>,
    ),
    Or<(Changed<MidiPlayer>, Without<MidiPlaybackState>)>,
>;

fn player_sources_sync(
    mut commands: Commands,
    players: Players,
    midis: Res<Assets<MidiFileAsset>>,
    mut engine: NonSendMut<MidiSynthEngine>,
) {
    let now = engine.audio_clock_seconds().unwrap_or(0.0);

    for (entity, player, settings, node, has_state) in &players {
        match player.0.resolve(&midis) {
            Ok(midi) => {
                commands
                    .entity(entity)
                    .insert(MidiPlaybackState::new(midi, settings, now));
                // Restarting over a previous playback: clear lingering voices.
                if has_state && let Some(node) = node {
                    engine.enqueue(node.0, None, SynthMsg::Reset);
                }
            }
            Err(MidiResolveError::AssetNotLoaded) => {
                if player.is_changed() && has_state {
                    // The source was just replaced but its asset is still
                    // loading: drop the *stale* playback state so the
                    // `Without<MidiPlaybackState>` retry path recreates it
                    // (with the new source) once the asset arrives. Without
                    // this, a completed old state would stay forever and the
                    // new source would never play.
                    commands.entity(entity).remove::<MidiPlaybackState>();
                }
            }
            Err(MidiResolveError::InvalidSequence(err)) => {
                warn!(
                    "bevy_soundfont_synth: MidiPlayer sequence rejected (times must be non-decreasing): {err}"
                );
            }
        }
    }
}

/// Remove stale playback state from entities that no longer have a
/// [`MidiPlayer`] (e.g. after `MidiPlaybackMode::Remove` or despawn). The
/// synthesizer node itself is owned by [`MidiSoundFont`] and stays alive.
fn cleanup_stale_states(
    mut commands: Commands,
    stale: Query<Entity, (With<MidiPlaybackState>, Without<MidiPlayer>)>,
) {
    for entity in &stale {
        commands.entity(entity).remove::<MidiPlaybackState>();
    }
}

/// Poll every active playback against the audio clock and schedule the due
/// MIDI events individually through firewheel's `scheduled_events` API. Loop
/// wraps trigger [`MidiPlaybackRestarted`] on the entity; completion triggers
/// [`MidiPlaybackFinished`].
fn poll_playbacks(
    mut commands: Commands,
    mut playbacks: Query<(
        Entity,
        &MidiSynthNode,
        &MidiPlaybackSettings,
        &mut MidiPlaybackState,
    )>,
    mut engine: NonSendMut<MidiSynthEngine>,
) {
    let Some(now) = engine.audio_clock_seconds() else {
        return;
    };

    for (entity, node, settings, mut state) in &mut playbacks {
        let state = &mut *state;

        // Live settings (paused / speed) are folded into the epoch.
        state.state.set_paused(settings.paused, now);
        state.state.set_speed(settings.speed as f64, now);

        // Volume changes are pushed to the audio thread immediately.
        if settings.volume != state.last_volume {
            engine.enqueue(node.0, None, SynthMsg::SetMasterVolume(settings.volume));
            state.last_volume = settings.volume;
        }

        // Schedule every event that has become due since the last poll.
        let result = state.state.poll(now);
        for due in result.due {
            engine.enqueue(
                node.0,
                Some(at_seconds(due.audio_instant_seconds)),
                SynthMsg::Midi(due.message),
            );
        }

        // Fire one restart event per loop wrap that happened this poll.
        let first_count = result.loop_count - result.restarts;
        for i in 0..result.restarts {
            let count = first_count + i + 1;
            commands
                .entity(entity)
                .trigger(|e| MidiPlaybackRestarted::new(e, count));
        }

        if state.state.is_done() && !state.ended_notified {
            state.ended_notified = true;
            commands
                .entity(entity)
                .trigger(|e| MidiPlaybackFinished::new(e, state.state.loop_count()));
            match settings.mode {
                MidiPlaybackMode::Once | MidiPlaybackMode::Loop => {}
                MidiPlaybackMode::Despawn => {
                    commands.entity(entity).despawn();
                }
                MidiPlaybackMode::Remove => {
                    commands
                        .entity(entity)
                        .remove::<(MidiPlayer, MidiPlaybackSettings)>();
                }
            }
        }
    }
}

/// Last-schedule flush: send pending commands to the audio thread and collect
/// stream errors.
fn engine_tick(
    mut engine: NonSendMut<MidiSynthEngine>,
    mut stream_errors: MessageWriter<MidiStreamError>,
) {
    engine.tick();
    if let Some(err) = engine.poll_stream_error() {
        stream_errors.write(MidiStreamError(err));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::TimedMidiEvent;
    use crate::midi::MidiEventKind;
    use bevy_asset::Handle;
    use firewheel::clock::{EventInstant, InstantSeconds};

    fn note_on(key: u8) -> MidiEventKind {
        MidiEventKind::NoteOn {
            channel: 0,
            key,
            velocity: 100,
        }
    }

    /// Test world with the engine (no audio stream: the audio clock reads 0)
    /// and the requirement-1 observer installed.
    fn test_world() -> World {
        let mut world = World::new();
        world.insert_non_send(MidiSynthEngine::default());
        world.add_observer(timed_midi_event_observer);
        world
    }

    /// Run `live_events` once against the world.
    fn run_live_events(world: &mut World) {
        let mut schedule = Schedule::default();
        schedule.add_systems(live_events);
        schedule.run(world);
    }

    fn engine(world: &World) -> &MidiSynthEngine {
        world.non_send::<MidiSynthEngine>()
    }

    #[test]
    fn timed_event_anchors_delay_to_audio_clock() {
        let mut world = test_world();
        let entity = world.spawn_empty().id();

        // No stream yet, so the anchor is the audio clock zero.
        world
            .commands()
            .entity(entity)
            .trigger(|entity| TimedMidiEvent::new(entity, 1.5, note_on(60)));
        world.flush();

        assert_eq!(engine(&world).live.len(), 1);
        assert_eq!(engine(&world).live[0].0, entity);
        assert_eq!(engine(&world).live[0].1, 1.5);
    }

    #[test]
    fn immediate_event_is_anchored_to_now() {
        let mut world = test_world();
        let entity = world.spawn_empty().id();

        world
            .commands()
            .entity(entity)
            .trigger(|entity| TimedMidiEvent::new(entity, 0.0, note_on(60)));
        world.flush();

        // Zero delay: due at the current audio clock (0.0 without a stream).
        assert_eq!(engine(&world).live[0].1, 0.0);
    }

    #[test]
    fn timed_event_with_negative_delay_fires_immediately() {
        let mut world = test_world();
        let entity = world.spawn_empty().id();

        world
            .commands()
            .entity(entity)
            .trigger(|entity| TimedMidiEvent::new(entity, -3.0, note_on(60)));
        world.flush();

        // `now` (0.0) + clamped delay (0.0): due instantly, not in the past.
        assert_eq!(engine(&world).live[0].1, 0.0);
    }

    #[test]
    fn live_events_routes_with_scheduled_instant() {
        let mut world = test_world();
        let entity = world
            .spawn((
                MidiSoundFont(Handle::default()),
                MidiSynthNode(NodeID::DANGLING),
            ))
            .id();

        world
            .commands()
            .entity(entity)
            .trigger(|entity| TimedMidiEvent::new(entity, 1.5, note_on(60)));
        world.flush();
        run_live_events(&mut world);

        let engine = engine(&world);
        assert!(engine.live.is_empty(), "buffer drained");
        assert_eq!(engine.pending.len(), 1);
        let (id, time, msg) = &engine.pending[0];
        assert_eq!(*id, NodeID::DANGLING);
        assert_eq!(
            *time,
            Some(EventInstant::AtClockSeconds(InstantSeconds(1.5)))
        );
        assert!(matches!(msg, SynthMsg::Midi(_)));
    }

    #[test]
    fn live_events_drops_events_without_a_font() {
        let mut world = test_world();
        let entity = world.spawn_empty().id();

        world
            .commands()
            .entity(entity)
            .trigger(|entity| TimedMidiEvent::new(entity, 0.0, note_on(60)));
        world.flush();
        run_live_events(&mut world);

        let engine = engine(&world);
        assert!(engine.live.is_empty());
        assert!(engine.pending.is_empty());
    }
}
