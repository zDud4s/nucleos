//! Dictation decisions, kept away from the OS calls that carry them out.
//!
//! The núcleo owns transcription and cleanup (`core/src/voice.rs`, `core/src/transcribe.rs`); the
//! shell owns only the parts that need a desktop: a hotkey, a microphone, and putting text into
//! whatever window the person was typing in.
//!
//! Every subtle decision in that job lives in this module as a pure function, because the three
//! hazards the design named are all of the form "we are about to type into someone else's
//! application" and none of them can be tested through `cpal`, `GetForegroundWindow` or
//! `SendInput`. Those three ways to get it wrong -- pasting after the focus moved, pasting while
//! the hotkey's own modifiers are still down, and restoring a clipboard the user has since
//! changed -- are decisions, not syscalls, and they are decided here and tested here.
//!
//! The platform layer that calls these is deliberately thin: read a window handle, ask whether a
//! key is down, read and write the clipboard, send a keystroke. It holds no rules.

/// Mono 16-bit at this rate is what `core/src/voice.rs` sizes its body limit against and what
/// whisper wants anyway, so the conversion happens here rather than being negotiated per request.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Matches `MAX_CAPTURE_SECONDS` in `core/src/voice.rs`. Duplicated rather than shared because the
/// shell and the núcleo are separate processes with separate manifests; the core refuses anything
/// longer regardless, so this is the earlier and kinder of the two refusals -- it stops a recording
/// that has run away rather than transcribing twenty minutes and then being told no.
pub const MAX_CAPTURE_SECONDS: u32 = 20 * 60;

// ---------------------------------------------------------------------------------------------
// Audio conversion
// ---------------------------------------------------------------------------------------------

/// Averages interleaved frames down to one channel.
///
/// A microphone hands out whatever its device config says, commonly stereo at 44.1 or 48 kHz, and
/// the transcriber wants mono at 16 kHz. Averaging rather than taking the first channel because a
/// laptop's second channel is not silence -- dropping it loses half the signal on hardware that
/// splits the capsule across both.
///
/// A trailing partial frame is discarded: a frame missing channels is not a quieter frame, it is an
/// incomplete one, and averaging it against absent samples would put a click at the end of every
/// recording.
pub fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    let channels = channels as usize;
    let frames = interleaved.len() / channels;
    let mut out = Vec::with_capacity(frames);
    for frame in 0..frames {
        let start = frame * channels;
        let sum: f32 = interleaved[start..start + channels].iter().sum();
        out.push(sum / channels as f32);
    }
    out
}

/// Linear resampling to the transcriber's rate.
///
/// Linear, not windowed-sinc: this is speech heading into a model that was trained on 16 kHz audio,
/// and the aliasing a better filter would remove sits above the band the model uses. A dependency
/// and a thousand lines of DSP would buy nothing measurable here.
///
/// `from_hz == 0` returns empty rather than dividing by it. A device that reports no sample rate is
/// a device we cannot interpret, and inventing a rate would silently pitch-shift the recording.
pub fn resample_linear(input: &[f32], from_hz: u32, to_hz: u32) -> Vec<f32> {
    if input.is_empty() || from_hz == 0 || to_hz == 0 {
        return Vec::new();
    }
    if from_hz == to_hz {
        return input.to_vec();
    }
    // u64 throughout: twenty minutes at 48 kHz is 57.6M samples, and multiplying that by a target
    // rate overflows u32 long before it overflows this.
    let out_len = ((input.len() as u64 * to_hz as u64) / from_hz as u64) as usize;
    if out_len == 0 {
        return Vec::new();
    }
    let ratio = f64::from(from_hz) / f64::from(to_hz);
    let last = input.len() - 1;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let left = pos.floor() as usize;
        let frac = (pos - pos.floor()) as f32;
        let a = input[left.min(last)];
        let b = input[(left + 1).min(last)];
        out.push(a + (b - a) * frac);
    }
    out
}

/// Converts to signed 16-bit, clamping rather than wrapping.
///
/// A sample above 1.0 happens -- input gain, a shout, a device that does not normalise -- and the
/// difference matters: clamping is audible clipping the model copes with, while wrapping turns a
/// loud vowel into full-scale noise of the opposite sign. NaN becomes silence, because a NaN sample
/// has no correct integer and propagating it would poison the surrounding interpolation.
pub fn to_pcm16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|s| {
            if s.is_nan() {
                0
            } else {
                (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
            }
        })
        .collect()
}

/// Wraps PCM in a canonical 44-byte WAV header.
///
/// Hand-written rather than pulled from a crate: the header is four fixed chunks and two computed
/// lengths, and writing it here means the bytes the núcleo receives are covered by a test in the
/// process that produced them.
pub fn wav_bytes(pcm: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    // Everything after this field: the 4-byte "WAVE" tag, the 24-byte fmt chunk, the 8-byte data
    // header, then the samples.
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk length
    out.extend_from_slice(&1u16.to_le_bytes()); // format 1 = uncompressed PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate: rate * channels * 2
    out.extend_from_slice(&2u16.to_le_bytes()); // block align: channels * 2
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in pcm {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// The whole conversion, in the order the hardware forces: interleaved device samples in, the WAV
/// the núcleo accepts out.
pub fn encode_capture(interleaved: &[f32], channels: u16, device_rate: u32) -> Vec<u8> {
    let mono = downmix_to_mono(interleaved, channels);
    let resampled = resample_linear(&mono, device_rate, TARGET_SAMPLE_RATE);
    wav_bytes(&to_pcm16(&resampled), TARGET_SAMPLE_RATE)
}

/// Whether a recording has outrun the cap, measured in device samples so the check can run inside
/// the audio callback without allocating.
pub fn capture_exceeds_cap(frames: u64, device_rate: u32) -> bool {
    if device_rate == 0 {
        return true;
    }
    frames / u64::from(device_rate) >= u64::from(MAX_CAPTURE_SECONDS)
}

// ---------------------------------------------------------------------------------------------
// Hazard 1 -- do not type into a window the person has since left
// ---------------------------------------------------------------------------------------------

/// A window handle, carried as the integer the OS gave us so this module stays testable off-Windows.
pub type WindowId = isize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteDecision {
    /// The same window is still in front; typing into it is what the person asked for.
    Paste,
    /// Focus moved between the hotkey and the transcript. The text goes to the shell instead.
    HoldBecauseFocusMoved,
}

/// Decides whether the transcript may be typed into the foreground window.
///
/// Transcription takes a second or two, which is long enough to alt-tab, and the paste is a
/// synthesised keystroke that goes wherever focus happens to be. Getting this wrong types a
/// sentence into a chat window, a terminal, or a password field -- so the only case that pastes is
/// the one where the window is provably the same one.
///
/// An unknown handle on either side holds. `GetForegroundWindow` returns null when a screen is
/// locked or another desktop is in front, and "we could not tell" must never be treated as "yes":
/// a wrong hold costs one copy-paste, a wrong paste puts text somewhere the person cannot undo.
pub fn decide_paste(
    recorded_into: Option<WindowId>,
    now_focused: Option<WindowId>,
) -> PasteDecision {
    match (recorded_into, now_focused) {
        (Some(then), Some(now)) if then == now => PasteDecision::Paste,
        _ => PasteDecision::HoldBecauseFocusMoved,
    }
}

// ---------------------------------------------------------------------------------------------
// Hazard 2 -- do not paste while the hotkey's own modifiers are still down
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierWait {
    Released,
    StillHeld,
}

/// Waits for the modifiers the hotkey itself used to come back up.
///
/// The hotkey is a chord -- something like Ctrl+Shift+Space -- and the paste is Ctrl+V sent with
/// `SendInput`. If Shift is still physically down when that arrives, the target application does
/// not receive Ctrl+V; it receives Ctrl+Shift+V, which is "paste without formatting" in some
/// editors, "paste and match style" in others, and a completely unrelated command in a terminal.
/// The keystroke has to wait for the person's fingers.
///
/// Polling with a bounded count rather than waiting forever: a stuck modifier -- a key event lost
/// to a remote desktop session, a chord released while another window had focus -- would otherwise
/// hang the dictation with no way out. Exhausting the budget returns `StillHeld`, which the caller
/// treats exactly like a moved focus: keep the text, do not type it.
///
/// The probe runs before the first wait, because the overwhelmingly common case is that the person
/// let go while the model was still working and there is nothing to wait for.
pub fn wait_for_modifier_release(
    mut modifiers_down: impl FnMut() -> bool,
    mut wait: impl FnMut(),
    max_polls: u32,
) -> ModifierWait {
    for _ in 0..max_polls {
        if !modifiers_down() {
            return ModifierWait::Released;
        }
        wait();
    }
    if modifiers_down() {
        ModifierWait::StillHeld
    } else {
        ModifierWait::Released
    }
}

// ---------------------------------------------------------------------------------------------
// Hazard 3 -- do not restore a clipboard the person has since changed
// ---------------------------------------------------------------------------------------------

/// Whether the clipboard we overwrote may be put back.
///
/// Pasting means borrowing the clipboard, and giving it back is the polite thing to do -- but only
/// if it is still ours to give. Between our write and our restore the person may have copied
/// something; if they did, the clipboard now holds *their* text and writing our saved value over it
/// destroys work they can still see themselves having done.
///
/// So the test is not "did we save something" but "is what is there right now exactly what we
/// put there". Anything else -- their copy, an image, a clipboard we cannot read -- means the
/// borrow is over and the saved value is dropped. A clipboard left holding a transcript is a small
/// cost; a clipboard that silently loses what someone just copied is not.
pub fn should_restore_clipboard(what_we_wrote: &str, clipboard_now: Option<&str>) -> bool {
    clipboard_now == Some(what_we_wrote)
}

// ---------------------------------------------------------------------------------------------
// The hotkey's own state machine
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Recording,
    /// The audio is with the núcleo. No new capture may begin until it answers.
    Transcribing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    StartRecording,
    StopAndTranscribe,
    Ignore,
}

/// What a hotkey press means, given where we already are.
///
/// A global shortcut fires on every press, including the repeats a held key generates, and the two
/// presses that start and stop a dictation are the same chord. Without this, holding the hotkey for
/// half a second starts and stops a recording several times, and a press arriving while the model
/// is still working starts a second capture whose result races the first into the same window.
///
/// `Transcribing` therefore absorbs presses instead of queueing them. Dropping a press is
/// recoverable by pressing again; two concurrent dictations typing into one window is not.
pub fn on_hotkey(phase: Phase) -> (Phase, HotkeyAction) {
    match phase {
        Phase::Idle => (Phase::Recording, HotkeyAction::StartRecording),
        Phase::Recording => (Phase::Transcribing, HotkeyAction::StopAndTranscribe),
        Phase::Transcribing => (Phase::Transcribing, HotkeyAction::Ignore),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- audio ---------------------------------------------------------------------------------

    #[test]
    fn stereo_is_averaged_not_halved() {
        // Both channels carry signal; taking channel 0 would report 1.0 and lose the other side.
        assert_eq!(downmix_to_mono(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
    }

    #[test]
    fn mono_passes_through_untouched() {
        assert_eq!(downmix_to_mono(&[0.1, -0.2, 0.3], 1), vec![0.1, -0.2, 0.3]);
    }

    #[test]
    fn a_partial_trailing_frame_is_dropped_rather_than_averaged_against_nothing() {
        // Five samples of stereo is two frames and a stray. Keeping the stray would emit a sample
        // averaged against absent channels -- a click at the end of every recording.
        assert_eq!(
            downmix_to_mono(&[1.0, 1.0, 0.5, 0.5, 0.9], 2),
            vec![1.0, 0.5]
        );
    }

    #[test]
    fn a_device_reporting_no_channels_yields_no_audio_rather_than_dividing_by_zero() {
        assert!(downmix_to_mono(&[1.0, 2.0], 0).is_empty());
    }

    #[test]
    fn resampling_to_the_same_rate_changes_nothing() {
        let input = [0.0, 0.25, -0.5];
        assert_eq!(resample_linear(&input, 16_000, 16_000), input.to_vec());
    }

    #[test]
    fn upsampling_interpolates_between_neighbours() {
        // 2 samples at 8k -> 4 at 16k, sampled at input positions 0, 0.5, 1.0, 1.5. The last two
        // clamp to the final sample: there is nothing beyond it to interpolate towards.
        assert_eq!(
            resample_linear(&[0.0, 1.0], 8_000, 16_000),
            vec![0.0, 0.5, 1.0, 1.0]
        );
    }

    #[test]
    fn downsampling_from_a_real_device_rate_picks_every_third_sample() {
        // 48 kHz is what a laptop actually hands out; 16 kHz is what the transcriber wants.
        let input = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5];
        assert_eq!(resample_linear(&input, 48_000, 16_000), vec![0.0, 0.3]);
    }

    #[test]
    fn a_device_reporting_no_sample_rate_yields_no_audio_rather_than_a_pitch_shift() {
        assert!(resample_linear(&[0.0, 1.0], 0, 16_000).is_empty());
    }

    #[test]
    fn a_sample_over_full_scale_clips_instead_of_wrapping() {
        // The bug this pins: wrapping turns a shout into full-scale noise of the opposite sign,
        // which a transcriber hears as a completely different sound rather than as loudness.
        assert_eq!(to_pcm16(&[2.0, -2.0]), vec![i16::MAX, -i16::MAX]);
    }

    #[test]
    fn a_nan_sample_becomes_silence() {
        assert_eq!(to_pcm16(&[f32::NAN]), vec![0]);
    }

    #[test]
    fn the_wav_header_says_mono_sixteen_bit_at_the_transcribers_rate() {
        let wav = wav_bytes(&[0, -1], TARGET_SAMPLE_RATE);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(
            u16::from_le_bytes([wav[20], wav[21]]),
            1,
            "uncompressed PCM"
        );
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "mono");
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            TARGET_SAMPLE_RATE
        );
        assert_eq!(
            u16::from_le_bytes([wav[34], wav[35]]),
            16,
            "bits per sample"
        );
    }

    #[test]
    fn the_wav_lengths_agree_with_the_bytes_that_follow_them() {
        // A header claiming more data than it carries is the failure that makes a player or a
        // transcriber read past the end, and it is invisible without checking both fields.
        let wav = wav_bytes(&[1, 2, 3], TARGET_SAMPLE_RATE);
        assert_eq!(wav.len(), 44 + 6);
        let riff = u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]);
        assert_eq!(riff as usize, wav.len() - 8);
        let data = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data, 6);
        assert_eq!(i16::from_le_bytes([wav[44], wav[45]]), 1);
    }

    #[test]
    fn an_empty_capture_still_produces_a_valid_header() {
        let wav = wav_bytes(&[], TARGET_SAMPLE_RATE);
        assert_eq!(wav.len(), 44);
        assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), 0);
    }

    #[test]
    fn the_full_conversion_lands_at_sixteen_kilohertz_mono() {
        // One second of 48 kHz stereo -> one second of 16 kHz mono, so 16000 samples of 2 bytes.
        let interleaved = vec![0.25f32; 48_000 * 2];
        let wav = encode_capture(&interleaved, 2, 48_000);
        assert_eq!(wav.len(), 44 + 16_000 * 2);
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            TARGET_SAMPLE_RATE
        );
    }

    #[test]
    fn the_cap_is_reached_at_twenty_minutes_and_not_before() {
        let rate = 48_000;
        let twenty_minutes = u64::from(MAX_CAPTURE_SECONDS) * u64::from(rate);
        assert!(!capture_exceeds_cap(twenty_minutes - rate as u64, rate));
        assert!(capture_exceeds_cap(twenty_minutes, rate));
    }

    #[test]
    fn a_device_with_no_rate_is_treated_as_already_over_the_cap() {
        // Refusing beats recording forever into a buffer whose duration we cannot compute.
        assert!(capture_exceeds_cap(1, 0));
    }

    // -- hazard 1: focus ------------------------------------------------------------------------

    #[test]
    fn the_same_window_is_pasted_into() {
        assert_eq!(decide_paste(Some(42), Some(42)), PasteDecision::Paste);
    }

    #[test]
    fn a_window_change_between_hotkey_and_transcript_holds_the_paste() {
        // Two seconds is enough to alt-tab, and the synthesised keystroke goes wherever focus is.
        assert_eq!(
            decide_paste(Some(42), Some(99)),
            PasteDecision::HoldBecauseFocusMoved
        );
    }

    #[test]
    fn an_unknowable_focus_holds_rather_than_guessing() {
        // GetForegroundWindow returns null on a locked screen. "We cannot tell" is not "yes".
        assert_eq!(
            decide_paste(Some(42), None),
            PasteDecision::HoldBecauseFocusMoved
        );
        assert_eq!(
            decide_paste(None, Some(42)),
            PasteDecision::HoldBecauseFocusMoved
        );
        assert_eq!(
            decide_paste(None, None),
            PasteDecision::HoldBecauseFocusMoved
        );
    }

    // -- hazard 2: modifiers --------------------------------------------------------------------

    #[test]
    fn an_already_released_chord_is_not_waited_on() {
        let mut waits = 0;
        let outcome = wait_for_modifier_release(|| false, || waits += 1, 50);
        assert_eq!(outcome, ModifierWait::Released);
        assert_eq!(waits, 0, "the common case must not cost a single sleep");
    }

    #[test]
    fn a_chord_held_briefly_is_waited_out() {
        let mut polls = 0;
        let mut waits = 0;
        let outcome = wait_for_modifier_release(
            || {
                polls += 1;
                polls <= 3
            },
            || waits += 1,
            50,
        );
        assert_eq!(outcome, ModifierWait::Released);
        assert_eq!(waits, 3);
    }

    #[test]
    fn a_stuck_modifier_gives_up_instead_of_hanging_the_dictation() {
        // A key event lost to a remote desktop session leaves Shift down forever. Waiting forever
        // would strand the transcript with no way to get it back.
        let mut waits = 0;
        let outcome = wait_for_modifier_release(|| true, || waits += 1, 5);
        assert_eq!(outcome, ModifierWait::StillHeld);
        assert_eq!(waits, 5, "the budget is spent, not exceeded");
    }

    #[test]
    fn a_zero_poll_budget_still_answers_from_the_current_state() {
        assert_eq!(
            wait_for_modifier_release(|| false, || {}, 0),
            ModifierWait::Released
        );
        assert_eq!(
            wait_for_modifier_release(|| true, || {}, 0),
            ModifierWait::StillHeld
        );
    }

    // -- hazard 3: clipboard --------------------------------------------------------------------

    #[test]
    fn an_untouched_clipboard_is_handed_back() {
        assert!(should_restore_clipboard("hello", Some("hello")));
    }

    #[test]
    fn a_clipboard_the_person_has_since_used_is_left_alone() {
        // The bug this pins: restoring here destroys something they just copied and can see
        // themselves having copied.
        assert!(!should_restore_clipboard("hello", Some("their own copy")));
    }

    #[test]
    fn an_unreadable_clipboard_is_left_alone() {
        // An image, or a clipboard locked by another process. Either way the borrow is over.
        assert!(!should_restore_clipboard("hello", None));
    }

    // -- the state machine ----------------------------------------------------------------------

    #[test]
    fn the_first_press_records_and_the_second_transcribes() {
        let (phase, action) = on_hotkey(Phase::Idle);
        assert_eq!(
            (phase, action),
            (Phase::Recording, HotkeyAction::StartRecording)
        );
        let (phase, action) = on_hotkey(phase);
        assert_eq!(
            (phase, action),
            (Phase::Transcribing, HotkeyAction::StopAndTranscribe)
        );
    }

    #[test]
    fn a_press_while_the_model_is_working_is_absorbed() {
        // Key repeat and impatience both produce this. Two dictations typing into one window is
        // the outcome being prevented; a dropped press only costs another press.
        assert_eq!(
            on_hotkey(Phase::Transcribing),
            (Phase::Transcribing, HotkeyAction::Ignore)
        );
    }
}
