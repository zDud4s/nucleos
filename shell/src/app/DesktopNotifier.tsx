import { useEffect, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useChats } from "../data/chats";
import { EMPTY_SEEN, toastsFor, type Toast } from "../data/desktop-notify";
import { useDesktopNotify } from "../data/desktop-notify-pref";
import { useRecentFeed, type NotifyPolicy } from "../data/feed";
import { useNotifyPolicy, useProposals } from "../data/system";
import { inTauri, sendDesktopToast } from "./desktop-toast";

/** What the notify policy says when it has not been read: everything passes. */
const OPEN: NotifyPolicy = { families: [], kinds: [] };

/**
 * How long a toast's destination is kept for the window's next return. A window brought back
 * hours later is not answering a toast, and jumping to a stale page would be a surprise.
 */
const ACT_ON_MS = 2 * 60 * 1000;

function windowInFront(): boolean {
  return document.visibilityState === "visible" && document.hasFocus();
}

/**
 * Whether the window is in front of the owner: visible and holding focus. A window minimized,
 * hidden in the tray or behind another is not, and that is when a toast has a job to do.
 *
 * The webview's own events cover the browser preview; inside the desktop host the window's
 * focus event is added, because a webview does not always hear that its window was covered.
 */
function useWindowInFront(): boolean {
  const [front, setFront] = useState(windowInFront);

  useEffect(() => {
    const read = () => setFront(windowInFront());
    window.addEventListener("focus", read);
    window.addEventListener("blur", read);
    document.addEventListener("visibilitychange", read);

    let unlisten: (() => void) | undefined;
    let gone = false;
    if (inTauri()) {
      getCurrentWindow()
        .onFocusChanged(({ payload }) => setFront(payload && document.visibilityState === "visible"))
        .then((fn) => {
          if (gone) fn();
          else unlisten = fn;
        })
        .catch(() => {});
    }
    return () => {
      gone = true;
      unlisten?.();
      window.removeEventListener("focus", read);
      window.removeEventListener("blur", read);
      document.removeEventListener("visibilitychange", read);
    };
  }, []);

  return front;
}

/**
 * The second channel beside Telegram: a toast from the operating system when something wants the
 * owner and the window is not in front.
 *
 * It reads the lists the badge and the drawer already keep alive — no request of its own — and
 * leaves every decision to `toastsFor`. The policy is the one Telegram obeys.
 *
 * The toast's click does not reach the page on every platform, so the destination of the last
 * toast is kept and the window goes there when it next comes to the front.
 *
 * It renders nothing, and it lives in the AppShell for the reason `AttentionHeartbeat` does: it
 * is a fact about the window, not about a page.
 */
export function DesktopNotifier({ send = sendDesktopToast }: { send?: (toast: Toast) => unknown }) {
  const navigate = useNavigate();
  const [enabled] = useDesktopNotify();
  const front = useWindowInFront();

  const proposals = useProposals();
  const feed = useRecentFeed();
  const chats = useChats();
  const policy = useNotifyPolicy();

  const seen = useRef(EMPTY_SEEN);
  const outstanding = useRef<{ to: string; at: number } | null>(null);

  useEffect(() => {
    const out = toastsFor({
      focused: front,
      enabled,
      policy: policy.data ?? OPEN,
      proposals: proposals.data,
      // The feed waits for the policy to answer, or a kind the owner silenced could speak once on
      // the way in. A failed read falls to "everything passes", as the sidecar's does.
      feed: policy.isPending ? undefined : feed.data,
      chats: chats.data,
      seen: seen.current,
    });
    seen.current = out.seen;

    for (const toast of out.toasts) void Promise.resolve(send(toast)).catch(() => {});
    const last = out.toasts[out.toasts.length - 1];
    if (last) outstanding.current = { to: last.to, at: Date.now() };
  }, [front, enabled, policy.data, policy.isPending, proposals.data, feed.data, chats.data, send]);

  useEffect(() => {
    if (!front || outstanding.current === null) return;
    const { to, at } = outstanding.current;
    outstanding.current = null;
    if (Date.now() - at <= ACT_ON_MS) void navigate({ to });
  }, [front, navigate]);

  return null;
}
