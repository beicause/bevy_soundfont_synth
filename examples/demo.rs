//! Manual smoke test for all three use cases, split across two synth entities
//! with different SoundFonts:
//! * `TimGM6mb.sf2` (SF2) plays requirement 1 (immediate `MidiEvent`s) and
//!   requirement 2 (a timed event sequence).
//! * `FluidR3Mono_GM.sf3` (SF3) plays requirement 3: `demo_generated.mid`
//!   finishes twice, then playback switches to `Only Time.mid` (looping). Each
//!   loop of `Only Time.mid` prints the accumulated loop count
//!   ([`MidiPlaybackRestarted`]); every finished play fires
//!   [`MidiPlaybackFinished`].
//!
//! Prepare the assets first (they are git-ignored):
//!
//! ```text
//! cargo xtask fetch-fonts        # assets/TimGM6mb.sf2 + assets/FluidR3Mono_GM.sf3
//! cargo xtask generate-demo-midi # assets/demo_generated.mid
//! # optionally drop "Only Time.mid" into assets/
//! ```
//!
//! Then run: `cargo run --example demo` (requires an audio device). Volume
//! uses the plugin defaults (`Volume::UNITY_GAIN`).

use std::time::Duration;

use bevy_app::{App, ScheduleRunnerPlugin, Startup, TaskPoolPlugin, Update};
use bevy_asset::{AssetPlugin, AssetServer, Handle};
use bevy_ecs::observer::On;
use bevy_ecs::prelude::*;
use bevy_log::{LogPlugin, info, warn};
use bevy_soundfont_synth::{
    MidiEvent, MidiEventKind, MidiPlaybackFinished, MidiPlaybackRestarted, MidiPlaybackSettings,
    MidiPlayer, MidiSoundFont, SoundFontAsset, SoundFontSynthPlugin, TimedMidiEvent,
};
use bevy_time::TimePlugin;

fn main() {
    let asset_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    if !asset_dir.join("TimGM6mb.sf2").exists() {
        warn!("assets/TimGM6mb.sf2 not found — run `cargo xtask fetch-fonts` first");
    }
    if !asset_dir.join("FluidR3Mono_GM.sf3").exists() {
        warn!("assets/FluidR3Mono_GM.sf3 not found — run `cargo xtask fetch-fonts` first");
    }
    if !asset_dir.join("demo_generated.mid").exists() {
        warn!("assets/demo_generated.mid not found — run `cargo xtask generate-demo-midi` first");
    }
    if !asset_dir.join("Only Time.mid").exists() {
        warn!("assets/Only Time.mid not found — the demo will stay silent after the 3rd demo loop");
    }

    App::new()
        .add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(16)))
        .add_plugins((
            LogPlugin::default(),
            TaskPoolPlugin::default(),
            AssetPlugin::default(),
            TimePlugin,
        ))
        .add_plugins(SoundFontSynthPlugin::default())
        .init_resource::<Demo>()
        .add_systems(Startup, setup)
        .add_systems(Update, demo_timeline)
        .add_observer(on_file_finished)
        .add_observer(on_file_restarted)
        .run();
}

#[derive(Resource, Default)]
struct Demo {
    /// SF2 synth entity (TimGM6mb): req 1 immediate events + req 2 sequence.
    synth_tim: Option<Entity>,
    /// SF3 synth entity (FluidR3Mono): req 3 MIDI file playback.
    synth_sf3: Option<Entity>,
    note_on_sent: bool,
    note_off_sent: bool,
    sequence_started: bool,
    /// Whether the first `demo_generated.mid` play was started.
    file_started: bool,
    /// Completed non-looped plays of `demo_generated.mid`.
    file_round: u32,
    /// Whether playback switched to `Only Time.mid` (looping).
    only_time_started: bool,
}

const DEMO_GENERATED_ROUNDS: u32 = 2;

fn setup(mut commands: Commands, server: Res<AssetServer>, mut demo: ResMut<Demo>) {
    let font_tim: Handle<SoundFontAsset> = server.load("TimGM6mb.sf2");
    demo.synth_tim = Some(commands.spawn(MidiSoundFont(font_tim)).id());
    let font_sf3: Handle<SoundFontAsset> = server.load("FluidR3Mono_GM.sf3");
    demo.synth_sf3 = Some(commands.spawn(MidiSoundFont(font_sf3)).id());
    info!("demo: spawned synth entity (TimGM6mb.sf2) and synth entity (FluidR3Mono_GM.sf3)");
}

fn demo_timeline(
    mut commands: Commands,
    server: Res<AssetServer>,
    time: Res<bevy_time::Time>,
    mut demo: ResMut<Demo>,
) {
    let t = time.elapsed_secs();

    // Req 1: immediate MIDI events on the TimGM6mb entity.
    if t >= 0.5 && !demo.note_on_sent {
        demo.note_on_sent = true;
        info!("demo: note-on C4 (immediate event, TimGM6mb)");
        if let Some(synth) = demo.synth_tim {
            commands.entity(synth).trigger(|e| {
                MidiEvent::on(
                    e,
                    MidiEventKind::NoteOn {
                        channel: 0,
                        key: 60,
                        velocity: 100,
                    },
                )
            });
        }
    }
    if t >= 1.0 && !demo.note_off_sent {
        demo.note_off_sent = true;
        info!("demo: note-off C4 (immediate event, TimGM6mb)");
        if let Some(synth) = demo.synth_tim {
            commands.entity(synth).trigger(|e| {
                MidiEvent::on(
                    e,
                    MidiEventKind::NoteOff {
                        channel: 0,
                        key: 60,
                    },
                )
            });
        }
    }

    // Req 2: a timed event sequence on the TimGM6mb entity.
    if t >= 1.5 && !demo.sequence_started {
        demo.sequence_started = true;
        info!("demo: playing sequence C4 E4 G4 C5 (TimGM6mb)");
        if let Some(synth) = demo.synth_tim {
            let events = [60, 64, 67, 72]
                .iter()
                .enumerate()
                .flat_map(|(i, &key)| {
                    [
                        TimedMidiEvent {
                            seconds: i as f64 * 0.15,
                            kind: MidiEventKind::NoteOn {
                                channel: 0,
                                key,
                                velocity: 90,
                            },
                        },
                        TimedMidiEvent {
                            seconds: i as f64 * 0.15 + 0.12,
                            kind: MidiEventKind::NoteOff { channel: 0, key },
                        },
                    ]
                })
                .collect();
            commands.entity(synth).insert(MidiPlayer::sequence(events));
            commands.entity(synth).insert(MidiPlaybackSettings::ONCE);
        }
    }

    // Req 3: play a MIDI file on the SF3 entity — `demo_generated.mid`
    // (non-looping) to be repeated 3 times, then `Only Time.mid` (looping).
    if t >= 2.5 && !demo.file_started {
        demo.file_started = true;
        info!(
            "demo: playing MIDI file demo_generated.mid, round 1/{} (FluidR3Mono_GM.sf3)",
            DEMO_GENERATED_ROUNDS
        );
        if let Some(synth) = demo.synth_sf3 {
            commands
                .entity(synth)
                .insert(MidiPlayer::file(server.load("demo_generated.mid")));
            commands.entity(synth).insert(MidiPlaybackSettings::ONCE);
        }
    }
}

/// Observe `MidiPlaybackFinished` on the SF3 file entity: replay
/// `demo_generated.mid` until 2 rounds are done, then switch to
/// `Only Time.mid` (looping). `loop_count` on the event is unused here (the
/// demo rounds are non-looping plays).
fn on_file_finished(
    event: On<MidiPlaybackFinished>,
    mut commands: Commands,
    server: Res<AssetServer>,
    mut demo: ResMut<Demo>,
) {
    let Some(synth_sf3) = demo.synth_sf3 else {
        return;
    };

    if event.entity != synth_sf3 || !demo.file_started || demo.only_time_started {
        return;
    }

    demo.file_round += 1;
    if demo.file_round < DEMO_GENERATED_ROUNDS {
        info!(
            "demo: demo_generated.mid round {}/{}",
            demo.file_round + 1,
            DEMO_GENERATED_ROUNDS
        );
        commands
            .entity(synth_sf3)
            .insert(MidiPlayer::file(server.load("demo_generated.mid")));
        commands
            .entity(synth_sf3)
            .insert(MidiPlaybackSettings::ONCE);
    } else {
        demo.only_time_started = true;
        info!("demo: switching to Only Time.mid (looping, FluidR3Mono_GM.sf3)");
        commands
            .entity(synth_sf3)
            .insert(MidiPlayer::file(server.load("Only Time.mid")));
        commands
            .entity(synth_sf3)
            .insert(MidiPlaybackSettings::LOOP);
    }
}

/// Observe `MidiPlaybackRestarted` on the SF3 file entity: each loop of
/// `Only Time.mid` prints the accumulated loop count.
fn on_file_restarted(event: On<MidiPlaybackRestarted>, demo: Res<Demo>) {
    let Some(synth_sf3) = demo.synth_sf3 else {
        return;
    };
    if event.entity != synth_sf3 || !demo.only_time_started {
        return;
    }
    info!("demo: Only Time.mid looped {} times", event.loop_count);
}
