import { invoke, isTauri } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { useQuery } from "@tanstack/react-query";
import { apiFetch, apiText, isApiUnavailable, probeHealth, type UpdateReadiness } from "./client";
import { keys } from "./keys";

/** How often the shell asks the release feed whether something newer exists. */
export const UPDATE_EVERY = 6 * 60 * 60 * 1000;

/** How long the install waits for the núcleo to let go of its files. */
const STOP_WAIT = 15_000;
const STOP_POLL = 500;

export type UpdatePhase = "waiting" | "stopping" | "installing";

/** Whether this build can update itself: the desktop host, with an updater configured. */
export async function updatesEnabled(): Promise<boolean> {
  return isTauri() && (await invoke<boolean>("updates_enabled"));
}

export type InstallKind = "app-image" | "system-package" | "other";

/** How this build was installed; `other` outside the desktop host. */
export async function installKind(): Promise<InstallKind> {
  if (!isTauri()) return "other";
  return invoke<InstallKind>("install_kind");
}

export function useInstallKind() {
  return useQuery({ queryKey: keys.installKind, queryFn: installKind, retry: false });
}

/**
 * The offered update, or null when there is none, updates are disabled here, or this is a
 * .deb/.rpm install that its package manager updates.
 */
export async function findUpdate(): Promise<Update | null> {
  if (!(await updatesEnabled())) return null;
  if ((await installKind()) === "system-package") return null;
  return check();
}

export function useUpdate() {
  return useQuery({
    queryKey: keys.update,
    queryFn: findUpdate,
    refetchInterval: UPDATE_EVERY,
    retry: false,
  });
}

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** How the update's wait for running work is observed and cut short. */
export interface UpdateWait {
  /** Told what the drain is waiting on each time it is read. */
  onWaiting?: (readiness: UpdateReadiness) => void;
  /** True once the person chose "Update now": the wait ends and the daemon stops at once. */
  forced?: () => boolean;
  /** How often the drain is read. */
  pollMs?: number;
  /** How long the daemon may wait for running work, in seconds. */
  timeoutSecs?: number;
}

/** The daemon's own default wait for running work, `DEFAULT_DRAIN_TIMEOUT`. */
const DRAIN_SECS = 30 * 60;
const WAIT_POLL = 2_000;

/**
 * Download, wait for running work to settle, stop the núcleo, install, relaunch.
 *
 * The installer replaces the daemon's executable, so the daemon has to be gone first. It is asked
 * to drain (`POST /system/shutdown`), which refuses new work and stops once no run is live, so an
 * update no longer cuts a turn. While it waits the phase is `waiting` and `onWaiting` hears what
 * on. `forced()` turning true ends the wait through the immediate path (`/daemon/shutdown`).
 *
 * Nothing is installed unless the daemon is really down: a drain that timed out (the daemon
 * carries on, `draining` false) or a daemon that stays up throws. A shutdown request that cannot
 * reach the daemon is not fatal: the likeliest cause is that it is already down.
 */
export async function applyUpdate(
  update: Update,
  onPhase: (phase: UpdatePhase) => void,
  wait: UpdateWait = {},
): Promise<void> {
  await update.download();

  onPhase("waiting");
  let stopNow = false;
  try {
    const first = await apiFetch<UpdateReadiness>(
      `/system/shutdown?timeout_secs=${wait.timeoutSecs ?? DRAIN_SECS}`,
      { method: "POST" },
    );
    wait.onWaiting?.(first);
    for (;;) {
      if (wait.forced?.()) {
        stopNow = true;
        break;
      }
      await sleep(wait.pollMs ?? WAIT_POLL);
      if (wait.forced?.()) {
        stopNow = true;
        break;
      }
      let readiness: UpdateReadiness;
      try {
        readiness = await apiFetch<UpdateReadiness>("/system/update-readiness");
      } catch (error) {
        if (isApiUnavailable(error)) break; // the daemon went away: the check below confirms it
        throw error;
      }
      if (!readiness.draining) {
        throw new Error(
          "running work did not settle in time, so the daemon was left running and nothing was installed",
        );
      }
      wait.onWaiting?.(readiness);
    }
  } catch (error) {
    // Only "could not reach the daemon" is the already-down case; a refusal or a timed-out drain is not.
    if (!isApiUnavailable(error)) throw error;
  }

  onPhase("stopping");
  if (stopNow) {
    try {
      await apiText("/daemon/shutdown", { method: "POST" });
    } catch {
      // Already down, or going down: the wait below is the real check.
    }
  }
  const deadline = Date.now() + STOP_WAIT;
  let down = false;
  while (Date.now() < deadline) {
    if (!(await probeHealth())) {
      down = true;
      break;
    }
    await sleep(STOP_POLL);
  }
  if (!down) {
    throw new Error("the daemon is still running, so nothing was installed");
  }

  onPhase("installing");
  await update.install();
  await relaunch();
}
