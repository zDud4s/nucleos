import { useEffect, useRef } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { startBrowserBridge, useBrowserStore } from "../data/browser-store";

/**
 * Keeps the agent's browser reachable from anywhere in the app.
 *
 * It starts the store's bridge (the 1 s poll and the env-rebuilt listener) for
 * the life of the window, and when something asks to focus a browser session
 * while another page is open, it takes the person to /web so BrowserPage
 * mounts and consumes the intent itself. This component never consumes it.
 *
 * It renders nothing, and lives in the AppShell because the bridge is a fact
 * about the window, not about the page that happens to be showing.
 */
export function BrowserBridge() {
  const navigate = useNavigate();
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const { focusIntent } = useBrowserStore();
  const handled = useRef<number | null>(null);

  useEffect(() => startBrowserBridge(), []);

  useEffect(() => {
    if (focusIntent === null) return;
    if (pathname.startsWith("/web")) return;
    if (handled.current === focusIntent.nonce) return;
    handled.current = focusIntent.nonce;
    void navigate({ to: "/web" });
  }, [focusIntent, pathname, navigate]);

  return null;
}
