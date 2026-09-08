# bevy_soundfont_synth

A [Bevy](https://bevyengine.org) (0.19) plugin that loads **SoundFont** (SF2/SF3)
and **MIDI** files as assets and plays them through a
[firewheel](https://github.com/BillyDM/firewheel) audio graph backed by a local
fork of [rustysynth](https://github.com/beicause/rustysynth) (`rustysynth/`).

Three ways to produce audio, all unified under one playback scheduler:

| # | What | API |
|---|------|-----|
| 1 | Trigger a single MIDI message immediately | [`MidiEvent`] `EntityEvent`, triggered **on** a synth entity |
| 2 | Queue a series of timed MIDI events | `MidiPlayer(MidiSource::sequence(..))` component |
| 3 | Play a MIDI file asset | `MidiPlayer(MidiSource::file(handle))` component |

## Features

- **Assets**: `.sf2` / `.sf3` → `SoundFontAsset` (SF3 Ogg decoding via the `sf3`
  feature), `.mid` / `.midi` → `MidiFileAsset` (tempo changes folded into
  absolute event times by the fork's parser).
- **firewheel node as a component**: every entity with a `MidiSoundFont`
  component gets its own synthesizer node (`MidiSynthNode`); a component hook
  tears it down when the font is replaced or the entity despawns. The SoundFont
  is **per entity** — nothing is global.
- **`NonSend` audio context**: `FirewheelContext` + `CpalStream` live in the
  [`MidiSynthEngine`] `NonSend` resource.
- **Sample-accurate scheduling** (firewheel `scheduled_events`): sequences and
  files are polled every frame on the main thread and every due MIDI message is
  scheduled at its absolute audio-clock instant, so notes land exactly on the
  audio thread.
- **Entity events for immediate messages** (Bevy 0.19 `Event`/`EntityEvent`
  + observer; `world.trigger`-free, triggered on the target entity).

## Usage

```rust,ignore
use bevy::prelude::*;
use bevy_soundfont_synth::{
    MidiEvent, MidiEventKind, MidiPlaybackSettings, MidiPlayer, MidiSoundFont,
    MidiSource, SoundFontSynthPlugin, TimedMidiEvent,
};

fn setup(mut commands: Commands, server: Res<AssetServer>) {
    // A synth entity with its own SoundFont (its synthesizer node is created
    // automatically once the font asset is loaded):
    let synth = commands
        .spawn(MidiSoundFont(server.load("fonts/FluidR3Mono_GM.sf2")))
        .id();

    // 1) Immediate MIDI events, triggered on the synth entity:
    commands.entity(synth).trigger(|e| {
        MidiEvent::on(e, MidiEventKind::NoteOn { channel: 0, key: 60, velocity: 100 })
    });
    // ... later:
    commands.entity(synth).trigger(|e| {
        MidiEvent::on(e, MidiEventKind::NoteOff { channel: 0, key: 60 })
    });

    // 2) A series of timed MIDI events:
    let events = (0..4).flat_map(|i| {
        [
            TimedMidiEvent { seconds: i as f64 * 0.15, kind: MidiEventKind::NoteOn { channel: 0, key: 60 + i * 4, velocity: 90 } },
            TimedMidiEvent { seconds: i as f64 * 0.15 + 0.12, kind: MidiEventKind::NoteOff { channel: 0, key: 60 + i * 4 } },
        ]
    }).collect();
    commands.spawn((
        MidiSoundFont(server.load("fonts/FluidR3Mono_GM.sf2")),
        MidiPlayer::sequence(events),
        MidiPlaybackSettings::ONCE,
    ));

    // 3) A MIDI file (played through the same scheduler):
    commands.spawn((
        MidiSoundFont(server.load("fonts/FluidR3Mono_GM.sf2")),
        MidiPlayer::file(server.load("songs/canon.mid")),
        MidiPlaybackSettings::LOOP,
    ));
}

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, SoundFontSynthPlugin))
        .add_systems(Startup, setup)
        .run();
}
```

Control during playback by mutating `MidiPlaybackSettings`
(`paused`, `speed`, `volume`); removing `MidiPlayer` (or despawning the entity)
stops playback. Playback notifications are *entity events* triggered on the
playing entity and observed with `On<..>` observers:
[`MidiPlaybackFinished`] fires when a non-looping playback finishes (carrying
the total `loop_count`), [`MidiPlaybackRestarted`] fires on every loop wrap
(carrying the 1-based `loop_count`). `MidiPlaybackMode::Despawn` / `Remove`
handle the entity automatically.

## SoundFont banks

The `.sf2`/`.sf3` banks in `assets/` are git-ignored: they are large and not
redistributed here. Fetch the demo fonts with the bundled xtask:

```sh
cargo xtask fetch-fonts   # downloads assets/TimGM6mb.sf2
```

`xtask/` is a small utility crate (a `cargo` alias in `.cargo/config.toml`);
sources are the same mirrors the upstream rustysynth project verifies.
Alternatively, drop any soundfont you have into `assets/` and load it by path.

## Architecture

```
Bevy world                                        NonSend MidiSynthEngine
  MidiEvent (EntityEvent, triggered on entity) ──▶ observer → live buffer
  MidiSoundFont + MidiPlayer + MidiPlaybackSettings
    │  synth_node_sync: build firewheel node      cx.add_node(SynthNode)
    │  player_sources_sync: MidiSource::resolve → Arc<MidiFile> (shared)
    │  poll_playbacks (every frame):
    │    PlaybackState::poll(audio clock) → due messages
    │      └─ engine.enqueue(node, AtClockSeconds(t), SynthMsg::Midi(msg))
    └─ engine_tick (Last): cx.schedule_event_for(...) → cx.update()
                                                     │ firewheel events channel
                                                     ▼
                                        MidiSynthProcessor (audio thread)
                                          events(): apply SynthMsg
                                          process(): synth.render(...) → CPAL
```

## Modules

- `assets` — `SoundFontAsset`, `MidiFileAsset` + asset loaders.
- `events` — `MidiEvent` (EntityEvent), `MidiPlaybackFinished` /
  `MidiPlaybackRestarted` (EntityEvents, triggered by the engine on the playing
  entity), `MidiStreamError` (Message).
- `midi` — `MidiEventKind`, `TimedMidiEvent` and conversions to/from the fork's
  `MidiMessage`.
- `play` — components: `MidiSoundFont`, `MidiPlayer`, the `MidiSource` trait
  (`FileSource`, `SequenceSource`), `MidiPlaybackSettings`/`MidiPlaybackMode`.
- `playback` — pure, testable `PlaybackState` scheduler (looping, speed, pause,
  start offset).
- `node` — `MidiSynthNode` component + firewheel node/processor.
- `engine` — `SoundFontSynthPlugin`, `MidiSynthEngine` (NonSend), systems.

## Notes & limitations

- Events are ~1 frame + 1 block late at most; scheduled file/sequence events
  are sample-accurate against firewheel's audio clock (a rhythm game should
  sync to that clock).
- Changing `MidiSoundFont` rebuilds the synthesizer (stops current voices);
  replacing `MidiPlayer` restarts playback and resets the synthesizer.
- Looping, symtem-po folding, multi-track merging and SF3 decoding come from
  the local rustysynth fork; SMPTE-timed MIDI files are rejected with a clear
  error.
- Sequences passed to `MidiPlayer::sequence` are sorted internally; times must
  be non-negative.
- Requires a host app with `bevy_asset::AssetPlugin` (part of
  `DefaultPlugins`); the demo uses `ScheduleRunnerPlugin` + `TaskPoolPlugin` +
  `AssetPlugin` + `TimePlugin`.

## License

Except where noted (below and/or in individual files), all code in this repository is dual-licensed under either:

* MIT License ([LICENSE-MIT](LICENSE-MIT) or [http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))
* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or [http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))

at your option.

Exception:
- `assets/Only Time.mid` comes from the
  [Lakh MIDI Dataset v0.1](https://colinraffel.com/projects/lmd/).
  Its attribution notice lives in `assets/Only Time.mid.license`.
