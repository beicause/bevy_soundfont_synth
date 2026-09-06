//! `cargo xtask <task>` — development tasks for this repository.
//!
//! Tasks:
//! * `fetch-fonts`        — downloads the SoundFont banks into `assets/`
//!   (the files themselves are git-ignored; see the root `.gitignore`).
//! * `generate-demo-midi` — writes the demo MIDI file `assets/demo_generated.mid`.
//!   Sources are the same verified files used by the upstream rustysynth fork
//!   (https://github.com/beicause/rustysynth).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/xtask
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn download(url: &str, target: &Path) -> Result<(), String> {
    let agent = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .timeout_recv_body(Some(Duration::from_secs(300)))
        .build()
        .new_agent();
    let mut response = agent.get(url).call().map_err(|err| err.to_string())?;
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(512 * 1024 * 1024)
        .read_to_end(&mut body)
        .map_err(|err| err.to_string())?;
    fs::write(target, body).map_err(|err| err.to_string())
}

/// Download `name` from `mirrors` into `assets_dir`, skipping if it exists.
fn fetch_with_mirrors(name: &str, mirrors: &[&str], assets_dir: &Path) -> Result<(), String> {
    let target = assets_dir.join(name);

    if target.is_file() {
        println!("exists: {}", target.display());
        return Ok(());
    }

    for url in mirrors {
        println!("downloading {url} ...");
        match download(url, &target) {
            Ok(()) => {
                println!("saved: {}", target.display());
                return Ok(());
            }
            Err(err) => {
                let _ = fs::remove_file(&target);
                eprintln!("  failed: {err}");
            }
        }
    }

    Err(format!("could not download {name} from any source"))
}

/// Mirrors for TimGM6mb.sf2, verified to produce the same data the upstream
/// fork's golden tests expect.
const TIMGM6MB_MIRRORS: &[&str] = &[
    "https://member.keymusician.com/Member/TimGM6mb.sf2",
    "https://raw.githubusercontent.com/arbruijn/TimGM6mb/master/TimGM6mb.sf2",
    "https://raw.githubusercontent.com/deepin-community/timgm6mb-soundfont/master/TimGM6mb.sf2",
];

/// Mirrors for FluidR3Mono_GM.sf3, shipped by the upstream rustysynth fork.
const FLUID_SF3_MIRRORS: &[&str] = &[
    "https://raw.githubusercontent.com/beicause/rustysynth/main/samples/FluidR3Mono_GM.sf3",
    "https://github.com/beicause/rustysynth/raw/main/samples/FluidR3Mono_GM.sf3",
];

fn fetch_timgm6mb(assets_dir: &Path) -> Result<(), String> {
    fetch_with_mirrors("TimGM6mb.sf2", TIMGM6MB_MIRRORS, assets_dir)
}

fn fetch_fluid_sf3(assets_dir: &Path) -> Result<(), String> {
    fetch_with_mirrors("FluidR3Mono_GM.sf3", FLUID_SF3_MIRRORS, assets_dir)
}

/// Encode a MIDI variable-length quantity.
fn push_vlq(buf: &mut Vec<u8>, mut value: u32) {
    let mut bytes = [0u8; 4];
    let mut i = 3;
    bytes[3] = (value & 0x7F) as u8;
    value >>= 7;
    while value > 0 {
        i -= 1;
        bytes[i] = ((value & 0x7F) | 0x80) as u8;
        value >>= 7;
    }
    buf.extend_from_slice(&bytes[i..]);
}

/// Write a small format-0 demo SMF: a two-octave C major arpeggio at 120 BPM,
/// used by `examples/demo.rs` for the "play a MIDI file" case.
fn write_demo_midi(path: &Path) -> Result<(), String> {
    const TPB: u32 = 480; // ticks per quarter note
    // (note, octave) pairs, one quarter note each: C4 E4 G4 C5 · G4 C5 E5 G5
    let notes: &[(u8, u8)] = &[
        (0, 4), (4, 4), (7, 4), (12, 4),
        (7, 4), (12, 4), (16, 4), (19, 4),
    ];

    let mut track = Vec::new();
    // Explicit tempo: 500000 us/quarter = 120 BPM (also the default).
    push_vlq(&mut track, 0);
    track.extend_from_slice(&[0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]);

    // SMF deltas are *relative* to the previous event: keep a running tick.
    let mut prev_tick = 0u32;
    for (step, &(semitone, octave)) in notes.iter().enumerate() {
        let key = 12 * octave + semitone;
        let tick = step as u32 * TPB;
        push_vlq(&mut track, tick - prev_tick);
        track.extend_from_slice(&[0x90, key, 100]); // Note On, channel 0
        push_vlq(&mut track, TPB - 60);
        track.extend_from_slice(&[0x80, key, 0]); // Note Off just before the next beat
        prev_tick = tick + (TPB - 60);
    }

    push_vlq(&mut track, 0);
    track.extend_from_slice(&[0xFF, 0x2F, 0x00]); // End of track

    let mut midi = Vec::new();
    midi.extend_from_slice(b"MThd");
    midi.extend_from_slice(&6u32.to_be_bytes());
    midi.extend_from_slice(&[0, 0, 0, 1]); // format 0, 1 track
    midi.extend_from_slice(&(TPB as u16).to_be_bytes());
    midi.extend_from_slice(b"MTrk");
    midi.extend_from_slice(&(track.len() as u32).to_be_bytes());
    midi.extend_from_slice(&track);

    fs::write(path, midi).map_err(|err| err.to_string())
}

fn usage() {
    eprintln!(
        "Usage: cargo xtask <task>\n\nTasks:\n  \
         fetch-fonts        download the SoundFont banks into assets/\n  \
         generate-demo-midi write assets/demo_generated.mid for examples/demo.rs"
    );
}

fn main() {
    let task = std::env::args().nth(1);
    let code = match task.as_deref() {
        Some("fetch-fonts") => {
            let assets_dir = repo_root().join("assets");
            if let Err(err) = fs::create_dir_all(&assets_dir) {
                eprintln!("error: {err}");
                1
            } else {
                let mut failed = false;
                for (name, fetcher) in [
                    ("TimGM6mb.sf2", fetch_timgm6mb as fn(&Path) -> Result<(), String>),
                    ("FluidR3Mono_GM.sf3", fetch_fluid_sf3 as fn(&Path) -> Result<(), String>),
                ] {
                    match fetcher(&assets_dir) {
                        Ok(()) => println!("ok: {name}"),
                        Err(err) => {
                            eprintln!("error: {err}");
                            failed = true;
                        }
                    }
                }
                if failed {
                    1
                } else {
                    println!("done: fonts ready in {}", assets_dir.display());
                    0
                }
            }
        }
        Some("generate-demo-midi") => {
            let assets_dir = repo_root().join("assets");
            if let Err(err) = fs::create_dir_all(&assets_dir) {
                eprintln!("error: {err}");
                1
            } else {
                let target = assets_dir.join("demo_generated.mid");
                match write_demo_midi(&target) {
                    Ok(()) => {
                        println!("saved: {}", target.display());
                        0
                    }
                    Err(err) => {
                        eprintln!("error: {err}");
                        1
                    }
                }
            }
        }
        Some(other) => {
            eprintln!("unknown task: {other}\n");
            usage();
            2
        }
        None => {
            usage();
            1
        }
    };
    std::process::exit(code);
}