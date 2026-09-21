/**
 * The three global chords, registered with the host once per launch — from the shell, not a page.
 *
 * The host does not register them itself: they are configured in `.ai/voice.yaml`, which only the
 * daemon reads, so the webview reads `GET /voice/config` and hands them over
 * (`shell/src-tauri/src/lib.rs` says the same from the other side). Until 2026-09-21 the Voice page
 * did that handing over, which meant a "global" chord did not exist until somebody had opened the
 * Voice page since the app started. The conversation chord is answered from every page now
 * (`app/ConversationChord.tsx`), and a chord answered everywhere and registered from one page is
 * still a chord that works on one page. So the shell registers them, and the page reads the outcome.
 *
 * A query rather than an effect because two components ask and one registration must happen: the
 * shell mounts it always and the Voice page mounts it to show what came back. The chords are the key,
 * so a changed chord in the configuration is a new registration and the same chords are the cached
 * answer. Its key sits outside `keys.voice` on purpose — that namespace is the daemon's routes, and
 * invalidating it must not re-run a host call that unregisters every chord before it registers any.
 */

import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";

import { useVoiceConfig } from "./voice";

export interface HotkeyRegistration {
  /** The host's sentence when this desktop gives out no global hotkeys at all (Wayland). */
  unavailable: string | null;
  /** The chords another application already holds, or `null` when registration was not attempted. */
  conflicts: string[] | null;
  /** Whether the registration call itself failed, as opposed to answering with conflicts. */
  failed: boolean;
}

async function register(dictation: string, memo: string, conversation: string): Promise<HotkeyRegistration> {
  // Asked FIRST, and in the same call, because on Wayland the answer is a sentence and the
  // registration must not happen. Sequenced here, "there is a sentence" and "nothing was registered"
  // are the same decision rather than two that could disagree.
  //
  // A host that does not know the command is a host with nothing to refuse, and so is one that
  // answers `undefined` rather than `null` — both mean "register them". Otherwise the shell would
  // silently stop registering hotkeys the day it runs against an older host.
  const unavailable = (await invoke<string | null>("voice_hotkeys_unavailable").catch(() => null)) ?? null;
  if (unavailable !== null) return { unavailable, conflicts: null, failed: false };
  try {
    // All three in ONE call, because the host unregisters everything before it registers anything —
    // a call naming only one chord would silently drop the other two.
    const conflicts = await invoke<string[]>("voice_register_hotkeys", { dictation, memo, conversation });
    return { unavailable: null, conflicts, failed: false };
  } catch {
    return { unavailable: null, conflicts: null, failed: true };
  }
}

/** The outcome of registering the chords, or `undefined` until the configuration has named them. */
export function useHotkeyRegistration(): HotkeyRegistration | undefined {
  const config = useVoiceConfig();
  const dictation = config.data?.hotkey;
  const memo = config.data?.memo_hotkey;
  const conversation = config.data?.conversation_hotkey ?? "";
  const known = dictation !== undefined && memo !== undefined;

  const registration = useQuery({
    queryKey: ["host", "hotkeys", dictation, memo, conversation] as const,
    queryFn: () => register(dictation ?? "", memo ?? "", conversation),
    enabled: known,
    // Registered once per set of chords, and never again on a timer, a focus or a remount: every
    // call unregisters all three before registering them, so a refetch is a moment with no chords.
    staleTime: Infinity,
    gcTime: Infinity,
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  return registration.data;
}
