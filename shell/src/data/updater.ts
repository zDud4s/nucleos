import { invoke, isTauri } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { useQuery } from "@tanstack/react-query";
import { apiText, probeHealth } from "./client";
import { keys } from "./keys";

/** How often the shell asks the release feed whether something newer exists. */
export const UPDATE_EVERY = 6 * 60 * 60 * 1000;

/** How long the install waits for the núcleo to let go of its files. */
const STOP_WAIT = 15_000;
const STOP_POLL = 500;

export type UpdatePhase = "stopping" | "installing";

/** Whether this build can update itself: the desktop host, with an updater configured. */
export async function updatesEnabled(): Promise<boolean> {
  return isTauri() && (await invoke<boolean>("updates_enabled"));
}

/** The offered update, or null when there is none or updates are disabled here. */
export async function findUpdate(): Promise<Update | null> {
  if (!(await updatesEnabled())) return null;
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

/**
 * Download, stop the núcleo, install, relaunch.
 *
 * The installer replaces the daemon's executable, so the daemon has to be gone
 * first. A failed shutdown request is not fatal: the likeliest cause is that it
 * is already down.
 */
export async function applyUpdate(
  update: Update,
  onPhase: (phase: UpdatePhase) => void,
): Promise<void> {
  await update.download();

  onPhase("stopping");
  try {
    await apiText("/daemon/shutdown", { method: "POST" });
  } catch {
    // Already down, or going down: either way the wait below is the real check.
  }
  const deadline = Date.now() + STOP_WAIT;
  while (Date.now() < deadline) {
    if (!(await probeHealth())) break;
    await sleep(STOP_POLL);
  }

  onPhase("installing");
  await update.install();
  await relaunch();
}
