//! Dictation decisions, kept away from the OS calls that carry them out.
//!
//! The núcleo owns transcription and cleanup (`core/src/voice.rs`, `core/src/transcribe.rs`); the
//! webview owns the microphone and the WAV encoding (`shell/src/audio.ts`, for the reason recorded
//! there); and this process owns the one thing neither of them can do — reaching into whatever
//! application the person was typing in.
//!
//! Every subtle decision in that job lives here as a pure function, because the three hazards the
//! design named are all of the form "we are about to type into someone else's application" and none
//! of them can be tested through `GetForegroundWindow`, `GetAsyncKeyState` or `SendInput`. Those three
//! ways to get it wrong -- pasting after the focus moved, pasting while the hotkey's own modifiers are
//! still down, and restoring a clipboard the user has since changed -- are decisions, not syscalls,
//! and they are decided here and tested here.
//!
//! The platform layer in `dictation.rs` holds no rules.

// -------------------------------------------------------------------------------------------------
// Hazard 1 -- do not type into a window the person has since left
// -------------------------------------------------------------------------------------------------

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
/// synthesised keystroke that goes wherever focus happens to be. Getting this wrong types a sentence
/// into a chat window, a terminal, or a password field -- so the only case that pastes is the one
/// where the window is provably the same one.
///
/// An unknown handle on either side holds. `GetForegroundWindow` returns null when a screen is locked
/// or another desktop is in front, and "we could not tell" must never be treated as "yes": a wrong
/// hold costs one copy-paste, a wrong paste puts text somewhere the person cannot undo.
pub fn decide_paste(
    recorded_into: Option<WindowId>,
    now_focused: Option<WindowId>,
) -> PasteDecision {
    match (recorded_into, now_focused) {
        (Some(then), Some(now)) if then == now => PasteDecision::Paste,
        _ => PasteDecision::HoldBecauseFocusMoved,
    }
}

// -------------------------------------------------------------------------------------------------
// Hazard 2 -- do not paste while the hotkey's own modifiers are still down
// -------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierWait {
    Released,
    StillHeld,
}

/// Waits for the modifiers the hotkey itself used to come back up.
///
/// The hotkey is a chord -- something like Ctrl+Alt+Space -- and the paste is Ctrl+V sent with
/// `SendInput`. If Alt is still physically down when that arrives, the target application does not
/// receive Ctrl+V; it receives Ctrl+Alt+V, which is a different command in one editor and nothing at
/// all in another. The keystroke has to wait for the person's fingers.
///
/// Polling with a bounded count rather than waiting forever: a stuck modifier -- a key event lost to a
/// remote desktop session, a chord released while another window had focus -- would otherwise hang the
/// dictation with no way out. Exhausting the budget returns `StillHeld`, which the caller treats
/// exactly like a moved focus: keep the text, do not type it.
///
/// The probe runs before the first wait, because the overwhelmingly common case is that the person let
/// go while the model was still working and there is nothing to wait for.
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

// -------------------------------------------------------------------------------------------------
// Hazard 3 -- do not restore a clipboard the person has since changed
// -------------------------------------------------------------------------------------------------

/// Whether the clipboard we overwrote may be put back.
///
/// Pasting means borrowing the clipboard, and giving it back is the polite thing to do -- but only if
/// it is still ours to give. Between our write and our restore the person may have copied something;
/// if they did, the clipboard now holds *their* text and writing our saved value over it destroys work
/// they can still see themselves having done.
///
/// So the test is not "did we save something" but "is what is there right now exactly what we put
/// there". Anything else -- their copy, an image, a clipboard we cannot read -- means the borrow is
/// over and the saved value is dropped. A clipboard left holding a transcript is a small cost; a
/// clipboard that silently loses what someone just copied is not.
pub fn should_restore_clipboard(what_we_wrote: &str, clipboard_now: Option<&str>) -> bool {
    clipboard_now == Some(what_we_wrote)
}

// -------------------------------------------------------------------------------------------------
// The hotkey's own state machine
// -------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// A fresh shell must accept the very first press, so this is the default rather than a state that
    /// would absorb it.
    #[default]
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
/// half a second starts and stops a recording several times, and a press arriving while the model is
/// still working starts a second capture whose result races the first into the same window.
///
/// `Transcribing` therefore absorbs presses instead of queueing them. Dropping a press is recoverable
/// by pressing again; two concurrent dictations typing into one window is not.
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
        // A key event lost to a remote desktop session leaves a modifier down forever. Waiting
        // forever would strand the transcript with no way to get it back.
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
        // Key repeat and impatience both produce this. Two dictations typing into one window is the
        // outcome being prevented; a dropped press only costs another press.
        assert_eq!(
            on_hotkey(Phase::Transcribing),
            (Phase::Transcribing, HotkeyAction::Ignore)
        );
    }

    #[test]
    fn a_fresh_shell_starts_idle_so_it_accepts_the_first_press() {
        assert_eq!(Phase::default(), Phase::Idle);
    }
}
