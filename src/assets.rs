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
