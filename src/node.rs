//! The firewheel audio node and its ECS component.
//!
//! * [`SynthNode`] — the firewheel `AudioNode` constructor. Deliberately dumb:
//!   all scheduling happens on the main thread (see `crate::playback`), MIDI
//!   messages arrive as firewheel custom events (immediate or scheduled) and
//!   this node only applies them and renders the block.
//! * [`MidiSynthNode`] — the ECS *component* that stores the live firewheel
//!   node id on an entity. It is inserted by the engine when an entity gets a
//!   [`MidiSoundFont`](crate::play::MidiSoundFont), removed automatically when
//!   the font is replaced or the entity despawns (via a component hook), and
//!   used as the routing target for immediate [`MidiEvent`](crate::events::MidiEvent)s.

use std::sync::Arc;

use bevy_ecs::lifecycle::HookContext;
use bevy_ecs::prelude::Component;
use bevy_ecs::world::DeferredWorld;
use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    clock::{EventInstant, EventInstant::AtClockSeconds, InstantSeconds},
    event::ProcEvents,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError, NodeID, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use rustysynth_ext::{MidiMessage, SoundFont, Synthesizer, SynthesizerSettings};

use crate::engine::MidiSynthEngine;

/// A command payload sent over firewheel's custom-event channel
/// (`NodeEventType::custom`).
#[derive(Debug, Clone, Copy)]
pub(crate) enum SynthMsg {
    /// Apply a MIDI channel message to the synthesizer.
    Midi(MidiMessage),
    /// Reset the synthesizer (all notes, controllers, effects).
    Reset,
    /// Set master volume in firewheel [`Volume`] units (0 dB = unity).
    SetMasterVolume(firewheel::Volume),
}

/// The firewheel `AudioNode` constructor, living on the main thread.
pub(crate) struct SynthNode {
    pub font: Arc<SoundFont>,
    pub polyphony: usize,
}

impl AudioNode for SynthNode {
    type Configuration = EmptyConfig;

    fn info(&self, _configuration: &Self::Configuration) -> Result<AudioNodeInfo, NodeError> {
        Ok(AudioNodeInfo::new()
            .debug_name("bevy_soundfont_synth")
            .channel_config(ChannelConfig::new(ChannelCount::ZERO, ChannelCount::STEREO)))
    }

    fn construct_processor(
        &self,
        _configuration: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> Result<impl AudioNodeProcessor, NodeError> {
        let font = Arc::clone(&self.font);
        let settings = synthesizer_settings(cx.stream_info.sample_rate.get(), self.polyphony);
        let synth = Synthesizer::new(&font, &settings)
            .map_err(|e| NodeError::from_boxed(Box::new(e)))?;
        Ok(MidiSynthProcessor {
            font,
            polyphony: self.polyphony,
            synth,
            volume: firewheel::Volume::UNITY_GAIN,
        })
    }
}

fn synthesizer_settings(sample_rate: u32, polyphony: usize) -> SynthesizerSettings {
    let mut settings = SynthesizerSettings::new(sample_rate as i32);
    settings.block_size = 64;
    settings.maximum_polyphony = polyphony.clamp(8, 256);
    settings.enable_reverb_and_chorus = true;
    settings
}

/// The realtime processor, running on the audio thread.
struct MidiSynthProcessor {
    font: Arc<SoundFont>,
    polyphony: usize,
    synth: Synthesizer,
    volume: firewheel::Volume,
}

impl AudioNodeProcessor for MidiSynthProcessor {
    fn events(&mut self, _info: &ProcInfo, events: &mut ProcEvents, _extra: &mut ProcExtra) {
        for event in events.drain() {
            if let Some(msg) = event.downcast_ref::<SynthMsg>().copied() {
                self.apply(msg);
            }
        }
    }

    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let frames = info.frames;
        let (left, right) = buffers.outputs.split_at_mut(1);
        self.synth
            .render(&mut left[0][..frames], &mut right[0][..frames]);
        ProcessStatus::OutputsModified
    }

    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        // A stream restart (e.g. device or sample-rate change) needs a
        // synthesizer at the new sample rate. This stops current voices.
        let new_rate = stream_info.sample_rate.get();
        if new_rate as i32 != self.synth.get_sample_rate() {
            let settings = synthesizer_settings(new_rate, self.polyphony);
            match Synthesizer::new(&self.font, &settings) {
                Ok(synth) => {
                    self.synth = synth;
                    self.synth.set_master_volume(self.volume.amp());
                }
                Err(e) => {
                    bevy_log::error!(
                        "bevy_soundfont_synth: failed to rebuild synthesizer after stream restart: {e}"
                    );
                }
            }
        }
    }
}

impl MidiSynthProcessor {
    fn apply(&mut self, msg: SynthMsg) {
        match msg {
            SynthMsg::Midi(message) => dispatch_message(&mut self.synth, message),
            SynthMsg::Reset => self.synth.reset(),
            SynthMsg::SetMasterVolume(volume) => {
                self.volume = volume;
                // `Volume::amp()` allows values above unity for amplification.
                self.synth.set_master_volume(volume.amp());
            }
        }
    }
}

/// Route a `MidiMessage::Normal` into `Synthesizer::process_midi_message`
/// (command without the channel nibble; the fork's messages carry the channel
/// in the low nibble of `status`).
pub(crate) fn dispatch_message(synth: &mut Synthesizer, message: MidiMessage) {
    if let MidiMessage::Normal { status, data1, data2 } = message {
        synth.process_midi_message(
            (status & 0x0F) as i32,
            (status & 0xF0) as i32,
            data1 as i32,
            data2 as i32,
        );
    }
}

/// An absolute audio-clock instant in seconds.
pub(crate) fn at_seconds(seconds: f64) -> EventInstant {
    AtClockSeconds(InstantSeconds(seconds))
}

/// ECS component holding the firewheel node id of an entity's synthesizer.
///
/// Engine-managed: inserted when the entity has a loaded
/// [`MidiSoundFont`](crate::play::MidiSoundFont), replaced when the font is
/// changed (the old firewheel node is torn down by a component hook), and
/// removed/cleaned up when the entity despawns.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiSynthNode(pub NodeID);

/// `on_discard` hook: the firewheel node is removed from the graph before the
/// component value actually disappears. Runs on font replacement and on entity
/// despawn.
pub(crate) fn register_node_cleanup_hook(app: &mut bevy_app::App) {
    app.world_mut()
        .register_component_hooks::<MidiSynthNode>()
        .on_discard(node_discard_hook);
}

fn node_discard_hook(mut world: DeferredWorld, ctx: HookContext) {
    // on_discard runs while the component data is still present.
    let node_id = world
        .get_entity_mut(ctx.entity)
        .ok()
        .and_then(|cell| cell.get::<MidiSynthNode>().map(|node| node.0));
    let Some(node_id) = node_id else {
        return;
    };
    if let Some(mut engine) = world.get_non_send_mut::<MidiSynthEngine>() {
        engine.remove_node(node_id);
    }
}