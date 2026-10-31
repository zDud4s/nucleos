//! Keeping the machine from going to sleep on its own while the daemon has work in flight.
//!
//! Only the automatic idle sleep is held off. Closing the lid, pressing the power button or asking
//! for sleep from the menu still obeys the system: this asks the OS "do not idle-sleep", it does not
//! override the user. Nothing here knows what "work" is; the caller counts live runs and passes a
//! bool to [`KeepAwake::hold_awake_while`], which is why this lives at the bottom layer.
//!
//! The OS action sits behind [`Inhibitor`] so the policy (the setting, the held/released edge, the
//! "warn once" rule) is tested without touching the real machine.
//!
//! - **Windows.** `SetThreadExecutionState` is per THREAD, so the flag is set and cleared from one
//!   dedicated long-lived thread, never from a tokio worker that may be a different thread next time.
//! - **macOS.** A `caffeinate -i` child process, alive while holding and killed on release.
//! - **Linux.** A `systemd-inhibit --what=idle` child process around a long sleep, same lifecycle.
//!
//! A missing or failing inhibitor never fails the daemon: the first failure is logged as a warning
//! and the next ones stay quiet, because this runs on a timer.

/// The OS side of holding the machine awake. Both calls are idempotent from the caller's view.
pub trait Inhibitor: Send {
    /// Starts holding. An `Err` is the reason the machine could not be held, for one warning.
    fn acquire(&mut self) -> Result<(), String>;
    /// Lets go. Never fails: there is nothing a caller could do about it.
    fn release(&mut self);
}

/// The policy around an [`Inhibitor`]: on only when enabled, edge-triggered, quiet after a failure.
pub struct KeepAwake {
    enabled: bool,
    held: bool,
    warned: bool,
    inhibitor: Box<dyn Inhibitor>,
}

impl KeepAwake {
    pub fn new(enabled: bool, inhibitor: Box<dyn Inhibitor>) -> Self {
        Self {
            enabled,
            held: false,
            warned: false,
            inhibitor,
        }
    }

    /// The real inhibitor of this platform.
    pub fn system(enabled: bool) -> Self {
        Self::new(enabled, Box::new(SystemInhibitor::default()))
    }

    /// Whether the machine is being held awake right now.
    pub fn is_held(&self) -> bool {
        self.held
    }

    /// Holds the machine awake while `has_work`, and lets go when it is false. Meant to be called on
    /// every tick: only a change of answer reaches the OS. With the setting off it never holds.
    pub fn hold_awake_while(&mut self, has_work: bool) {
        let want = self.enabled && has_work;
        if want == self.held {
            return;
        }
        if !want {
            self.inhibitor.release();
            self.held = false;
            return;
        }
        match self.inhibitor.acquire() {
            Ok(()) => self.held = true,
            Err(reason) => {
                // Not held, so the next tick tries again; only the warning is once.
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(%reason, "keep-awake: could not hold the machine awake; it may sleep with work running");
                }
            }
        }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        if self.held {
            self.inhibitor.release();
        }
    }
}

/* ------------------------------------------------------------------ Windows -- */

#[cfg(windows)]
mod platform {
    use std::sync::mpsc::{Sender, channel};

    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };

    use super::Inhibitor;

    /// Owns one thread that does nothing but set and clear the execution state, because the state
    /// belongs to the thread that set it.
    #[derive(Default)]
    pub struct SystemInhibitor {
        thread: Option<Sender<bool>>,
    }

    impl SystemInhibitor {
        fn send(&mut self, hold: bool) -> Result<(), String> {
            if self.thread.is_none() {
                let (tx, rx) = channel::<bool>();
                std::thread::Builder::new()
                    .name("keep-awake".to_string())
                    .spawn(move || {
                        while let Ok(hold) = rx.recv() {
                            let flags = if hold {
                                ES_CONTINUOUS | ES_SYSTEM_REQUIRED
                            } else {
                                ES_CONTINUOUS
                            };
                            // SAFETY: plain Win32 call with flag bits; no pointers involved.
                            unsafe { SetThreadExecutionState(flags) };
                        }
                        // Sender dropped: clear on the way out, though the thread's end does too.
                        unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
                    })
                    .map_err(|error| error.to_string())?;
                self.thread = Some(tx);
            }
            match &self.thread {
                Some(tx) => tx.send(hold).map_err(|error| error.to_string()),
                None => Err("no keep-awake thread".to_string()),
            }
        }
    }

    impl Inhibitor for SystemInhibitor {
        fn acquire(&mut self) -> Result<(), String> {
            self.send(true)
        }

        fn release(&mut self) {
            let _ = self.send(false);
        }
    }
}

/* ------------------------------------------------------- macOS and Linux -- */

#[cfg(not(windows))]
mod platform {
    use std::process::{Child, Command, Stdio};

    use super::Inhibitor;

    #[cfg(target_os = "macos")]
    fn command() -> Command {
        let mut command = Command::new("caffeinate");
        command.arg("-i");
        command
    }

    #[cfg(not(target_os = "macos"))]
    fn command() -> Command {
        let mut command = Command::new("systemd-inhibit");
        command.args([
            "--what=idle",
            "--who=NucleOS",
            "--why=A run is in progress",
            "sleep",
            "infinity",
        ]);
        command
    }

    /// A child process that holds the inhibition for as long as it lives.
    #[derive(Default)]
    pub struct SystemInhibitor {
        child: Option<Child>,
    }

    impl Inhibitor for SystemInhibitor {
        fn acquire(&mut self) -> Result<(), String> {
            if self.child.is_some() {
                return Ok(());
            }
            let child = command()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| error.to_string())?;
            self.child = Some(child);
            Ok(())
        }

        fn release(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    impl Drop for SystemInhibitor {
        fn drop(&mut self) {
            self.release();
        }
    }
}

pub use platform::SystemInhibitor;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Records what the policy asked of the OS, and can be made to fail.
    struct Fake {
        calls: Arc<Mutex<Vec<&'static str>>>,
        fail: bool,
    }

    impl Inhibitor for Fake {
        fn acquire(&mut self) -> Result<(), String> {
            self.calls.lock().unwrap().push("acquire");
            if self.fail {
                Err("no such binary".to_string())
            } else {
                Ok(())
            }
        }

        fn release(&mut self) {
            self.calls.lock().unwrap().push("release");
        }
    }

    fn keep_awake(enabled: bool, fail: bool) -> (KeepAwake, Arc<Mutex<Vec<&'static str>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fake = Fake {
            calls: calls.clone(),
            fail,
        };
        (KeepAwake::new(enabled, Box::new(fake)), calls)
    }

    #[test]
    fn work_holds_the_machine_awake_and_idle_lets_go() {
        let (mut awake, calls) = keep_awake(true, false);
        awake.hold_awake_while(false);
        assert!(!awake.is_held());
        awake.hold_awake_while(true);
        awake.hold_awake_while(true);
        assert!(awake.is_held());
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["acquire"],
            "one edge, one call"
        );
        awake.hold_awake_while(false);
        assert!(!awake.is_held());
        assert_eq!(*calls.lock().unwrap(), vec!["acquire", "release"]);
    }

    #[test]
    fn the_setting_off_never_holds() {
        let (mut awake, calls) = keep_awake(false, false);
        awake.hold_awake_while(true);
        assert!(!awake.is_held());
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_missing_inhibitor_does_not_fail_the_daemon() {
        let (mut awake, calls) = keep_awake(true, true);
        awake.hold_awake_while(true);
        awake.hold_awake_while(true);
        assert!(!awake.is_held(), "a failed hold is not a held one");
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["acquire", "acquire"],
            "it keeps trying on each tick, and only warns once"
        );
        assert!(awake.warned);
        awake.hold_awake_while(false);
        assert_eq!(
            calls.lock().unwrap().len(),
            2,
            "nothing held, nothing to release"
        );
    }
}
