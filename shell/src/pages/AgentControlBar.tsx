// §spec pilar-de-browser
import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { embeddedKeep, embeddedOwner } from "../data/browser-embedded";
import type { Tab } from "../data/browser-tabs";
import { Button } from "../ui";

type Mode =
  | { kind: "agent" }
  | { kind: "human" }
  | { kind: "keep"; rowId: number; hosts: string[] };

// Per-session control state, kept at module level so it survives the bar being unmounted when the
// person switches tabs or leaves the page. Keyed by session id.
const modes = new Map<string, Mode>();

/** Test hook: forget every remembered session state. */
export function resetAgentControlState(): void {
  modes.clear();
}

function hostsOf(chain: string[]): string[] {
  const seen = new Set<string>();
  for (const entry of chain) {
    let host = entry;
    try {
      host = new URL(entry).host || entry;
    } catch {
      // Not a URL: show it as it came.
    }
    seen.add(host);
  }
  return [...seen];
}

function refusal(action: string, e: unknown): string {
  if (isApiRefusal(e)) {
    if (e.code === "pillar_off") return `Could not ${action} (pillar is off)`;
    if (e.status === 404) return "Session is gone";
  }
  return `Could not ${action}`;
}

type Props = {
  tab: Tab;
  highlight: boolean;
  onOpenInMyTabs: (tab: Tab) => void;
};

/**
 * The strip above an agent tab: who drives it, and the hand-over between the agent and the person.
 *
 * There is deliberately no React-level input gating: the agent webview is a native window outside
 * the DOM, so once the person takes control they can already type and click in it, and nothing in
 * this bar needs to block that. "Take control" only tells the daemon who owns the session.
 */
export function AgentControlBar({ tab, highlight, onOpenInMyTabs }: Props) {
  const sessionId = tab.sessionId;
  const [mode, setModeState] = useState<Mode>(
    () => (sessionId === undefined ? undefined : modes.get(sessionId)) ?? { kind: "agent" },
  );
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  function setMode(next: Mode) {
    if (sessionId !== undefined) modes.set(sessionId, next);
    setModeState(next);
  }

  async function run(work: () => Promise<void>, action: string) {
    setBusy(true);
    setNotice(null);
    try {
      await work();
    } catch (e: unknown) {
      setNotice(refusal(action, e));
    } finally {
      setBusy(false);
    }
  }

  const takeControl = () =>
    run(async () => {
      if (sessionId === undefined) return;
      await embeddedOwner(sessionId, "human");
      setMode({ kind: "human" });
    }, "take control");

  const giveBack = () =>
    run(async () => {
      if (sessionId === undefined) return;
      const reply = await embeddedOwner(sessionId, "agent");
      if (reply !== undefined && reply.chain.length > 0) {
        setMode({ kind: "keep", rowId: reply.row_id, hosts: hostsOf(reply.chain) });
      } else {
        setMode({ kind: "agent" });
      }
    }, "give back");

  const answer = (rowId: number, keep: boolean, writable: boolean) =>
    run(async () => {
      await embeddedKeep(rowId, keep, writable);
      setMode({ kind: "agent" });
    }, "save the choice");

  return (
    <div className="browser-agent-bar" data-mode={mode.kind}>
      {mode.kind === "agent" ? (
        <>
          <span>Agent is driving</span>
          <Button
            type="button"
            disabled={busy || sessionId === undefined}
            data-highlight={highlight ? "true" : undefined}
            onClick={() => void takeControl()}
          >
            Take control
          </Button>
        </>
      ) : null}
      {mode.kind === "human" ? (
        <>
          <span>You are driving</span>
          <Button type="button" disabled={busy} onClick={() => void giveBack()}>
            Give back to agent
          </Button>
          <Button type="button" disabled={busy} onClick={() => onOpenInMyTabs(tab)}>
            Open in my tabs
          </Button>
        </>
      ) : null}
      {mode.kind === "keep" ? (
        <>
          <span>Keep these sites?</span>
          <ul className="browser-agent-hosts">
            {mode.hosts.map((host) => (
              <li key={host}>{host}</li>
            ))}
          </ul>
          <Button type="button" disabled={busy} onClick={() => void answer(mode.rowId, true, false)}>
            Keep
          </Button>
          <Button type="button" disabled={busy} onClick={() => void answer(mode.rowId, true, true)}>
            Keep, and allow forms
          </Button>
          <Button type="button" disabled={busy} onClick={() => void answer(mode.rowId, false, false)}>
            No
          </Button>
        </>
      ) : null}
      {notice !== null ? (
        <span className="browser-agent-notice" role="status">
          {notice}
        </span>
      ) : null}
    </div>
  );
}
