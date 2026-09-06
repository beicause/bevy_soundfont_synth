//! Main-thread playback state for sequences / MIDI files (requirements 2 and 3).
//!
//! The engine *polls* this state every frame against firewheel's audio clock
//! and schedules every due MIDI event individually through firewheel's
//! `scheduled_events` API (`EventInstant::AtClockSeconds`), so events land on
//! the audio thread at sample-accurate times without any per-sample work on
//! the main thread.
//!
//! Time model: playback runs in cycles. At `cycle_start_audio` (an audio-clock
//! reading in seconds) the file position was `start_file`; file time advances
//! at `speed` times wall-clock, and every event maps back to an absolute
//! audio-clock instant. Looping wraps the file position into
//! `[start_file, length)` again. Pausing and speed changes "fold" the cycle so
//! no wall-clock time is lost or double counted.

use std::sync::Arc;

use bevy_ecs::prelude::Component;
use rustysynth_ext::{MidiFile, MidiMessage};

/// A due event: the message to dispatch and the absolute audio-clock instant
/// (in seconds) at which it should sound.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DueEvent {
    pub message: MidiMessage,
    pub audio_instant_seconds: f64,
}

/// The result of one [`PlaybackState::poll`]: the due MIDI events plus how many
/// times looping playback wrapped back to the start during this poll.
///
/// The `due` slice borrows a buffer owned by [`PlaybackState`] and reused
/// across polls, so no per-frame allocation happens.
#[derive(Debug, Clone, PartialEq)]
pub struct PollResult<'a> {
    /// The events to schedule, in file order.
    pub due: &'a [DueEvent],
    /// How many elapsed cycles were folded during this poll (0 normally, >1
    /// when the audio clock jumped far ahead of the last poll).
    pub restarts: u32,
    /// Total number of loop restarts since playback started.
    pub loop_count: u32,
}

/// Pure playback state; no firewheel or bevy types involved, fully testable.
pub struct PlaybackState {
    midi: Arc<MidiFile>,
    play_loop: bool,
    speed: f64,
    paused: bool,
    cursor: usize,
    /// Audio-clock (seconds) at which the current cycle started.
    cycle_start_audio: f64,
    /// File position (seconds) at which the current cycle started.
    start_file: f64,
    /// File length in seconds.
    length: f64,
    done: bool,
    /// Total loop restarts since playback started (0 until the first wrap).
    loop_count: u32,
    /// Reused event buffer handed out by [`PlaybackState::poll`]; cleared and
    /// refilled every poll to avoid per-frame allocations.
    due: Vec<DueEvent>,
}

impl PlaybackState {
    /// Start playing `midi` at the given audio-clock reading.
    ///
    /// `start_position` skips playback to an offset in seconds into the file;
    /// events before it are never played.
    pub fn new(
        midi: Arc<MidiFile>,
        play_loop: bool,
        start_position: f64,
        speed: f64,
        now_audio: f64,
    ) -> Self {
        let length = midi.get_length();
        let start_file = start_position.max(0.0);
        let times = midi.get_times();
        // Events strictly before the start position are skipped.
        let cursor = times.partition_point(|t| *t < start_file);
        let done = length <= start_file;
        Self {
            midi,
            play_loop,
            speed: speed.max(1e-6),
            paused: false,
            cursor,
            cycle_start_audio: now_audio,
            start_file,
            length,
            done,
            loop_count: 0,
            due: Vec::new(),
        }
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Total number of loop restarts so far (0 until the first wrap).
    pub fn loop_count(&self) -> u32 {
        self.loop_count
    }

    /// The file position in seconds at the given audio-clock reading.
    fn file_now(&self, now_audio: f64) -> f64 {
        self.start_file + (now_audio - self.cycle_start_audio) * self.speed
    }

    /// Freeze the current file position: convert wall-clock progress into the
    /// cycle start, so that later changes (speed, pause) apply from now on.
    ///
    /// Already-dispatched events must never be replayed: the cursor only ever
    /// moves forward (`max` with the partition point).
    fn fold(&mut self, now_audio: f64) {
        let position = self.file_now(now_audio);
        self.cycle_start_audio = now_audio;
        self.start_file = position.max(0.0);
        let partition = self
            .midi
            .get_times()
            .partition_point(|t| *t < self.start_file);
        self.cursor = self.cursor.max(partition);
        if position >= self.length {
            self.done = true;
        }
    }

    /// Set the paused state. Folding ensures no wall-clock time is lost while
    /// paused.
    pub fn set_paused(&mut self, paused: bool, now_audio: f64) {
        if paused == self.paused {
            return;
        }
        if paused {
            self.fold(now_audio);
        } else {
            // Resuming: restart the cycle from *now* so the pause gap is not
            // counted as playback time.
            self.cycle_start_audio = now_audio;
        }
        self.paused = paused;
    }

    /// Change playback speed. Folds first so the speed change applies from
    /// now on.
    pub fn set_speed(&mut self, speed: f64, now_audio: f64) {
        if speed == self.speed {
            return;
        }
        self.fold(now_audio);
        self.speed = speed.max(1e-6);
    }

    /// Advance the playhead to `now_audio` and collect every event that is due
    /// at or before it, each with its absolute audio-clock instant, plus the
    /// number of loop restarts that occurred.
    ///
    /// # Termination guarantee (no loop cap needed)
    ///
    /// A single poll needs at most *one* loop wrap structurally:
    /// 1. Non-finite clocks or positions (`NaN`/`±Inf`) are rejected up front —
    ///    they would make every comparison below behave erratically.
    /// 2. The wrap computes `loops = floor((file_now - length)/cycle) + 1`,
    ///    which mathematically folds `file_now` back into
    ///    `[start_file, length)` in one step (any number of elapsed cycles at
    ///    once). A leftover non-finite/out-of-range `loops`, a wrap that does
    ///    not rewind the cursor, or a fold that failed to converge (float
    ///    edge) marks the playback `done` instead of looping again.
    /// 3. After the single fold, `file_now < length` holds, so the second
    ///    dispatch sweep ends the poll — there is nothing left to wrap.
    pub fn poll(&mut self, now_audio: f64) -> PollResult<'_> {
        // Reuse the buffer: clear it and hand out a borrow of it.
        self.due.clear();

        // Guard 1: a non-finite clock cannot advance the playhead. Reject the
        // poll without touching the state; a valid clock may come later.
        if self.paused || self.done || !now_audio.is_finite() {
            return PollResult {
                due: &self.due,
                restarts: 0,
                loop_count: self.loop_count,
            };
        }

        let times = self.midi.get_times();
        let messages = self.midi.get_messages();
        let mut file_now = self.file_now(now_audio);
        let cycle = self.length - self.start_file;
        let mut restarts: u32 = 0;

        // Guard 2: the playhead itself must be finite. `(now - cycle_start) *
        // speed` can overflow to `±Inf`; then no wrap can ever converge.
        if !file_now.is_finite() {
            return PollResult {
                due: &self.due,
                restarts: 0,
                loop_count: self.loop_count,
            };
        }

        // First sweep: dispatch every event of the current cycle up to the
        // playhead.
        while self.cursor < times.len() && times[self.cursor] <= file_now {
            let message = messages[self.cursor];
            // Tempo/Loop/EndOfTrack markers are meta data: never scheduled.
            if let MidiMessage::Normal { .. } = message {
                let audio_instant =
                    self.cycle_start_audio + (times[self.cursor] - self.start_file) / self.speed;
                self.due.push(DueEvent {
                    message,
                    audio_instant_seconds: audio_instant,
                });
            }
            self.cursor += 1;
        }

        // Wrap once, and only when the whole cycle has passed.
        if self.cursor == times.len() && file_now >= self.length {
            if !self.play_loop || cycle <= 0.0 {
                self.done = true;
            } else {
                // Guard 3: the number of elapsed cycles must be representable
                // by the u32 loop counters. A playhead that is dozens of
                // orders of magnitude past the file end is a broken input, not
                // a long playback: stop instead of wrapping the counter.
                let raw_loops = ((file_now - self.length) / cycle).floor();
                if !(raw_loops.is_finite() && raw_loops >= 0.0 && raw_loops < f64::from(u32::MAX)) {
                    self.done = true;
                } else {
                    let pre_wrap_cursor = self.cursor;
                    let loops = raw_loops as usize + 1;
                    let loops_u32 = loops as u32;
                    self.cycle_start_audio += loops as f64 * cycle / self.speed;
                    file_now -= loops as f64 * cycle;
                    self.start_file = self.start_file.min(self.length);
                    self.cursor = times.partition_point(|t| *t < self.start_file);
                    // Saturate instead of overflowing: u32 counters must never
                    // wrap around (debug builds would panic on overflow).
                    self.loop_count = self.loop_count.saturating_add(loops_u32);
                    restarts = restarts.saturating_add(loops_u32);
                    if self.cursor == pre_wrap_cursor {
                        // The wrap did not rewind the cursor: the cycle tail
                        // holds no events waiting to be dispatched again —
                        // degenerate file, stop.
                        self.done = true;
                    } else if file_now >= self.length {
                        // Guard 4: the fold must have converged below the file
                        // end. Mathematically guaranteed, but a float edge
                        // (e.g. a huge playhead where the subtraction is a
                        // no-op) would otherwise make the next sweep wrap
                        // again forever.
                        self.done = true;
                    } else {
                        // Second sweep: dispatch the events of the new cycle
                        // up to the (now folded) playhead.
                        while self.cursor < times.len() && times[self.cursor] <= file_now {
                            let message = messages[self.cursor];
                            if let MidiMessage::Normal { .. } = message {
                                let audio_instant = self.cycle_start_audio
                                    + (times[self.cursor] - self.start_file) / self.speed;
                                self.due.push(DueEvent {
                                    message,
                                    audio_instant_seconds: audio_instant,
                                });
                            }
                            self.cursor += 1;
                        }
                    }
                }
            }
        }

        PollResult {
            due: &self.due,
            restarts,
            loop_count: self.loop_count,
        }
    }
}

/// Engine-managed per-entity playback state (a [`Component`], internal).
#[derive(Component)]
pub(crate) struct MidiPlaybackState {
    pub(crate) state: PlaybackState,
    pub(crate) last_volume: firewheel::Volume,
    pub(crate) ended_notified: bool,
}

impl MidiPlaybackState {
    pub(crate) fn new(
        midi: Arc<MidiFile>,
        settings: &crate::play::MidiPlaybackSettings,
        now_audio: f64,
    ) -> Self {
        use crate::play::MidiPlaybackMode;
        Self {
            state: PlaybackState::new(
                midi,
                settings.mode == MidiPlaybackMode::Loop,
                settings.start_position.unwrap_or(0.0),
                settings.speed as f64,
                now_audio,
            ),
            last_volume: settings.volume,
            ended_notified: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::MidiEventKind;

    fn file(dur: f64) -> Arc<MidiFile> {
        let events = [
            (
                0.0,
                MidiEventKind::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 100,
                }
                .into_message(),
            ),
            (
                1.0,
                MidiEventKind::NoteOff {
                    channel: 0,
                    key: 60,
                }
                .into_message(),
            ),
            (
                dur,
                MidiEventKind::NoteOn {
                    channel: 0,
                    key: 64,
                    velocity: 100,
                }
                .into_message(),
            ),
        ];
        Arc::new(MidiFile::new_with_events(events).unwrap())
    }

    #[test]
    fn schedules_events_in_window() {
        let mut st = PlaybackState::new(file(2.0), false, 0.0, 1.0, 0.0);
        let result = st.poll(0.4);
        assert_eq!(result.due.len(), 1);
        assert_eq!(result.due[0].audio_instant_seconds, 0.0);
        assert_eq!(result.restarts, 0);

        let result = st.poll(1.5);
        assert_eq!(result.due.len(), 1);
        assert_eq!(result.due[0].audio_instant_seconds, 1.0);
        assert!(!st.is_done());

        let result = st.poll(2.0);
        assert_eq!(result.due.len(), 1);
        assert_eq!(result.due[0].audio_instant_seconds, 2.0);
        assert!(st.is_done());

        let result = st.poll(3.0);
        assert!(result.due.is_empty());
    }

    #[test]
    fn start_position_skips_events() {
        let mut st = PlaybackState::new(file(2.0), false, 1.5, 1.0, 100.0);
        let result = st.poll(100.1);
        assert!(result.due.is_empty());
        assert!(!st.is_done());

        let result = st.poll(102.0);
        assert_eq!(result.due.len(), 1);
        assert_eq!(result.due[0].audio_instant_seconds, 100.5);
    }

    #[test]
    fn loop_wraps_and_keeps_absolute_times() {
        let mut st = PlaybackState::new(file(2.0), true, 0.0, 1.0, 10.0);
        let result = st.poll(10.0); // t=0 → 10.0
        assert_eq!(result.due.len(), 1);
        assert_eq!(result.due[0].audio_instant_seconds, 10.0);

        let result = st.poll(11.0); // t=1 → 11.0
        assert_eq!(result.due.len(), 1);
        assert_eq!(result.due[0].audio_instant_seconds, 11.0);

        // t=2 fires at the loop boundary (12.0), then the next cycle's t=0 is
        // also due at the same instant. The cycle wraps: one restart.
        let result = st.poll(12.0);
        assert_eq!(result.due.len(), 2);
        assert_eq!(result.due[0].audio_instant_seconds, 12.0);
        assert_eq!(result.due[1].audio_instant_seconds, 12.0);
        assert_eq!(result.restarts, 1);
        assert_eq!(result.loop_count, 1);
        assert!(!st.is_done());
        assert_eq!(st.loop_count(), 1);

        let result = st.poll(12.5);
        assert!(result.due.is_empty());
        assert_eq!(result.restarts, 0);

        let result = st.poll(13.0); // next cycle t=1 → 13.0
        assert_eq!(result.due.len(), 1);
        assert_eq!(result.due[0].audio_instant_seconds, 13.0);

        // Next cycle boundary: t=2 plus the following cycle's t=0, both at
        // 14.0; then t=1 at 15.0; then the next boundary pair at 16.0.
        let result = st.poll(14.0);
        assert_eq!(result.due.len(), 2);
        assert_eq!(result.due[0].audio_instant_seconds, 14.0);
        assert_eq!(result.due[1].audio_instant_seconds, 14.0);
        assert_eq!(result.restarts, 1);
        assert_eq!(result.loop_count, 2);

        let result = st.poll(15.0);
        assert_eq!(result.due.len(), 1);
        assert_eq!(result.due[0].audio_instant_seconds, 15.0);

        let result = st.poll(16.0);
        assert_eq!(result.due.len(), 2);
        assert_eq!(result.due[0].audio_instant_seconds, 16.0);
        assert_eq!(result.due[1].audio_instant_seconds, 16.0);
        assert_eq!(result.restarts, 1);
        assert_eq!(result.loop_count, 3);
        assert!(!st.is_done());
    }

    #[test]
    fn pause_freezes_playhead() {
        let mut st = PlaybackState::new(file(2.0), false, 0.0, 1.0, 0.0);
        st.poll(1.0);
        st.set_paused(true, 1.5);
        let result = st.poll(4.5);
        assert!(result.due.is_empty());
        st.set_paused(false, 4.5);
        let result = st.poll(4.6);
        assert!(result.due.is_empty());

        let result = st.poll(5.0);
        assert_eq!(result.due.len(), 1);
        assert_eq!(result.due[0].audio_instant_seconds, 5.0);
        assert!(st.is_done());
    }

    #[test]
    fn speed_changes_fold() {
        let mut st = PlaybackState::new(file(2.0), false, 0.0, 1.0, 0.0);
        st.poll(1.0);
        st.set_speed(2.0, 1.0);
        let result = st.poll(1.4);
        assert!(result.due.is_empty());

        let result = st.poll(1.5);
        assert_eq!(result.due.len(), 1);
        assert_eq!(result.due[0].audio_instant_seconds, 1.5);
        assert!(st.is_done());
    }

    #[test]
    fn loop_restart_counts_accumulate() {
        let mut st = PlaybackState::new(file(2.0), true, 0.0, 1.0, 0.0);
        // Jump far ahead: several full cycles elapse in one poll.
        let result = st.poll(8.5);
        assert_eq!(result.restarts, 4);
        assert_eq!(result.loop_count, 4);
        assert_eq!(st.loop_count(), 4);
        assert!(!st.is_done());

        let result = st.poll(9.0);
        // t=1 of the current cycle is due at 9.0.
        assert_eq!(result.due.len(), 1);
        assert_eq!(result.due[0].audio_instant_seconds, 9.0);
        assert_eq!(result.restarts, 0);
        assert_eq!(result.loop_count, 4);

        // A further single wrap bumps the count by one.
        let result = st.poll(10.5);
        assert_eq!(result.restarts, 1);
        assert_eq!(result.loop_count, 5);
    }

    // --- Termination: no loop cap is needed because pathological inputs are
    // rejected up front (see the `poll` docs for the proof outline). These
    // tests pin the rejection behavior: none of them may hang or panic.

    #[test]
    fn non_finite_clocks_are_rejected() {
        let mut st = PlaybackState::new(file(2.0), true, 0.0, 1.0, 10.0);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let result = st.poll(bad);
            assert!(result.due.is_empty());
            assert_eq!(result.restarts, 0);
            assert_eq!(result.loop_count, 0);
            assert!(!st.is_done(), "rejected poll must not change state");
        }
        // Playback still works afterwards: the rejected polls never moved the
        // cursor, so the events due at/after 10.5 are still pending.
        let result = st.poll(11.5);
        assert_eq!(result.due.len(), 2);
        assert_eq!(result.due[0].audio_instant_seconds, 10.0);
        assert_eq!(result.due[1].audio_instant_seconds, 11.0);
    }

    #[test]
    fn overflowing_playhead_is_rejected() {
        // speed is so large that (now - cycle_start) * speed overflows to Inf.
        let mut st = PlaybackState::new(file(2.0), true, 0.0, f64::MAX, 0.0);
        let result = st.poll(2.0);
        assert!(
            result.due.is_empty(),
            "Inf playhead must not dispatch events"
        );
        assert_eq!(result.restarts, 0);
        assert!(!st.is_done());
        assert_eq!(st.loop_count(), 0);
    }

    #[test]
    fn unrepresentable_loop_count_stops_playback() {
        // file_now = 1e10 s means ~5e9 cycles, far beyond u32::MAX:
        // the counter cannot represent it, so playback stops instead of
        // wrapping the counter (debug builds would panic on overflow).
        let mut st = PlaybackState::new(file(2.0), true, 0.0, 1.0e10, 0.0);
        let result = st.poll(1.0);
        assert_eq!(result.restarts, 0);
        assert!(st.is_done());
        // The terminal state is sticky: later polls stay empty.
        let result = st.poll(1.5);
        assert!(result.due.is_empty());
        assert!(st.is_done());
    }

    #[test]
    fn huge_but_representable_span_folds_in_one_wrap() {
        // file_now = 2e9 s -> 1e9 cycles, representable in u32. A single
        // poll folds all of them at once (no loop, no cap): the first sweep
        // dispatches the whole file (its absolute instants are in the past),
        // one wrap folds the 1e9 cycles, then the second sweep dispatches the
        // new cycle's first event at the folded position.
        let mut st = PlaybackState::new(file(2.0), true, 0.0, 1.0e6, 0.0);
        let result = st.poll(2000.0);
        assert_eq!(result.restarts, 1_000_000_000);
        assert_eq!(result.loop_count, 1_000_000_000);
        assert_eq!(result.due.len(), 4);
        assert_eq!(result.due[0].audio_instant_seconds, 0.0);
        assert_eq!(result.due[3].audio_instant_seconds, 2000.0);
        // `result` borrows `st`; check terminal state after the borrow ends.
        assert!(!st.is_done());
    }

    #[test]
    fn empty_file_ends_immediately() {
        let f = Arc::new(MidiFile::new_with_events::<Vec<(f64, MidiMessage)>>(vec![]).unwrap());
        let mut st = PlaybackState::new(f, true, 0.0, 1.0, 0.0);
        let result = st.poll(0.1);
        assert!(result.due.is_empty());
        let result = st.poll(10.0);
        assert!(result.due.is_empty());
    }
}
#[cfg(test)]
mod generated_file_tests {
    use super::*;
    use std::io::Cursor;

    fn demo_file() -> Option<Arc<MidiFile>> {
        let path = format!("{}/assets/demo_generated.mid", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).ok()?;
        let midi = MidiFile::new(&mut Cursor::new(bytes)).ok()?;
        Some(Arc::new(midi))
    }

    #[test]
    fn generated_demo_file_loops_forever() {
        let Some(midi) = demo_file() else {
            eprintln!(
                "skipping: assets/demo_generated.mid missing (run `cargo xtask generate-demo-midi`)"
            );
            return;
        };
        let length = midi.get_length();
        assert!(length > 0.0, "file has a length");

        let mut st = PlaybackState::new(midi, true, 0.0, 1.0, 1000.0);
        let length = 4.0; // one cycle in seconds (dense arpeggio after the fix)
        let mut total = 0usize;
        let mut after_second_cycle = 0usize;
        // Simulate ~3.5 cycles of polling at 60 Hz.
        for i in 0..((3.5 * length) as usize * 60) {
            let result = st.poll(1000.0 + i as f64 / 60.0);
            total += result.due.len();
            if i as f64 / 60.0 > 2.5 * length {
                after_second_cycle += result.due.len();
            }
        }
        assert!(!st.is_done(), "looping playback must never finish");
        assert!(total > 0, "events were scheduled");
        assert!(
            after_second_cycle > 0,
            "events must still be scheduled after two full cycles — looping is broken"
        );
    }

    #[test]
    fn generated_demo_file_without_loop_finishes() {
        let Some(midi) = demo_file() else {
            eprintln!("skipping: assets/demo_generated.mid missing");
            return;
        };
        eprintln!(
            "file: len={:.3}s events={}",
            midi.get_length(),
            midi.get_times().len()
        );
        let mut st = PlaybackState::new(midi, false, 0.0, 1.0, 1000.0);
        for i in 0..600 {
            let _ = st.poll(1000.0 + i as f64 / 60.0);
        }
        assert!(st.is_done(), "non-looping playback must finish");
    }
}
