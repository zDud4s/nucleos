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
/// Emitted by the third chord, and carrying nothing.
///
/// The asymmetry with the other two is the design, not an omission. A dictation is a recording this
/// process drives — it captured a window handle and it will paste into it — so its state lives here.
/// A conversation is a MODE, and everything the mode does happens in the webview: the gate that hears
/// speech, the queue that plays the answer, and the decision to stop that queue when somebody cuts in.
/// `shell/src/lib/conversation.ts` says why that decision cannot afford to cross this boundary.
///
/// So this process contributes the one thing the webview cannot have — a chord that works when the
/// window is not focused — and gets out of the way.
const CONVERSATION_EVENT: &str = "voice://conversation-toggle";

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
// Which display server is in front of us, and what to say when it is one that cannot be typed into.
// -------------------------------------------------------------------------------------------------

/// Offered to the person when the desktop itself forbids the paste, and read verbatim by
/// `shell/src/components/Voice.tsx`. Wayland is a design decision, not a fault: no client may
/// synthesise input into another client's window, so the text goes to the clipboard and the
/// sentence says where it went and what to do about it.
#[cfg(target_os = "linux")]
const WAYLAND_HELD: &str = "this desktop runs Wayland, which lets no application type into another; the text is on the clipboard, paste it yourself";

/// Offered to the person when the desktop gives no application a system-wide key grab, and read
/// verbatim by `shell/src/pages/Voice.tsx`. Sibling of `WAYLAND_HELD` above and the same design
/// decision seen from the other end: a Wayland compositor hands an ordinary client no global
/// hotkey, so the chords would "register" and then never fire. Saying so is the whole value - a
/// hotkey that never arrives is indistinguishable from the feature being broken.
///
/// Compiled in test builds on every host, so the decision below can be tested anywhere.
#[cfg(any(target_os = "linux", test))]
const NO_GLOBAL_HOTKEYS: &str =
    "this desktop runs Wayland, which gives no application global hotkeys; use the buttons below";

/// Which display server this session runs, as far as the environment is willing to say.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    Wayland,
    X11,
    Unknown,
}

/// Reads the session out of the three variables that carry it, in the only order that is safe.
///
/// A Wayland claim beats a `DISPLAY`, and that ordering is the whole reason this is a function
/// rather than two inline conditions: XWayland exports `DISPLAY` too, so "there is a DISPLAY,
/// therefore X11" reads a Wayland desktop as X11 and then synthesises a keystroke into a
/// compositor that permits no such thing. Nothing fails loudly - the events are simply never
/// delivered, which on screen is indistinguishable from dictation being ignored.
///
/// An empty string is absent, not present-and-blank. An exported-but-cleared `WAYLAND_DISPLAY` is
/// a variable somebody unset badly, and reading it as a compositor would refuse to type on a
/// desktop that could have been typed into - the opposite mistake, and just as invisible. By the
/// same rule a session type naming X11 with nothing behind it names no server anything can reach,
/// so that is `Unknown` rather than `X11`.
#[cfg(any(target_os = "linux", test))]
fn session_from(
    wayland_display: Option<&str>,
    xdg_session_type: Option<&str>,
    display: Option<&str>,
) -> Session {
    fn present(value: Option<&str>) -> Option<&str> {
        value.filter(|value| !value.is_empty())
    }

    if present(wayland_display).is_some() || present(xdg_session_type) == Some("wayland") {
        Session::Wayland
    } else if present(display).is_some() {
        Session::X11
    } else {
        Session::Unknown
    }
}

/// Whether this session has global hotkeys to give, and the sentence to show when it has not.
///
/// `None` means "no reason to refuse" and not "no desktop": X11 delivers the grabs, and a session
/// nothing could identify is an environment that arrived incomplete rather than a compositor that
/// forbids them - refusing there would take working chords away from an X11 desktop. Only Wayland
/// is answered with a sentence, because only Wayland is known to accept the registration and then
/// deliver nothing.
///
/// Pure, and compiled in test builds everywhere, so the rule is tested on the host that runs the
/// gate rather than only on the one platform it governs.
#[cfg(any(target_os = "linux", test))]
fn hotkeys_unavailable_for(session: Session) -> Option<&'static str> {
    match session {
        Session::Wayland => Some(NO_GLOBAL_HOTKEYS),
        Session::X11 | Session::Unknown => None,
    }
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

    /// Nothing about Windows forbids one process typing into another's window, so there is never a
    /// reason to hold the text. A function rather than a constant so that every platform answers
    /// the same question, and so `deliver` needs no `cfg` of its own.
    pub fn paste_unsupported() -> Option<&'static str> {
        None
    }
}

// -------------------------------------------------------------------------------------------------
// The Linux shim. The X11 protocol spoken directly, and an honest refusal on Wayland.
// -------------------------------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod platform {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt as _, KeyButMask, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
    };
    use x11rb::protocol::xtest::ConnectionExt as _;
    use x11rb::wrapper::ConnectionExt as _;

    use super::{session_from, Session, WAYLAND_HELD};

    /// `Control_L` and lowercase `v`, as keysyms. A keysym is what a key MEANS; XTest wants a
    /// keycode, which is where on this particular keyboard that meaning currently sits - so the
    /// mapping is read at paste time rather than hardcoded, or a Dvorak layout pastes whatever key
    /// happens to sit where QWERTY keeps its V.
    const CONTROL_L: u32 = 0xffe3;
    const LOWERCASE_V: u32 = 0x0076;

    /// The session, read from this process's own environment.
    fn session() -> Session {
        let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
        let xdg_session_type = std::env::var("XDG_SESSION_TYPE").ok();
        let display = std::env::var("DISPLAY").ok();
        session_from(
            wayland_display.as_deref(),
            xdg_session_type.as_deref(),
            display.as_deref(),
        )
    }

    /// The window the window manager says is active, or `None` when there is no answer to trust.
    ///
    /// `_NET_ACTIVE_WINDOW` on the root window is the EWMH answer, and it is the window manager's
    /// rather than the server's - a desktop running no EWMH-compliant WM simply has no such
    /// property, which is a real answer and not an error. Anything that is not a live X11 session
    /// gets `None` too, because `voice::decide_paste` already treats `None` as "do not type".
    pub fn foreground_window() -> Option<isize> {
        if session() != Session::X11 {
            return None;
        }
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        // `only_if_exists`: asking for the atom must not create it. A zero back means no window
        // manager ever set the property, so there is nothing to read.
        let atom = conn
            .intern_atom(true, b"_NET_ACTIVE_WINDOW")
            .ok()?
            .reply()
            .ok()?
            .atom;
        if atom == x11rb::NONE {
            return None;
        }
        let reply = conn
            .get_property(false, root, atom, AtomEnum::WINDOW, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        let window = reply.value32()?.next()?;
        if window == 0 {
            None
        } else {
            Some(window as isize)
        }
    }

    /// Whether any key a hotkey chord can use is still physically down.
    ///
    /// `true` on every failure, and that is the point: the honest answer to "is a modifier down"
    /// when the X server cannot be reached is "cannot tell", and this module already treats
    /// "cannot tell" as "do not type". Reporting `false` would let the paste go ahead into a
    /// window whose state nobody has read.
    ///
    /// The four bits are X11's own names for the chord keys: `MOD1` is Alt on every layout in
    /// practice and `MOD4` is Super. X reports the modifier and not the key, so there is no
    /// separate left/right bit - this is the Windows shim's five virtual keys in four masks.
    pub fn modifiers_down() -> bool {
        let chord = KeyButMask::SHIFT | KeyButMask::CONTROL | KeyButMask::MOD1 | KeyButMask::MOD4;
        let Ok((conn, screen)) = x11rb::connect(None) else {
            return true;
        };
        let Some(root) = conn.setup().roots.get(screen).map(|screen| screen.root) else {
            return true;
        };
        let Ok(cookie) = conn.query_pointer(root) else {
            return true;
        };
        let Ok(reply) = cookie.reply() else {
            return true;
        };
        u16::from(reply.mask & chord) != 0
    }

    /// Sends Ctrl+V through the XTest extension.
    ///
    /// XTest rather than `send_event`, because an event delivered by `send_event` is flagged as
    /// synthetic and most toolkits drop it; XTest injects at the server's own input queue, where it
    /// is indistinguishable from a keypress. The four events go as four requests - X has no atomic
    /// batch - then `flush` and `sync`, and `sync` is what turns a request the server refused into
    /// an error this function can see rather than a silent nothing.
    pub fn send_paste() -> bool {
        let Ok((conn, screen)) = x11rb::connect(None) else {
            return false;
        };
        let Some(root) = conn.setup().roots.get(screen).map(|screen| screen.root) else {
            return false;
        };
        let (Some(control), Some(v)) = (
            keycode_for(&conn, CONTROL_L),
            keycode_for(&conn, LOWERCASE_V),
        ) else {
            return false;
        };
        let events = [
            (KEY_PRESS_EVENT, control),
            (KEY_PRESS_EVENT, v),
            (KEY_RELEASE_EVENT, v),
            (KEY_RELEASE_EVENT, control),
        ];
        for (kind, keycode) in events {
            if conn
                .xtest_fake_input(kind, keycode, 0, root, 0, 0, 0)
                .is_err()
            {
                return false;
            }
        }
        conn.flush().is_ok() && conn.sync().is_ok()
    }

    /// Why the text cannot be typed on this desktop, when it cannot.
    ///
    /// Wayland is the only such desktop here, and the refusal is by design rather than by bug: a
    /// compositor delivers input to the focused client and to nobody else, so there is no call this
    /// process could make. `None` everywhere else, a session nobody could identify included -
    /// `foreground_window` refuses that one on its own, with the reason the rest of the module
    /// already uses.
    pub fn paste_unsupported() -> Option<&'static str> {
        if session() == Session::Wayland {
            Some(WAYLAND_HELD)
        } else {
            None
        }
    }

    /// Where this keyboard currently keeps a given keysym, or `None` when it keeps it nowhere.
    ///
    /// The server reports the whole table at once - `keysyms_per_keycode` entries per key, from
    /// `min_keycode` up - so the position of the first chunk holding the keysym, offset by
    /// `min_keycode`, is the keycode.
    fn keycode_for(conn: &impl Connection, keysym: u32) -> Option<u8> {
        let (min, max) = {
            let setup = conn.setup();
            (setup.min_keycode, setup.max_keycode)
        };
        let count = max.checked_sub(min)?.checked_add(1)?;
        let mapping = conn.get_keyboard_mapping(min, count).ok()?.reply().ok()?;
        let per_keycode = usize::from(mapping.keysyms_per_keycode);
        if per_keycode == 0 {
            return None;
        }
        let index = mapping
            .keysyms
            .chunks(per_keycode)
            .position(|keysyms| keysyms.contains(&keysym))?;
        u8::try_from(usize::from(min) + index).ok()
    }
}

// -------------------------------------------------------------------------------------------------
// The macOS shim. Quartz events, and an honest refusal until Accessibility has been granted.
// -------------------------------------------------------------------------------------------------

/// Offered to the person when macOS has not granted this application Accessibility access, and read
/// verbatim by `shell/src/pages/Voice.tsx`. Like the Wayland refusal this is a design decision and
/// not a fault: synthesising input into another application is exactly the power macOS asks the
/// person to hand over explicitly, so until they have, the text goes to the clipboard and the
/// sentence says where it went and where to turn the permission on.
#[cfg(target_os = "macos")]
const ACCESSIBILITY_HELD: &str = "macOS has not given NucleOS Accessibility access, so it cannot type; the text is on the clipboard. Allow it in System Settings > Privacy & Security > Accessibility";

/// Whether the flags Quartz reports include any key a hotkey chord can use.
///
/// A function rather than an inline mask because it is the one macOS decision that can be checked
/// without a Mac: `CGEventSourceFlagsState` cannot be called here, but the bits it returns are
/// documented constants, so the READING of them is tested on every platform.
///
/// The four are `CGEventFlags`' own names: `CGEventFlagShift` 0x20000, `CGEventFlagControl`
/// 0x40000, `CGEventFlagAlternate` 0x80000 (the Option key) and `CGEventFlagCommand` 0x100000.
/// Everything else the field carries is state nobody is holding down - `CGEventFlagAlphaShift` is
/// caps lock being ON, `CGEventFlagNumericPad` is which part of the keyboard a key came from - and
/// reading one of those as a held modifier would refuse every paste forever, which on screen looks
/// exactly like dictation being ignored.
#[cfg(any(target_os = "macos", test))]
fn flags_hold_a_modifier(flags: u64) -> bool {
    const SHIFT: u64 = 0x20000;
    const CONTROL: u64 = 0x40000;
    const OPTION: u64 = 0x80000;
    const COMMAND: u64 = 0x100000;
    flags & (SHIFT | CONTROL | OPTION | COMMAND) != 0
}

/// Which window a dictation will be compared against, from the PID that owns the frontmost one.
///
/// macOS gives an ordinary application no supported way to name another application's window, so
/// the window this process remembers is really its owner PID - and two windows of the SAME
/// application therefore compare equal. `deliver()` will not notice somebody moving between two
/// documents of one editor, only a move to a different application. That is a deliberate,
/// documented loss of precision and not an oversight to be tidied away later; it errs on the side
/// of letting a paste through that a finer comparison would have held, into an application the
/// person did dictate into.
///
/// `None` survives the mapping untouched, because `voice::decide_paste` reads `None` as "do not
/// type" and turning it into an id would let delivery paste into whatever happens to be in front.
#[cfg(any(target_os = "macos", test))]
fn macos_front_window_id(pid: Option<i32>) -> Option<isize> {
    pid.map(|pid| pid as isize)
}

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::base::{CFGetTypeID, TCFType};
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::number::{CFNumber, CFNumberRef};
    use core_foundation::string::CFStringRef;
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::window::{
        copy_window_info, kCGNullWindowID, kCGWindowLayer, kCGWindowListExcludeDesktopElements,
        kCGWindowListOptionOnScreenOnly, kCGWindowOwnerPID,
    };

    use super::{flags_hold_a_modifier, macos_front_window_id, ACCESSIBILITY_HELD};

    /// `V`. `kVK_ANSI_V` from `<Carbon/HIToolbox/Events.h>`, which names a POSITION on an ANSI
    /// keyboard rather than a letter - a virtual keycode is the physical key - so this stays the
    /// paste key whatever layout is active, the way the Linux shim has to look its keycode up.
    const KEY_V: CGKeyCode = 9;

    /// `kCGEventSourceStateHIDSystemState`. The hardware's own state, which is what "is a key
    /// physically down" means; the combined session state would answer for synthesised events too.
    const HID_SYSTEM_STATE: i32 = 1;

    /// Not in `core-graphics` 0.25 - it wraps `CGEventSourceCreate` and the event constructors but
    /// not the flags query - so it is declared here, against the framework the rest of this module
    /// already links.
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        /// The modifier flags currently held for a given source state, as `CGEventFlags` bits.
        fn CGEventSourceFlagsState(state_id: i32) -> u64;
    }

    /// Whether this process has been granted Accessibility access.
    ///
    /// It lives in `ApplicationServices` rather than `CoreGraphics`, and in no wrapper crate this
    /// project already depends on. Deliberately the reading call and not
    /// `AXIsProcessTrustedWithOptions`, which is the one that can raise the system prompt: raising
    /// a prompt from inside a paste is not this function's business.
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        /// Declared returning `u8` and not `bool`: C's `Boolean` is `typedef unsigned char`, and
        /// a Rust `bool` holding anything but 0 or 1 is undefined behaviour rather than a wrong
        /// answer. Apple returns 0 or 1, so the byte is taken as a byte and compared at the call
        /// site.
        fn AXIsProcessTrusted() -> u8;
    }

    /// The application in front, named by the PID that owns its frontmost window, or `None` when
    /// there is no answer to trust.
    ///
    /// `kCGWindowListOptionOnScreenOnly` returns the on-screen windows in front-to-back order, so
    /// the FIRST entry at layer 0 is the frontmost ordinary window. The layer filter is what keeps
    /// the menu bar, the Dock, a notification and every other floating panel from being read as
    /// the window somebody dictated into; `kCGWindowListExcludeDesktopElements` drops the desktop
    /// itself for the same reason.
    ///
    /// Note what this does NOT need: the window list is public information, so it answers before
    /// Accessibility has been granted. `paste_unsupported` is what refuses in that case.
    pub fn foreground_window() -> Option<isize> {
        let windows = copy_window_info(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        )?;
        let owner = windows.iter().find_map(|window| {
            let window = *window;
            if window.is_null() {
                return None;
            }
            // Get rule: the array owns the dictionary, so this retains it for as long as the
            // borrow lasts and releases it at the end of the closure.
            let info = unsafe { CFDictionary::wrap_under_get_rule(window as CFDictionaryRef) };
            if number(&info, unsafe { kCGWindowLayer })? != 0 {
                return None;
            }
            i32::try_from(number(&info, unsafe { kCGWindowOwnerPID })?).ok()
        });
        macos_front_window_id(owner)
    }

    /// One integer-valued entry of a window description, or `None` when it is absent or is not a
    /// number.
    ///
    /// The type check is not defensive tidiness. These dictionaries are heterogeneous - strings,
    /// booleans and rectangles live beside the numbers - and handing a `CFStringRef` to
    /// `CFNumberGetValue` is undefined behaviour rather than a wrong answer.
    fn number(info: &CFDictionary, key: CFStringRef) -> Option<i64> {
        let value = *info.find(key as *const std::ffi::c_void)?;
        if value.is_null() || unsafe { CFGetTypeID(value) } != CFNumber::type_id() {
            return None;
        }
        unsafe { CFNumber::wrap_under_get_rule(value as CFNumberRef) }.to_i64()
    }

    /// Whether any key a hotkey chord can use is still physically down.
    ///
    /// The convention the Windows and Linux shims follow is that "cannot tell" means "do not
    /// type", and every caller reads it that way. There is no failure to report here to hold that
    /// line with: `CGEventSourceFlagsState` returns flags or a zero and has no error channel at
    /// all, so a state it could not read is indistinguishable from nothing being held. That is the
    /// one place this module is less careful than the other two, and it is the API's shape rather
    /// than a choice made here - the honest `true` the other two return on a failed connection has
    /// no call to hang off.
    pub fn modifiers_down() -> bool {
        flags_hold_a_modifier(unsafe { CGEventSourceFlagsState(HID_SYSTEM_STATE) })
    }

    /// Sends Cmd+V as two Quartz events, each carrying the Command flag.
    ///
    /// Two events and not the Windows shim's four: on macOS the modifier travels ON the key event
    /// as a flag rather than as a keystroke of its own, so a Cmd-down/Cmd-up pair is neither needed
    /// nor correct - and the flag has to be set before the event is posted, because posting is what
    /// hands it to the system.
    ///
    /// `CGEventTapLocation::HID` posts at the hardware event tap, the earliest point, which is
    /// where a key the person actually pressed would enter; a session-level post skips taps a real
    /// keystroke would have passed through. Posting reports nothing back at all, so the only honest
    /// answer this function has is whether both events could be created - a paste refused for want
    /// of Accessibility is caught by `paste_unsupported` before ever reaching here.
    pub fn send_paste() -> bool {
        let Ok(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
            return false;
        };
        let (Ok(down), Ok(up)) = (
            CGEvent::new_keyboard_event(source.clone(), KEY_V, true),
            CGEvent::new_keyboard_event(source, KEY_V, false),
        ) else {
            return false;
        };
        for event in [down, up] {
            event.set_flags(CGEventFlags::CGEventFlagCommand);
            event.post(CGEventTapLocation::HID);
        }
        true
    }

    /// Why the text cannot be typed on this Mac, when it cannot.
    ///
    /// Accessibility is the only such reason here, and it is a permission rather than a bug: macOS
    /// asks the person, in System Settings and not from inside the application, to hand over the
    /// power to type into another application. Until they have, a posted event is accepted and
    /// delivered nowhere - which on screen is indistinguishable from dictation being ignored - so
    /// the question is asked before any event is created, and `deliver()` puts the text on the
    /// clipboard when it answers.
    pub fn paste_unsupported() -> Option<&'static str> {
        if unsafe { AXIsProcessTrusted() } != 0 {
            None
        } else {
            Some(ACCESSIBILITY_HELD)
        }
    }
}

/// Everywhere else the shim reports "cannot tell", which every decision already treats as "do not
/// type". The shell targets Windows, Linux and macOS; this exists so the crate builds and tests on
/// whatever else somebody compiles it on.
#[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
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
    pub fn paste_unsupported() -> Option<&'static str> {
        None
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

/// Asked by the Voice tab BEFORE it registers anything: a desktop that gives no global hotkeys
/// gets the sentence instead of three chords nobody will ever hear.
///
/// Only Linux has a session to read - Windows and macOS both hand an application real grabs, so
/// there is nothing to refuse and the answer is `None` by construction rather than by probing.
#[tauri::command]
pub fn voice_hotkeys_unavailable() -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    {
        let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
        let xdg_session_type = std::env::var("XDG_SESSION_TYPE").ok();
        let display = std::env::var("DISPLAY").ok();
        hotkeys_unavailable_for(session_from(
            wayland_display.as_deref(),
            xdg_session_type.as_deref(),
            display.as_deref(),
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Called by the Voice tab once it has read `GET /voice/config`, because that is the only place the
/// configured chords exist — `.ai/voice.yaml` is self-governing and the shell may not read it.
/// Re-registering replaces what was there, so editing the config and reloading the tab is enough.
#[tauri::command]
pub fn voice_register_hotkeys(
    app: AppHandle,
    dictation: String,
    memo: String,
    conversation: String,
) -> Vec<String> {
    register_hotkeys(&app, &dictation, &memo, &conversation)
}

fn poisoned() -> String {
    "the shell's dictation state is poisoned".to_string()
}

fn deliver(text: &str, recorded_into: Option<WindowId>) -> Delivery {
    let held = |reason| Delivery {
        pasted: false,
        held: Some(reason),
    };

    // Asked before anything else, because on a desktop that lets no application type into
    // another there is nothing left to check: the clipboard IS the delivery, and every question
    // below - which window, which modifiers, whose clipboard to give back - is about a keystroke
    // that will not be sent. On Windows this is always `None`, so the path below stays exactly
    // what it was.
    if let Some(reason) = platform::paste_unsupported() {
        let _ = set_clipboard_text(text);
        return held(reason);
    }
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
fn register_hotkeys(
    app: &AppHandle,
    dictation: &str,
    memo: &str,
    conversation: &str,
) -> Vec<String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    // Whatever was registered before is dropped first: without this, changing a chord in the config
    // leaves the old one live and the shell answers to both.
    let _ = app.global_shortcut().unregister_all();

    let mut refused = Vec::new();
    for (chord, chord_kind) in [
        (dictation, Chord::Dictation),
        (memo, Chord::Memo),
        (conversation, Chord::Conversation),
    ] {
        if chord.trim().is_empty() {
            continue;
        }
        let handle = app.clone();
        let registered = app
            .global_shortcut()
            .on_shortcut(chord, move |_, _, event| {
                // Pressed only. A chord reports both press and release, and acting on both would
                // start and immediately stop every recording — and would toggle the conversation
                // mode in and straight back out on every press.
                if event.state() != tauri_plugin_global_shortcut::ShortcutState::Pressed {
                    return;
                }
                let app = handle.clone();
                match chord_kind {
                    Chord::Conversation => {
                        if let Err(error) = app.emit(CONVERSATION_EVENT, ()) {
                            eprintln!("[voice] could not toggle conversation: {error}");
                        }
                    }
                    Chord::Dictation | Chord::Memo => {
                        let state = app.state::<Dictation>();
                        let is_memo = chord_kind == Chord::Memo;
                        if let Err(error) = voice_hotkey(app.clone(), state, is_memo) {
                            eprintln!("[voice] hotkey failed: {error}");
                        }
                    }
                }
            });
        if registered.is_err() {
            refused.push(chord.to_string());
        }
    }
    refused
}

/// Which of the three chords fired.
///
/// A third enum rather than a third `Kind`, because these are not three of the same thing: two of
/// them start a recording this process owns, and the third toggles a mode it does not. Folding them
/// into `Kind` would put a value in the type that serialises into the núcleo's `kind` parameter and
/// is never sent there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chord {
    Dictation,
    Memo,
    Conversation,
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
    #[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
    #[test]
    fn the_non_windows_shim_never_types() {
        assert_eq!(platform::foreground_window(), None);
        assert!(platform::modifiers_down());
        assert!(!platform::send_paste());
    }

    /// A Wayland desktop is still Wayland when XWayland has handed us a `DISPLAY`.
    #[test]
    fn wayland_wins_over_an_xwayland_display() {
        // This is the whole reason the question is asked by a function rather than inline: XWayland
        // exports `DISPLAY` as well, so "there is a DISPLAY, therefore X11" reads a Wayland desktop
        // as X11 and then synthesises a keystroke no compositor will deliver. The paste fails
        // silently, which looks exactly like dictation being ignored.
        assert_eq!(
            session_from(Some("wayland-0"), None, Some(":0")),
            Session::Wayland
        );
        // The other route to the same answer. A process started outside the compositor's own
        // environment can be missing `WAYLAND_DISPLAY` while the session type still says plainly
        // what the desktop is, and that claim is enough on its own.
        assert_eq!(
            session_from(None, Some("wayland"), Some(":0")),
            Session::Wayland
        );
    }

    /// X11 is what is left once nothing claims Wayland.
    #[test]
    fn x11_is_a_display_with_no_wayland() {
        // A plain X11 session: a display, and no claim of Wayland from either direction.
        assert_eq!(session_from(None, None, Some(":0")), Session::X11);
        // A session type naming something else is not a Wayland claim. Only the exact word counts,
        // because it is the one the desktop's own session files agree on.
        assert_eq!(session_from(None, Some("x11"), Some(":0")), Session::X11);
        assert_eq!(session_from(None, Some("tty"), Some(":1")), Session::X11);
        // An exported-but-empty `WAYLAND_DISPLAY` is a variable somebody cleared, not a compositor.
        // Reading it as present would refuse to type on a desktop that can be typed into — the
        // opposite mistake, and just as invisible.
        assert_eq!(session_from(Some(""), None, Some(":0")), Session::X11);
    }

    /// With nothing to type into, the answer has to be "cannot tell" rather than a guess.
    #[test]
    fn no_display_at_all_is_unknown() {
        // A headless run — ssh, a systemd service, a container — has no desktop at all. `Unknown` is
        // what lets the refusal say so, instead of picking X11 and dying at the XTest call.
        assert_eq!(session_from(None, None, None), Session::Unknown);
        // Empty is absent on all three, not present-and-blank.
        assert_eq!(session_from(Some(""), Some(""), Some("")), Session::Unknown);
        // And a session type with no display behind it names no server anything can reach.
        assert_eq!(session_from(None, Some("x11"), Some("")), Session::Unknown);
    }

    /// A Wayland session has no global hotkeys to give, and the page has to be told so.
    ///
    /// The failure this prevents is a silent one: no Wayland compositor hands an ordinary client
    /// a system-wide grab, so the chords "register" and then simply never fire. On screen that is
    /// indistinguishable from the feature being broken, and there is nowhere to read why. X11 and
    /// a session nothing identified both keep their hotkeys - `None` here means "no reason to
    /// refuse", not "no desktop": refusing on Unknown would take the chords away from a working
    /// X11 session whose environment merely arrived incomplete.
    #[test]
    fn no_global_hotkeys_on_a_wayland_session() {
        let wayland = hotkeys_unavailable_for(Session::Wayland);
        assert!(
            wayland.is_some_and(|sentence| !sentence.trim().is_empty()),
            "a Wayland session has to come back with a sentence to show, got {wayland:?}"
        );
        assert_eq!(
            hotkeys_unavailable_for(Session::X11),
            None,
            "X11 delivers global hotkeys, so there is nothing to refuse"
        );
        assert_eq!(
            hotkeys_unavailable_for(Session::Unknown),
            None,
            "a session nothing could identify is not a reason to take the hotkeys away"
        );
    }

    /// A modifier the person is still holding turns a synthesized Cmd+V into another chord.
    #[test]
    fn any_held_modifier_blocks_the_paste() {
        // Cmd+V is only Cmd+V when nothing else is down. With Shift held, the same synthesized
        // chord arrives as Cmd+Shift+V, which pastes without formatting in some applications and
        // is a different command altogether in others; with Option or Control it is something
        // else again. So a held modifier HOLDS the paste instead of sending it, and the person
        // keeps the text on the clipboard rather than getting a surprise typed into their window.
        assert!(!flags_hold_a_modifier(0));
        // The four `CGEventFlags` bits that count, each one alone: Shift, Control, Option, Command.
        assert!(flags_hold_a_modifier(0x20000));
        assert!(flags_hold_a_modifier(0x40000));
        assert!(flags_hold_a_modifier(0x80000));
        assert!(flags_hold_a_modifier(0x100000));
        // Two of them together are still "something is held".
        assert!(flags_hold_a_modifier(0x20000 | 0x100000));
        // A bit outside the mask is not a held key. `CGEventFlags` also carries state nobody is
        // holding down at all, and reading one of those as a modifier would refuse every paste
        // forever, which on screen looks exactly like dictation being ignored.
        assert!(!flags_hold_a_modifier(0x1));
    }

    /// A front window is named by the process that owns it, and that costs precision on purpose.
    #[test]
    fn a_front_window_is_named_by_its_owner_pid() {
        // macOS gives an ordinary application no supported way to name another application's
        // window, so the window we remember is really its owner PID. Two windows of the SAME
        // application therefore compare equal, and `deliver()` will not notice the person moving
        // between them - it only notices a move to a different app. That is a deliberate,
        // documented loss of precision, not an oversight to be tidied away later.
        assert_eq!(macos_front_window_id(Some(4321)), Some(4321));
        // Nothing in front is nothing to remember, and the `None` has to survive the mapping:
        // turning it into an id would let delivery paste into whatever happens to be there.
        assert_eq!(macos_front_window_id(None), None);
    }
}
