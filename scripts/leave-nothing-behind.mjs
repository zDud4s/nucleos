/**
 * Nothing a gate starts outlives the gate.
 *
 * Every script here spawns something heavy — a headless Chromium, the built
 * Tauri window — and every one of them used to tear it down on exactly one
 * path: the one where everything worked. A run that failed, threw, or was
 * interrupted at the wrong moment left the process running and its profile on
 * disk. On Windows that is permanent rather than temporary: a child is not in
 * the parent's job object, so it survives the parent by default. The evidence
 * was over a hundred abandoned profile directories under `%TEMP%`, the oldest a
 * week old, and a set of processes nobody could account for.
 *
 * Orphans are worse than litter. The next person to see one cannot tell it from
 * their own browser — it is the same `chrome.exe`, under the same name — so the
 * obvious sweep, matching on process name, takes real windows with it. The only
 * honest way to tell a gate's browser from a person's is the profile path on
 * its command line, and the only way to never have to is to leave none.
 *
 * `taskkill /T` rather than `child.kill()`: Chromium is a process tree, and
 * killing the root leaves the renderers to be adopted and found much later.
 */
import { execFileSync } from "node:child_process";
import { appendFileSync, existsSync, rmSync } from "node:fs";

const owned = [];
let torn = false;

/**
 * How long to keep asking, in tenths of a second, before giving up as litter.
 *
 * Generous on purpose, and free when things go well: every loop below leaves
 * the moment it succeeds, so this is only ever spent on a machine that is not
 * answering. That is exactly when it is needed — the one run that still leaked
 * during this fix was the one sharing a machine with a full test suite.
 */
const PATIENCE = 150;

/**
 * Sleep without spinning, in a context that cannot await.
 *
 * All of this runs from an `exit` handler, where the event loop is already
 * closed and a promise will never resolve. `Atomics.wait` on a buffer nobody
 * will ever notify is the one way to hold the thread for a fixed time.
 */
function pause(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

/** Whether a pid has actually gone, as opposed to having been asked to. */
function gone(pid) {
  try {
    process.kill(pid, 0);
    return false;
  } catch (error) {
    /* EPERM means it exists and is not ours to signal, which is still existing. */
    return error.code === "ESRCH";
  }
}

/**
 * Tie a spawned process and its scratch directory to this script's lifetime.
 *
 * Declared once at the spawn and never mentioned again: the paths that leaked
 * are precisely the ones nobody remembered to write, so this cannot be
 * something a caller has to remember at each of them.
 */
export function ownUntilExit(child, scratch) {
  owned.push({ child, scratch });
  return child;
}

/**
 * A breadcrumb trail for the one thing here nobody can watch.
 *
 * All of this runs while the process is on its way out, often from a signal, so
 * there is no console left to print to and no test that can observe it from
 * inside. Set `NUCLEOS_TEARDOWN_TRACE` to a path and the teardown says what it
 * did — which is how the EPERM on the profile root was found, and how the
 * Ctrl+C path was shown to actually run rather than merely to be registered.
 * Off, and costing nothing, unless the variable is set.
 */
function trace(what) {
  if (process.env.NUCLEOS_TEARDOWN_TRACE === undefined) return;
  try {
    appendFileSync(process.env.NUCLEOS_TEARDOWN_TRACE, what + "\n");
  } catch {
    /* tracing must never be the reason a teardown fails */
  }
}

/*
  Killing first and deleting second, in two passes over everything rather than
  one pass per item.

  Windows ends a process a few seconds after a console event whatever its
  handler is doing, so a teardown can be cut off in the middle. What it is cut
  off during decides what the interruption costs: a process left running is
  indistinguishable from somebody's own browser and gets swept up by a filter
  that cannot tell, while a directory left in `%TEMP%` is only litter. So every
  kill is asked for before any of the waiting begins.
*/
function tearDown() {
  trace("tearDown entered, torn=" + torn + ", owned=" + owned.length);
  if (torn) return;
  torn = true;

  for (const { child } of owned) {
    if (child === undefined || child.pid === undefined) continue;
    try {
      if (process.platform === "win32") {
        execFileSync("taskkill", ["/pid", String(child.pid), "/T", "/F"], { stdio: "ignore" });
      } else {
        child.kill("SIGKILL");
      }
    } catch {
      /* Already gone, which is the outcome this was after. */
    }
    trace("killed " + child.pid);
  }

  for (const { child, scratch } of owned) {
    if (scratch === undefined) continue;
    /*
      `taskkill` returns once the kill is asked for, not once it has happened,
      and a Chromium that is still dying still holds its profile — deleting on
      the way past that gave EPERM on the profile root every time. Waiting on
      the root is not quite enough either, since the handles belong to its
      children, so the delete asks until it works rather than guessing at how
      long that takes. `rmSync`'s own `maxRetries` gave up too early.
    */
    if (child !== undefined && child.pid !== undefined) {
      for (let tenth = 0; tenth < PATIENCE && !gone(child.pid); tenth++) pause(100);
    }
    for (let attempt = 0; attempt < PATIENCE; attempt++) {
      try {
        rmSync(scratch, { recursive: true, force: true });
        break;
      } catch {
        /* A profile that never comes free is litter. Never fail a gate over it. */
        pause(100);
      }
    }
    trace("removed " + scratch + ", gone=" + !existsSync(scratch));
  }
}

/*
  `exit` covers the ordinary return, every `process.exit()` inside a `die()`, and
  the fatal path an uncaught exception or a rejected top-level await takes.
  Signals never reach it: an unhandled SIGINT ends the process without running a
  handler at all, and Ctrl+C is how most of these runs actually end.

  Three of them on Windows rather than one, because the console has three ways
  of saying stop and they arrive under different names: Ctrl+C is SIGINT,
  Ctrl+Break is SIGBREAK, and closing the window is SIGHUP. Closing the window
  is the one that would otherwise be missed, and it is not an unusual way to
  abandon a gate that is taking too long.
*/
process.on("exit", tearDown);
const SIGNALS =
  process.platform === "win32"
    ? ["SIGINT", "SIGTERM", "SIGBREAK", "SIGHUP"]
    : ["SIGINT", "SIGTERM", "SIGHUP"];
/* 128 + the signal's number, which is what a shell reports for a death by signal. */
const STATUS = { SIGHUP: 129, SIGINT: 130, SIGTERM: 143, SIGBREAK: 149 };
for (const signal of SIGNALS) {
  process.on(signal, () => {
    tearDown();
    process.exit(STATUS[signal]);
  });
}
