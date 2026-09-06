//! Ergonomic MIDI event types and conversions to/from the local rustysynth
//! fork's `MidiMessage`.

use std::sync::Arc;

use rustysynth_ext::{MidiFile, MidiFileError, MidiMessage};

/// A named MIDI message, used for immediate `MidiEvent`s and for
/// `MidiSequencePlayer` event lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiEventKind {
    /// Note On. A velocity of 0 is treated as a Note Off (standard behavior).
    NoteOn {
        channel: u8,
        key: u8,
        velocity: u8,
    },
    /// Note Off.
    NoteOff {
        channel: u8,
        key: u8,
    },
    /// Control Change.
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    /// Program Change (instrument selection).
    ProgramChange {
        channel: u8,
        program: u8,
    },
    /// Pitch Bend, in the range -8192..=8191.
    PitchBend {
        channel: u8,
        value: i16,
    },
}

impl MidiEventKind {
    /// Encode into the fork's compact `MidiMessage` representation
    /// (`status` carries the channel in its low nibble).
    pub fn into_message(self) -> MidiMessage {
        match self {
            MidiEventKind::NoteOn {
                channel,
                key,
                velocity,
            } => MidiMessage::Normal {
                status: 0x90 | (channel & 0x0F),
                data1: key,
                data2: velocity,
            },
            MidiEventKind::NoteOff { channel, key } => MidiMessage::Normal {
                status: 0x80 | (channel & 0x0F),
                data1: key,
                data2: 0,
            },
            MidiEventKind::ControlChange {
                channel,
                controller,
                value,
            } => MidiMessage::Normal {
                status: 0xB0 | (channel & 0x0F),
                data1: controller,
                data2: value,
            },
            MidiEventKind::ProgramChange { channel, program } => MidiMessage::Normal {
                status: 0xC0 | (channel & 0x0F),
                data1: program,
                data2: 0,
            },
            MidiEventKind::PitchBend { channel, value } => {
                let raw = (value as i32 + 8192).clamp(0, 16_383);
                MidiMessage::Normal {
                    status: 0xE0 | (channel & 0x0F),
                    data1: (raw & 0x7F) as u8,
                    data2: ((raw >> 7) & 0x7F) as u8,
                }
            }
        }
    }

    /// Decode from the fork's `MidiMessage`. Returns `None` for non-channel
    /// messages (`TempoChange`, `LoopStart`, `LoopEnd`, `EndOfTrack`).
    pub fn from_message(message: MidiMessage) -> Option<Self> {
        match message {
            MidiMessage::Normal { status, data1, data2 } => {
                let channel = status & 0x0F;
                match status & 0xF0 {
                    0x80 => Some(MidiEventKind::NoteOff {
                        channel,
                        key: data1,
                    }),
                    0x90 => Some(MidiEventKind::NoteOn {
                        channel,
                        key: data1,
                        velocity: data2,
                    }),
                    0xB0 => Some(MidiEventKind::ControlChange {
                        channel,
                        controller: data1,
                        value: data2,
                    }),
                    0xC0 => Some(MidiEventKind::ProgramChange {
                        channel,
                        program: data1,
                    }),
                    0xE0 => Some(MidiEventKind::PitchBend {
                        channel,
                        value: ((data2 as i16) << 7 | data1 as i16) - 8192,
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// A MIDI event with an absolute time offset in seconds (relative to the start
/// of its sequence).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimedMidiEvent {
    /// Seconds from the start of the sequence.
    pub seconds: f64,
    pub kind: MidiEventKind,
}

/// Build a parsed-rustysynth [`MidiFile`] from a custom event list (requirement 2).
///
/// Times must be non-decreasing; `MidiFile::new_with_events` validates this and
/// returns [`MidiFileError::InvalidEventList`] otherwise. Events at the same
/// time keep their relative order (stable sort).
pub fn build_midi_file(
    mut events: Vec<TimedMidiEvent>,
) -> Result<Arc<MidiFile>, MidiFileError> {
    events.sort_by(|a, b| a.seconds.total_cmp(&b.seconds));
    let messages = events
        .into_iter()
        .map(|e| (e.seconds, e.kind.into_message()));
    MidiFile::new_with_events(messages).map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_kinds() {
        let cases = [
            MidiEventKind::NoteOn {
                channel: 3,
                key: 60,
                velocity: 100,
            },
            MidiEventKind::NoteOff { channel: 3, key: 60 },
            MidiEventKind::ControlChange {
                channel: 1,
                controller: 7,
                value: 127,
            },
            MidiEventKind::ProgramChange {
                channel: 9,
                program: 0,
            },
            MidiEventKind::PitchBend {
                channel: 4,
                value: 0,
            },
            MidiEventKind::PitchBend {
                channel: 4,
                value: -8192,
            },
            MidiEventKind::PitchBend {
                channel: 4,
                value: 8191,
            },
        ];
        for kind in cases {
            let msg = kind.into_message();
            assert_eq!(MidiEventKind::from_message(msg), Some(kind), "{kind:?}");
        }
    }

    #[test]
    fn non_channel_messages_do_not_decode() {
        assert_eq!(MidiEventKind::from_message(MidiMessage::TempoChange { bytes: [1, 2, 3] }), None);
        assert_eq!(MidiEventKind::from_message(MidiMessage::EndOfTrack), None);
    }

    #[test]
    fn build_file_validates_and_sorts() {
        // Out of order input: builder sorts stably instead of erroring? No —
        // new_with_events rejects unsorted input, so unsorted is an error,
        // while the builder's sort guarantees success.
        let bad = build_midi_file(vec![
            TimedMidiEvent {
                seconds: 1.0,
                kind: MidiEventKind::NoteOff {
                    channel: 0,
                    key: 60,
                },
            },
            TimedMidiEvent {
                seconds: 0.0,
                kind: MidiEventKind::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 100,
                },
            },
        ]);
        assert!(bad.is_ok());

        // build_midi_file sorts before constructing, so this must succeed.
        let file = bad.unwrap();
        assert_eq!(file.get_times().len(), 2);
        let times = file.get_times();
        assert_eq!(times[0], 0.0);
        assert_eq!(times[1], 1.0);
        assert_eq!(file.get_length(), 1.0);
    }

    #[test]
    fn expedite_rejects_non_decreasing_without_sort() {
        // Direct new_with_events on sorted input works...
        let ok = MidiFile::new_with_events([
            (0.0, MidiEventKind::NoteOn { channel: 0, key: 60, velocity: 100 }.into_message()),
            (
                0.5,
                MidiEventKind::NoteOff { channel: 0, key: 60 }.into_message(),
            ),
        ]);
        assert!(ok.is_ok());

        // ...and unsorted input is rejected by the fork.
        let bad = MidiFile::new_with_events([
            (
                0.5,
                MidiEventKind::NoteOff { channel: 0, key: 60 }.into_message(),
            ),
            (0.0, MidiEventKind::NoteOn { channel: 0, key: 60, velocity: 100 }.into_message()),
        ]);
        assert!(bad.is_err());
    }
}