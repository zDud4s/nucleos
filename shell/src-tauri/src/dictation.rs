//! Where a dictation actually happens: the OS calls, and the order they go in.
//!
//! This module holds no rules. Every decision it acts on comes from `voice`, which is pure and
//! tested; what lives here is the part that cannot be tested without a desktop — a global hotkey, a
//! window handle, the keyboard state, the clipboard, and a synthesised keystroke.
//!
//! The microphone and the HTTP call are deliberately NOT here. `cpal` cannot currently be built
//! alongside Tauri (it compiles against `windows-core` 0.61 while `tauri` 2.11 requires 0.62, and its
//! `#[implement]` macros resolve to the wrong one), so capture happens in the webview — and once the
//! samples exist there they cannot affordably be handed over, because Tauri's IPC serialises arguments
//! as JSON. So the webview captures, encodes and POSTs, and calls `voice_paste` with the transcript.
//! That division turned out to be the better shape anyway: the browser owns device permission, and
//! this process keeps everything that has to reach into another application.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::voice::{
    decide_paste, on_hotkey, should_restore_clipboard, wait_for_modifier_release, HotkeyAction,
    ModifierWait, PasteDecision, Phase, WindowId,
};

/// How long to wait for the hotkey's own modifiers to come up, as polls of `MODIFIER_POLL`.
///
/// Two seconds in total. Long enough for someone still lifting their fingers as the transcript lands,
/// short enough that a modifier stuck by a lost key event does not hold the text hostage.
const MODIFIER_POLLS: u32 = 100;
const MODIFIER_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// How long to let the target application read our clipboard before offering the old one back.
///
/// The paste returns as soon as the keystroke is queued, but the receiving application reads the
/// clipboard on its own schedule. Restoring immediately hands the previous value to something that has
/// not read ours yet, which pastes the wrong text — and the failure looks like dictation being ignored.
const CLIPBOARD_SETTLE: std::time::Duration = std::time::Duration::from_millis(120);

/// Emitted to the webview so it knows to open or close the microphone. The hotkey is registered in
/// this process; the device lives in the other one.
const START_EVENT: &str = "voice://start";
const STOP_EVENT: &str = "voice://stop";

/// What the shell is doing right now, and which window it promised the text to.
#[derive(Default)]
pub struct Dictation {
    inner: Mutex<Inner>,
}

/// `recorded_into` is captured the moment the hotkey fires and never refreshed. That is the whole
/// point: it is the evidence `decide_paste` compares against later, and refreshing it would make the
/// comparison always succeed.
#[derive(Default)]
struct Inner {
    phase: Phase,
    recorded_into: Option<WindowId>,
    kind: Kind,
}

/// Which of the two hotkeys started this. Serialised into the event the webview reads, and from there
/// into the núcleo's `kind` query parameter, so the spellings have to match `core/src/voice.rs`'s
/// lowercase enum and the migration's CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Dictation,
    Memo,
}

/// What became of a transcript this process was asked to type.
#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    /// Whether the text was actually typed into the window that was in front.
    pub pasted: bool,
    /// Why it was not, when it was not. `None` when it was pasted.
    pub held: Option<&'static str>,
}

// -------------------------------------------------------------------------------------------------
// The Windows shim. Three calls, no decisions.
// -------------------------------------------------------------------------------------------------

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    /// `V`. The Win32 headers define no `VK_V` — the letter keys are their ASCII codes — so the
    /// literal is the canonical spelling rather than a magic number.
    const VK_V: VIRTUAL_KEY = 0x56;

    /// The window in front, or `None` when there isn't one we can name.
    ///
    /// A null handle is a real answer, not an error: it is what a locked screen or another desktop
    /// returns. `voice::decide_paste` treats `None` as "do not type", which is why this reports it
    /// rather than substituting a zero.
    pub fn foreground_window() -> Option<isize> {
        let handle = unsafe { GetForegroundWindow() };
        if handle.is_null() {
            None
        } else {
            Some(handle as isize)
        }
    }

    /// Whether any key a hotkey chord can use is still physically down.
    ///
    /// Both Win keys are included even though no default chord uses one, because the hotkey is
    /// configurable and a chord we did not anticipate must not be able to leak into the paste.
    pub fn modifiers_down() -> bool {
        const CHORD: [VIRTUAL_KEY; 5] = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN];
        CHORD.iter().any(|key| {
            // The HIGH bit means "down right now". The low bit means "was pressed since the last
            // call", a different question that would report true for a chord already released.
            (unsafe { GetAsyncKeyState(i32::from(*key)) } as u16 & 0x8000) != 0
        })
    }

    /// Sends Ctrl+V as four events in one call.
    ///
    /// One `SendInput` rather than four: the call is atomic with respect to other threads' input, so
    /// nothing can interleave a keystroke between our Ctrl-down and our V.
    pub fn send_paste() -> bool {
        fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: 0,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }
        let events = [
            key(VK_CONTROL, false),
            key(VK_V, false),
            key(VK_V, true),
            key(VK_CONTROL, true),
        ];
        let sent = unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        sent as usize == events.len()
    }
}

/// Off-Windows the shim reports "cannot tell", which every decision already treats as "do not type".
/// The shell targets Windows (spec §2); this exists so the crate builds and tests elsewhere.
#[cfg(not(windows))]
mod platform {
    pub fn foreground_window() -> Option<isize> {
        None
    }
    pub fn modifiers_down() -> bool {
        true
    }
    pub fn send_paste() -> bool {
        false
    }
}

fn clipboard_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

fn set_clipboard_text(text: &str) -> bool {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text.to_string()))
        .is_ok()
}

// -------------------------------------------------------------------------------------------------
// Commands
// -------------------------------------------------------------------------------------------------

/// Called by the hotkey handler, and by the tab's own buttons so dictation works without a hotkey.
///
/// Returns what the caller should now be doing, so nothing outside this module has to know the state
/// machine.
#[tauri::command]
pub fn voice_hotkey(
    app: AppHandle,
    state: State<'_, Dictation>,
    memo: bool,
) -> Result<&'static str, String> {
    let mut inner = state.inner.lock().map_err(|_| poisoned())?;
    let (next, action) = on_hotkey(inner.phase);
    inner.phase = next;

    match action {
        HotkeyAction::StartRecording => {
            // Captured BEFORE the recording and never refreshed — this is the evidence
            // `decide_paste` will compare against when the transcript comes back.
            inner.recorded_into = platform::foreground_window();
            inner.kind = if memo { Kind::Memo } else { Kind::Dictation };
            let kind = inner.kind;
            drop(inner);
            app.emit(START_EVENT, kind).map_err(|e| e.to_string())?;
            Ok("recording")
        }
        HotkeyAction::StopAndTranscribe => {
            drop(inner);
            app.emit(STOP_EVENT, ()).map_err(|e| e.to_string())?;
            Ok("transcribing")
        }
        // A press while the núcleo is still working. Absorbed rather than queued: two dictations
        // typing into one window is the outcome being prevented, and a dropped press costs a press.
        HotkeyAction::Ignore => Ok("busy"),
    }
}

/// What phase the shell is in, so the tab can show it without keeping a second copy.
#[tauri::command]
pub fn voice_phase(state: State<'_, Dictation>) -> Result<&'static str, String> {
    let inner = state.inner.lock().map_err(|_| poisoned())?;
    Ok(match inner.phase {
        Phase::Idle => "idle",
        Phase::Recording => "recording",
        Phase::Transcribing => "transcribing",
    })
}

/// Types a transcript into the window the dictation started in, or explains why it did not.
///
/// The three refusals are `voice`'s, not this function's. What is here is only the order: check the
/// window, wait for the chord, borrow the clipboard, paste, and give the clipboard back only if it is
/// still ours to give.
#[tauri::command]
pub fn voice_paste(state: State<'_, Dictation>, text: String) -> Result<Delivery, String> {
    let recorded_into = {
        let mut inner = state.inner.lock().map_err(|_| poisoned())?;
        // Released here rather than at the end: pasting takes up to two seconds of waiting for a
        // chord, and holding the phase across that would absorb every press made in the meantime.
        inner.phase = Phase::Idle;
        inner.recorded_into.take()
    };
    Ok(deliver(&text, recorded_into))
}

/// Puts the shell back to idle when the webview could not finish — a refused capture, an unreachable
/// daemon, a microphone the person declined.
///
/// Without this the phase stays `Transcribing`, which absorbs every later press: the feature would
/// simply stop responding, with nothing on screen saying why.
#[tauri::command]
pub fn voice_abandon(state: State<'_, Dictation>) -> Result<(), String> {
    let mut inner = state.inner.lock().map_err(|_| poisoned())?;
    inner.phase = Phase::Idle;
    inner.recorded_into = None;
    Ok(())
}

/// Called by the Voice tab once it has read `GET /voice/config`, because that is the only place the
/// configured chords exist — `.ai/voice.yaml` is self-governing and the shell may not read it.
/// Re-registering replaces what was there, so editing the config and reloading the tab is enough.
#[tauri::command]
pub fn voice_register_hotkeys(app: AppHandle, dictation: String, memo: String) -> Vec<String> {
    register_hotkeys(&app, &dictation, &memo)
}

fn poisoned() -> String {
    "the shell's dictation state is poisoned".to_string()
}

fn deliver(text: &str, recorded_into: Option<WindowId>) -> Delivery {
    let held = |reason| Delivery {
        pasted: false,
        held: Some(reason),
    };

    if decide_paste(recorded_into, platform::foreground_window()) != PasteDecision::Paste {
        return held("the window you dictated into is no longer in front");
    }
    if wait_for_modifier_release(
        platform::modifiers_down,
        || std::thread::sleep(MODIFIER_POLL),
        MODIFIER_POLLS,
    ) != ModifierWait::Released
    {
        return held("a modifier key is still held");
    }

    // Saved before ours goes in, so there is something to give back. `None` is fine: it means there
    // was nothing readable to restore, not that the paste cannot happen.
    let borrowed = clipboard_text();
    if !set_clipboard_text(text) {
        return held("the clipboard could not be written");
    }
    // Re-checked because waiting for the chord took real time, and this is the last moment before a
    // keystroke leaves this process.
    if decide_paste(recorded_into, platform::foreground_window()) != PasteDecision::Paste {
        restore(borrowed, text);
        return held("focus moved while waiting for the modifier keys");
    }
    let sent = platform::send_paste();
    restore(borrowed, text);
    if sent {
        Delivery {
            pasted: true,
            held: None,
        }
    } else {
        held("the keystroke could not be sent")
    }
}

/// Gives the clipboard back, but only when what is in it is still exactly what we put there.
fn restore(borrowed: Option<String>, ours: &str) {
    let Some(previous) = borrowed else { return };
    std::thread::sleep(CLIPBOARD_SETTLE);
    if should_restore_clipboard(ours, clipboard_text().as_deref()) {
        set_clipboard_text(&previous);
    }
}

/// Registers both chords, treating a refusal as information rather than as a failure to start.
///
/// A chord already taken by another application cannot be registered, and that is common — the shell
/// must still run, with the tab able to say which one did not take. Dictation stays reachable from the
/// tab's own buttons, so a collision costs convenience and not the feature.
fn register_hotkeys(app: &AppHandle, dictation: &str, memo: &str) -> Vec<String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    // Whatever was registered before is dropped first: without this, changing a chord in the config
    // leaves the old one live and the shell answers to both.
    let _ = app.global_shortcut().unregister_all();

    let mut refused = Vec::new();
    for (chord, is_memo) in [(dictation, false), (memo, true)] {
        if chord.trim().is_empty() {
            continue;
        }
        let handle = app.clone();
        let registered = app
            .global_shortcut()
            .on_shortcut(chord, move |_, _, event| {
                // Pressed only. A chord reports both press and release, and acting on both would
                // start and immediately stop every recording.
                if event.state() != tauri_plugin_global_shortcut::ShortcutState::Pressed {
                    return;
                }
                let app = handle.clone();
                let state = app.state::<Dictation>();
                if let Err(error) = voice_hotkey(app.clone(), state, is_memo) {
                    eprintln!("[voice] hotkey failed: {error}");
                }
            });
        if registered.is_err() {
            refused.push(chord.to_string());
        }
    }
    refused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kind_spellings_match_the_nucleos_enum() {
        // `core/src/voice.rs` declares `Kind` with `rename_all = "lowercase"` and migration 0036's
        // CHECK constraint accepts exactly these two. A mismatch is a runtime refusal and nothing at
        // compile time, because the value travels as a query string.
        assert_eq!(
            serde_json::to_string(&Kind::Dictation).unwrap(),
            "\"dictation\""
        );
        assert_eq!(serde_json::to_string(&Kind::Memo).unwrap(), "\"memo\"");
    }

    #[test]
    fn a_fresh_state_holds_no_window_so_nothing_can_be_pasted_into_by_default() {
        let inner = Inner::default();
        assert_eq!(inner.phase, Phase::Idle);
        assert_eq!(inner.recorded_into, None);
        // The pairing that matters: with no remembered window, delivery must refuse rather than type
        // into whatever happens to be in front.
        assert!(!deliver("some text", inner.recorded_into).pasted);
    }

    /// Off-Windows every shim answer has to be the one that refuses to type.
    #[cfg(not(windows))]
    #[test]
    fn the_non_windows_shim_never_types() {
        assert_eq!(platform::foreground_window(), None);
        assert!(platform::modifiers_down());
        assert!(!platform::send_paste());
    }
}
