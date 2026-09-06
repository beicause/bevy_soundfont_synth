//! Bevy assets for SoundFont (.sf2/.sf3) and MIDI (.mid/.midi) files.
//!
//! The heavy lifting is done by the local `rustysynth-ext` fork:
//! - `SoundFont::new` parses both SF2 and (with the `sf3` feature) SF3 files.
//! - `MidiFile::new` parses Standard MIDI Files, folding tempo changes into
//!   absolute event times that are exposed via `MidiFile::get_messages` /
//!   `MidiFile::get_times`.

use std::io::Cursor;
use std::sync::Arc;

use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, LoadContext};
use bevy_ecs::error::{BevyError, Severity};
use bevy_reflect::TypePath;
use bevy_tasks::ConditionalSendFuture;
use rustysynth_ext::{MidiFile, SoundFont};

/// A parsed SoundFont bank (SF2 or SF3).
///
/// Sample data is shared between every synthesizer through an `Arc`, so the
/// memory cost of spawning many players is small.
#[derive(Asset, TypePath, Debug)]
pub struct SoundFontAsset {
    pub(crate) font: Arc<SoundFont>,
}

impl SoundFontAsset {
    /// Access the underlying rustysynth [`SoundFont`].
    pub fn font(&self) -> &Arc<SoundFont> {
        &self.font
    }
}

/// A parsed Standard MIDI File.
///
/// Tempo changes are already folded into the absolute event times; see
/// [`rustysynth_ext::MidiFile::get_times`].
#[derive(Asset, TypePath, Debug)]
pub struct MidiFileAsset {
    pub(crate) midi: Arc<MidiFile>,
}

impl MidiFileAsset {
    /// The duration of the file in seconds.
    pub fn length_seconds(&self) -> f64 {
        self.midi.get_length()
    }

    /// Access the underlying rustysynth [`MidiFile`].
    pub fn midi(&self) -> &Arc<MidiFile> {
        &self.midi
    }
}

/// Loads `.sf2` and `.sf3` files into [`SoundFontAsset`].
#[derive(Default, TypePath)]
pub struct SoundFontLoader;

impl AssetLoader for SoundFontLoader {
    type Asset = SoundFontAsset;
    type Settings = ();
    type Error = BevyError;

    fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext,
    ) -> impl ConditionalSendFuture<Output = Result<Self::Asset, Self::Error>> {
        async move {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await.map_err(|e| {
                BevyError::new(Severity::Error, format!("failed to read soundfont: {e}"))
            })?;

            let mut cursor = Cursor::new(bytes);
            let font = SoundFont::new(&mut cursor).map_err(|e| {
                BevyError::new(
                    Severity::Error,
                    format!("invalid SoundFont (.sf2/.sf3): {e}"),
                )
            })?;

            Ok(SoundFontAsset {
                font: Arc::new(font),
            })
        }
    }

    fn extensions(&self) -> &[&str] {
        &["sf2", "sf3"]
    }
}

/// Loads `.mid` and `.midi` files into [`MidiFileAsset`].
#[derive(Default, TypePath)]
pub struct MidiFileLoader;

impl AssetLoader for MidiFileLoader {
    type Asset = MidiFileAsset;
    type Settings = ();
    type Error = BevyError;

    fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext,
    ) -> impl ConditionalSendFuture<Output = Result<Self::Asset, Self::Error>> {
        async move {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await.map_err(|e| {
                BevyError::new(Severity::Error, format!("failed to read midi file: {e}"))
            })?;

            let mut cursor = Cursor::new(bytes);
            let midi = MidiFile::new(&mut cursor).map_err(|e| {
                BevyError::new(
                    Severity::Error,
                    format!("invalid MIDI file (.mid/.midi): {e} (SMPTE time divisions are not supported)"),
                )
            })?;

            Ok(MidiFileAsset {
                midi: Arc::new(midi),
            })
        }
    }

    fn extensions(&self) -> &[&str] {
        &["mid", "midi"]
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny committed samples shipped with the local rustysynth fork, used as
    /// parsing fixtures (no downloads needed).
    fn fork_sample(name: &str) -> Vec<u8> {
        let path = format!("{}/rustysynth/samples/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).unwrap_or_else(|e| {
            panic!("missing fork sample {path}: {e} (is the rustysynth subtree present?)")
        })
    }

    /// Minimal format-0 SMF: one C4 quarter note at 120 BPM.
    fn minimal_smf_bytes() -> Vec<u8> {
        let mut track = Vec::new();
        let push_vlq = |buf: &mut Vec<u8>, mut value: u32| {
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
        };
        push_vlq(&mut track, 0);
        track.extend_from_slice(&[0x90, 60, 100]);
        push_vlq(&mut track, 420);
        track.extend_from_slice(&[0x80, 60, 0]);
        push_vlq(&mut track, 0);
        track.extend_from_slice(&[0xFF, 0x2F, 0x00]);

        let mut midi = Vec::new();
        midi.extend_from_slice(b"MThd");
        midi.extend_from_slice(&6u32.to_be_bytes());
        midi.extend_from_slice(&[0, 0, 0, 1]); // format 0, 1 track
        midi.extend_from_slice(&480u16.to_be_bytes());
        midi.extend_from_slice(b"MTrk");
        midi.extend_from_slice(&(track.len() as u32).to_be_bytes());
        midi.extend_from_slice(&track);
        midi
    }

    #[test]
    fn committed_fork_sf2_fixture_is_rejected_like_upstream() {
        // The fork ships `test_empty_samples.sf2` to exercise its own error
        // path: a bank with no usable sample data.
        let bytes = fork_sample("test_empty_samples.sf2");
        let mut cursor = Cursor::new(bytes);
        assert!(matches!(
            SoundFont::new(&mut cursor),
            Err(rustysynth_ext::SoundFontError::SampleDataNotFound)
        ));
    }

    #[test]
    fn optional_downloaded_font_parses() {
        // Only present after `cargo xtask fetch-fonts`; skips on fresh clones.
        let path = format!("{}/assets/TimGM6mb.sf2", env!("CARGO_MANIFEST_DIR"));
        if !std::path::Path::new(&path).exists() {
            eprintln!("skipping: TimGM6mb.sf2 not present (run `cargo xtask fetch-fonts`)");
            return;
        }
        let bytes = std::fs::read(&path).unwrap();
        let mut cursor = Cursor::new(bytes);
        let font = SoundFont::new(&mut cursor).expect("TimGM6mb.sf2 parses");
        assert!(font.get_info().get_version().get_major() >= 1);
    }

    #[test]
    fn parses_committed_fork_sf3_sample() {
        let bytes = fork_sample("dummy.sf3");
        let mut cursor = Cursor::new(bytes);
        let font = SoundFont::new(&mut cursor).expect("dummy.sf3 must parse with the sf3 feature");
        assert_eq!(font.get_bits_per_sample(), 16);
    }

    #[test]
    fn rejects_garbage_soundfont_bytes() {
        let mut cursor = Cursor::new(vec![0x42u8; 128]);
        assert!(SoundFont::new(&mut cursor).is_err());
    }

    #[test]
    fn parses_minimal_smf() {
        let mut cursor = Cursor::new(minimal_smf_bytes());
        let midi = MidiFile::new(&mut cursor).expect("hand-built SMF parses");
        assert!(!midi.get_messages().is_empty());
        assert!(midi.get_length() > 0.0);
    }

    #[test]
    fn rejects_garbage_midi_bytes() {
        let mut cursor = Cursor::new(vec![0u8; 64]);
        assert!(MidiFile::new(&mut cursor).is_err());
    }

    #[test]
    fn mthd_wrong_chunk_id_rejected() {
        // Valid-looking header length but bad chunk id.
        let mut bad = Vec::new();
        bad.extend_from_slice(b"NOPE");
        bad.extend_from_slice(&6u32.to_be_bytes());
        bad.extend_from_slice(&[0, 0, 1, 0, 0x01, 0xE0]);
        bad.extend_from_slice(b"MTrk");
        bad.extend_from_slice(&0u32.to_be_bytes());
        let mut cursor = Cursor::new(bad);
        assert!(MidiFile::new(&mut cursor).is_err());
    }
}
