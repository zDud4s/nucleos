//! Killing a spawned process's whole TREE, and the two platform contracts that make that possible.
//!
//! `kill_on_drop` reaches the direct child and stops there. Every program this daemon spawns is a
//! supervisor of something: `claude` and `codex` run bash, cargo, git and node; a gate command is
//! whatever the project put in it; and `git push` runs ssh and a credential helper. Terminating only
//! the parent orphans all of them, and an orphaned `cargo build` keeps file locks inside the worktree
//! the run was supposed to release — which is what makes `git worktree remove` fail through its
//! entire backoff and leaves the GC reporting the same failure every half hour.
//!
//! **This module owns the spawn as well as the kill, and that pairing is the reason it exists rather
//! than a tidier copy of what was here before.** Two divergent copies of `TreeKiller` lived in
//! `gate.rs` and `runner.rs`, and BOTH were wrong, each on a different platform:
//!
//! - Off Windows, the gate's copy ran `kill -KILL -<pid>`, which names a process *group* led by
//!   `<pid>` — and nothing in this repository ever created one, so every child inherited the daemon's
//!   group. That arm either killed nothing (no group with that id) or killed a group somebody else
//!   led. The runner's copy called `taskkill` with no `cfg` at all, so off Windows it was a silent
//!   no-op; the honest one of the two was the one that admitted nothing.
//! - On Windows, both ran `taskkill /T`, which walks the parent/child links in a process snapshot.
//!   **Measured on this machine: those links do not survive MSYS's `fork` emulation.** `sh -c 'sleep
//!   30 & sleep 30'` leaves a `sleep` whose recorded Windows parent is a forked stub that has already
//!   exited, so `/T` finds no such child and the grandchild outlives the kill — which is exactly the
//!   `bash` → `cargo` shape the runner's comment was written about.
//!
//! So each platform gets the primitive that actually holds a set of processes together, and each is
//! established at the moment that platform requires:
//!
//! | | Unix | Windows |
//! |---|---|---|
//! | Primitive | process group | job object |
//! | Established | `spawn_in_own_group`, BEFORE the spawn | `TreeKiller::new`, just AFTER it |
//! | Reaches a descendant spawned later | yes (inherits the group) | yes (inherits the job) |
//!
//! Neither is a copy-pasteable line, which is the argument for one module over two call sites. The
//! Windows half keeps `taskkill /T` as a fallback for when a job cannot be created or assigned, so a
//! refusal degrades to what this code already did rather than to nothing.

/// Prepares `command` so a `TreeKiller` on its pid can name every process it goes on to spawn.
///
/// Unix only, because that is the platform whose primitive has to exist before the child does: the
/// child becomes its own process-group leader, so `kill -KILL -<pid>` reaches it and everything it
/// spawned — and nothing else. It also detaches the child from the daemon's terminal group, which is
/// wanted anyway: a Ctrl-C in the daemon's console should not reach a gate command mid-measurement.
///
/// On Windows this does nothing and the work is `TreeKiller::new`'s, because a job object is assigned
/// to a process that already exists. **Calling this is still mandatory at every call site**, and not
/// as a formality: whoever adds a call site on one platform and skips it is writing the exact defect
/// this module was extracted to end.
pub fn spawn_in_own_group(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Through `as_std_mut` rather than tokio's own wrapper of the same name: this is the std
        // API the wrapper forwards to, and going straight to it keeps the call independent of which
        // tokio version re-exported it.
        command.as_std_mut().process_group(0);
    }
    command
}

/// Kills a spawned process's whole tree when it is dropped, unless disarmed first.
///
/// **Sound only while the `Child` is still alive**, because the open process handle is what stops
/// Windows reusing the pid. So every call site owes two things: `disarm()` the moment the child is
/// reaped, and declare the killer AFTER the child so it drops FIRST.
///
/// On Windows `new` does real work — it opens the process and puts it in a fresh job object — and
/// there is a window between `spawn()` and this call in which a descendant would escape that job.
/// It is the process-creation latency of a program that has not run a line of its own code yet, and
/// it is the price of not spawning suspended; stated because a silent race is worse than a small one.
pub struct TreeKiller {
    pid: u32,
    armed: bool,
    /// The Windows job object holding the child and everything it spawns, when one could be made.
    #[cfg(windows)]
    job: Option<windows::Job>,
}

impl TreeKiller {
    pub fn new(pid: u32) -> Self {
        Self {
            pid,
            armed: true,
            #[cfg(windows)]
            job: windows::Job::capture(pid),
        }
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// Kills now rather than at drop, for the call sites that must take the tree down *before* they
    /// let go of the child — a timeout that still has to reap the process, or a drain that cannot
    /// finish while a leftover grandchild holds the pipe.
    pub fn kill_now(&mut self) {
        if self.armed {
            self.terminate();
            self.armed = false;
        }
    }

    #[cfg(windows)]
    fn terminate(&self) {
        // The job when there is one, and only then the snapshot walk. They are not equivalent and
        // the fallback is strictly weaker — it is what runs when the OS refused us a job, not a
        // second opinion.
        match &self.job {
            Some(job) => job.terminate(),
            None => taskkill_tree(self.pid),
        }
    }

    #[cfg(not(windows))]
    fn terminate(&self) {
        kill_process_group(self.pid);
    }
}

impl Drop for TreeKiller {
    fn drop(&mut self) {
        if self.armed {
            self.terminate();
        }
    }
}

/// A set of processes that dies when THIS process dies, however this process dies.
///
/// [`TreeKiller`] is the other shape of the same problem and does not cover this one. It kills on
/// `Drop`, and a drop is a thing that happens in a program that is still running: `TerminateProcess`
/// — which is what `Stop-Process`, Task Manager and a crash all do — runs no destructor, unwinds
/// nothing, and leaves every child alive.
///
/// MEASURED, 2026-08-20: the sidecar supervisor set `kill_on_drop(true)` for exactly this reason and
/// it was inert against the case that actually happens. Two days of daemon restarts had left 31
/// orphaned sidecars — fourteen `email`, fourteen `telegram`, one each of the rest — and because an
/// orphan keeps the loopback port, every freshly spawned sidecar died at `bind` and was "restarted"
/// forever. The browser sidecar the app was talking to was two days old and would have stayed that
/// way through any number of restarts. The daemon was reporting `running`, and it was true, and it
/// was about the wrong process.
///
/// The Windows primitive for this is a job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The
/// kernel holds the set, so the promise is kept by the OS rather than by our code getting the chance
/// to run — which is the whole point, since the case being covered is the one where it does not.
///
/// This is the same primitive [`TreeKiller`] uses and the OPPOSITE flag, deliberately. There, closing
/// the handle must not kill, because `disarm()` exists to say "these are yours now". Here, closing
/// the handle is the only signal that will still be delivered.
///
/// Off Windows this adopts nothing and says so rather than pretending: `kill_on_drop` still covers
/// the orderly shutdown, and a hard kill still orphans. Naming the gap is better than a no-op that
/// reads like a guarantee.
pub struct Litter {
    #[cfg(windows)]
    job: Option<windows::Job>,
}

impl Litter {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            job: windows::Job::create(true),
        }
    }

    /// Puts a process, and everything it goes on to spawn, into the set.
    ///
    /// Best effort and silent about refusal, because the caller's alternative is not to spawn the
    /// sidecar at all — a supervisor that refused to start a child it could not adopt would trade a
    /// leak for an outage. The lifetime rule is the same as [`TreeKiller`]'s: the pid must belong to
    /// a child the caller still holds, or Windows may have reused it.
    pub fn adopt(&self, pid: u32) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.adopt(pid);
        }
        #[cfg(not(windows))]
        let _ = pid;
    }
}

impl Default for Litter {
    fn default() -> Self {
        Self::new()
    }
}

/// Best effort by definition: this can run while a future is being dropped, so it cannot await and
/// cannot report. `/T` is the whole point (such tree as it can see), `/F` because a cancelled run is
/// not being asked politely.
#[cfg(windows)]
fn taskkill_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// The negative pid is a process GROUP, which is why `spawn_in_own_group` is not optional. A child
/// spawned without it inherits the daemon's group, and this line then either names a group that does
/// not exist or names one the daemon shares with processes nobody asked us to touch.
#[cfg(not(windows))]
fn kill_process_group(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &format!("-{pid}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// A job object holding one process and, from the moment of assignment, everything it spawns.
    ///
    /// This is the Windows answer to a process group, and the reason it is worth a dependency and an
    /// `unsafe` block: membership is inherited by descendants and recorded by the kernel, so it
    /// survives the parent-link severing that `taskkill /T` cannot see through.
    ///
    /// **Deliberately WITHOUT `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.** That flag would make closing
    /// this handle kill everything in the job — which is precisely what `disarm()` exists to say we
    /// must not do, and disarming would then have to rewrite the job's limits before dropping it. One
    /// explicit `TerminateJobObject` is the smaller mechanism, and it keeps `Drop` meaning "release
    /// the handle" rather than "and by the way, kill".
    pub struct Job(HANDLE);

    // A kernel handle is a value, not a pointer into this process's memory, and nothing about a job
    // object is thread-affine. Needed because a `TreeKiller` is held across awaits inside a tokio
    // task, and the raw `HANDLE` is a `*mut c_void`.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        /// An empty job. `kill_on_close` decides what the kernel does when the last handle to it
        /// goes — which, for a process that was terminated rather than shut down, is the only thing
        /// that still happens. See [`super::Litter`] and [`super::TreeKiller`]: they want opposite
        /// answers, and the difference is this one flag.
        pub fn create(kill_on_close: bool) -> Option<Self> {
            // SAFETY: the handle is checked before use and released on every path out.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                if kill_on_close {
                    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                    let set = SetInformationJobObject(
                        job,
                        JobObjectExtendedLimitInformation,
                        (&raw const limits).cast(),
                        std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    );
                    if set == 0 {
                        // Refused: a job whose members outlive it is not the thing being asked for,
                        // and handing one back would be a guarantee that is not held.
                        CloseHandle(job);
                        return None;
                    }
                }
                Some(Self(job))
            }
        }

        /// Puts one process, and from this moment everything it spawns, into the job.
        pub fn adopt(&self, pid: u32) -> bool {
            // SAFETY: `self.0` is a job handle this type created and has not closed. The pid is one
            // whose `Child` the caller still holds, which is what stops Windows reusing it between
            // the spawn and this `OpenProcess`.
            unsafe {
                // The two rights this needs and no more: assigning costs a quota change, and
                // terminating the job terminates its members.
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if process.is_null() {
                    return false;
                }
                let assigned = AssignProcessToJobObject(self.0, process);
                // The process handle has done its work the moment the assignment is recorded; the
                // JOB handle is the one worth keeping. Closing this does not remove the process
                // from the job.
                CloseHandle(process);
                assigned != 0
            }
        }

        /// `None` whenever the OS declines, which the caller treats as "fall back to `taskkill`"
        /// rather than as "nothing to kill".
        pub fn capture(pid: u32) -> Option<Self> {
            let job = Self::create(false)?;
            if !job.adopt(pid) {
                return None;
            }
            Some(job)
        }

        pub fn terminate(&self) {
            // SAFETY: `self.0` is a job handle this type created and has not closed.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: as above, and this is the only place the handle is released.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    /// `sleep` as a *program*, which on Windows means Git's `usr/bin` must be on PATH — the same
    /// dependency nine tests in `gate::` and `transcribe::` already carry for `echo`. Long enough
    /// that "it exited on its own" is never the reason a test below sees a dead process.
    fn long_sleep() -> tokio::process::Command {
        let mut command = tokio::process::Command::new("sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        spawn_in_own_group(&mut command);
        command
    }

    /// The case `kill_on_drop` and `TreeKiller` both miss, and the one that actually happens.
    ///
    /// Both of those kill from a destructor, and a destructor is a thing that runs in a program that
    /// is still running. `TerminateProcess` — Stop-Process, Task Manager, a crash — runs none, so
    /// the promise has to be held by the kernel instead. Dropping the `Litter` closes the last
    /// handle to the job, which is the signal that is still delivered when nothing of ours is.
    ///
    /// Not a hypothetical: 31 orphaned sidecars from two days of daemon restarts, an orphan holding
    /// each loopback port, and every replacement dying at `bind` and being "restarted" forever.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_process_in_a_litter_does_not_outlive_it() {
        let mut child = long_sleep().spawn().expect("`sleep` must be on PATH");
        let litter = Litter::new();
        litter.adopt(child.id().expect("a live child has a pid"));

        drop(litter);

        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("the child outlived the job that was holding it")
            .expect("waiting on the child must succeed");
    }

    /// The control, and it is not a formality: without it the test above passes just as well against
    /// a `sleep` that was never going to last, and would prove nothing about the job at all.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_process_outside_a_litter_is_left_alone() {
        let mut child = long_sleep().spawn().expect("`sleep` must be on PATH");
        let litter = Litter::new();

        drop(litter);

        assert!(
            tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .is_err(),
            "the child ended on its own, so the other test measures nothing"
        );
    }

    #[tokio::test]
    async fn killing_a_tree_kills_the_process_it_names() {
        let mut child = long_sleep().spawn().expect("`sleep` must be on PATH");
        let mut killer = TreeKiller::new(child.id().expect("a live child has a pid"));

        killer.kill_now();

        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("a killed process must not outlive its killer")
            .expect("waiting on the child must succeed");
    }

    /// The other direction, and the one a no-op implementation would pass by accident if it were the
    /// only test: disarming has to actually withhold the kill.
    #[tokio::test]
    async fn a_disarmed_killer_leaves_the_process_alone() {
        let mut child = long_sleep().spawn().expect("`sleep` must be on PATH");
        let mut killer = TreeKiller::new(child.id().expect("a live child has a pid"));

        killer.disarm();
        drop(killer);

        assert!(
            child.try_wait().expect("try_wait must succeed").is_none(),
            "a disarmed killer must not have killed anything"
        );
        let _ = child.kill().await;
    }

    /// **The assertion that distinguishes killing a TREE from killing a child, which is the entire
    /// reason this type exists rather than `kill_on_drop` — and the one that failed on the
    /// `taskkill /T` implementation this module replaced.**
    ///
    /// The subject is deliberately an MSYS shell, because that is the shape that broke: `sh`'s
    /// background job is created through a `fork` emulation that leaves the surviving `sleep` naming
    /// an already-exited stub as its Windows parent, so a snapshot walk finds no descendant to kill.
    /// A job object records membership in the kernel instead, so it does not care how the process
    /// came to exist. This is `claude` → `bash` → `cargo` with the runtime cost of two `sleep`s.
    ///
    /// Observed through the pipe rather than by tracking the grandchild's pid, and that is the point
    /// rather than a convenience: EOF on stdout arrives only when EVERY holder of the write end has
    /// closed it, and the background `sleep` inherited that handle. So a read that finishes means the
    /// grandchild is gone, and a read that hangs means it is not — which is exactly the failure this
    /// guards against, stated as `gate.rs`'s drain loop already states it. Tracking pids instead
    /// would not even work here: MSYS `sh` reports its own pid namespace in `$!`, not the Windows one.
    #[tokio::test]
    async fn killing_a_tree_kills_a_grandchild_the_direct_child_left_behind() {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            // The background `sleep` outlives `sh` if only `sh` is killed, and holds the inherited
            // write end of the pipe for as long as it lives.
            .arg("sleep 30 & sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        spawn_in_own_group(&mut command);
        let mut child = command.spawn().expect("`sh` must be on PATH");
        let mut stdout = child.stdout.take().expect("stdout was piped");
        let mut killer = TreeKiller::new(child.id().expect("a live child has a pid"));

        // Give `sh` time to actually spawn the grandchild; killing before it exists would make this
        // test pass without ever exercising the mechanism.
        tokio::time::sleep(Duration::from_secs(2)).await;
        killer.kill_now();

        let mut sink = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), stdout.read_to_end(&mut sink))
            .await
            .expect("the grandchild still holds the pipe, so the tree was not killed")
            .expect("reading the pipe to EOF must succeed");
    }
}
