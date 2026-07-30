import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  checkHealth, getKillSwitch, getStatus, sendAttentionHeartbeat, setKillSwitch,
  type ConnectionState,
} from "./api";
import Assistant, { type Turn } from "./Assistant";
import Autopilot from "./Autopilot";
import Home from "./Home";
import Mail from "./Mail";
import Projects from "./Projects";
import Runs from "./Runs";
import System from "./System";
import { Button, ConfirmButton } from "./ui";
import "./App.css";

type Tab = "home" | "autopilot" | "runs" | "projects" | "assistant" | "mail" | "system";

const TABS: { key: Tab; label: string }[] = [
  { key: "home", label: "Home" },
  { key: "autopilot", label: "Autopilot" },
  { key: "runs", label: "Runs" },
  { key: "projects", label: "Projects" },
  { key: "assistant", label: "Assistant" },
  { key: "mail", label: "Mail" },
  { key: "system", label: "System" },
];

/**
 * What went wrong reading the daemon token, in the words the OS used. The
 * keyring error ("No matching entry…", "The user cancelled…") is the only
 * thing that tells the user whether to unlock a keychain or run the daemon's
 * setup, so it is shown rather than swallowed.
 */
function credentialFailure(error: unknown): string {
  const detail =
    typeof error === "string" ? error
      : error instanceof Error ? error.message
      : String(error);
  return `Could not read the daemon token from the credential manager — ${detail}`;
}

function App() {
  const [connection, setConnection] = useState<ConnectionState>("checking");
  const [token, setToken] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [killEngaged, setKillEngaged] = useState<boolean | null>(null);
  const [killBusy, setKillBusy] = useState(false);
  const [tab, setTab] = useState<Tab>("home");
  /** Why a reachable daemon still can't be used — the one failure a retry can't clear on its own. */
  const [blocked, setBlocked] = useState<string | null>(null);
  /**
   * The assistant's transcript, held here rather than in the page that draws it.
   *
   * Tabs render one page at a time, so leaving the assistant unmounts it — and with the transcript
   * in its own state, the message you had just sent disappeared, along with the poll that was
   * waiting for its answer. Owning it at this level costs nothing and is what makes coming back to
   * the tab show the conversation you left.
   */
  const [assistantTurns, setAssistantTurns] = useState<Turn[]>([]);
  const tokenRequest = useRef<Promise<string> | null>(null);
  const polling = useRef(false);

  useEffect(() => {
    let cancelled = false;

    // The credential read is shared across ticks so a healthy start costs one
    // keychain hit — but only while it SUCCEEDS. Caching a rejected promise
    // makes a momentarily locked keychain permanent for the session and
    // re-throws the same rejection at every 3-second tick forever.
    const readToken = () => {
      if (tokenRequest.current === null) {
        const attempt = invoke<string>("get_daemon_token");
        attempt.catch(() => {
          if (tokenRequest.current === attempt) tokenRequest.current = null;
        });
        tokenRequest.current = attempt;
      }
      return tokenRequest.current;
    };

    const check = async () => {
      // One round at a time: a slow daemon would otherwise stack a fresh batch
      // every 3 seconds and let the answers land out of order.
      if (polling.current) return;
      polling.current = true;
      try {
        const health = await checkHealth();
        if (cancelled) return;
        setConnection(health);
        if (health !== "connected") {
          setStatus(null);
          setKillEngaged(null);
          setBlocked(null);
          return;
        }

        let daemonToken: string;
        try {
          daemonToken = await readToken();
        } catch (error) {
          if (cancelled) return;
          setToken(null);
          setStatus(null);
          setKillEngaged(null);
          setBlocked(credentialFailure(error));
          return;
        }
        if (cancelled) return;
        setToken(daemonToken);

        const [nextStatus, nextKill] = await Promise.all([
          getStatus(daemonToken),
          getKillSwitch(daemonToken),
        ]);
        if (cancelled) return;
        if (!nextStatus.ok && nextStatus.fault === "unauthorized") {
          // The daemon answered and refused us: this is a stale token, not an
          // outage, and no amount of retrying the same token fixes it. Drop
          // the cached one so the next round re-reads the credential manager,
          // which is where a rotated token would already be waiting.
          tokenRequest.current = null;
          setToken(null);
          setStatus(null);
          setKillEngaged(null);
          setBlocked(
            "The daemon rejected the stored token. It was probably rotated — restart the núcleo, or re-run its setup so the credential manager holds the current one.",
          );
          return;
        }
        setBlocked(null);
        setStatus(nextStatus.ok ? nextStatus.value : null);
        setKillEngaged(nextKill);
      } finally {
        polling.current = false;
      }
    };
    void check();
    const interval = setInterval(() => void check(), 3000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  /**
   * Tells the daemon someone is watching.
   *
   * Deliberately separate from the health poll rather than folded into it. The poll runs whether or
   * not a person is there, and `attention.rs` refuses to infer presence from API traffic for exactly
   * that reason — a presence signal derived from our own polling would mark the owner permanently
   * present and stop autonomous work forever.
   *
   * So it is sent only while the window is actually VISIBLE. A minimised shell is not a foreground
   * client, and the daemon's window is 120 seconds, so a beat every 30 survives a couple of missed
   * ones and expires on its own within two minutes of the window being hidden or closed.
   *
   * This is a real behavioural change and not just a screen: with the shell in front of you,
   * autonomous starts are held back, which is the brake the design intended and that nothing was
   * previously arming.
   */
  useEffect(() => {
    if (token === null || connection !== "connected") return;
    const beat = () => {
      if (document.visibilityState !== "visible") return;
      void sendAttentionHeartbeat(token);
    };
    beat();
    const id = setInterval(beat, 30000);
    // Coming back to the window should register immediately rather than at the next tick, since
    // that is the moment presence actually changed.
    document.addEventListener("visibilitychange", beat);
    return () => {
      clearInterval(id);
      document.removeEventListener("visibilitychange", beat);
    };
  }, [connection, token]);

  const toggleKill = useCallback(
    async (engaged: boolean) => {
      if (token === null) return;
      setKillBusy(true);
      await setKillSwitch(token, engaged);
      const confirmed = await getKillSwitch(token);
      setKillEngaged(confirmed);
      setKillBusy(false);
    },
    [token],
  );

  const connected = connection === "connected";
  // Reachable is not the same as usable: without a token the daemon controls
  // below would all fail, so they are not offered.
  const usable = connected && blocked === null;

  return (
    <div className="shell-root">
      <header className="command">
        <span className="wordmark">NucleOS</span>
        <nav className="tabs" aria-label="NucleOS views">
          {TABS.map((entry) => (
            <button
              key={entry.key}
              className="tab"
              type="button"
              aria-current={tab === entry.key ? "page" : undefined}
              onClick={() => setTab(entry.key)}
            >
              {entry.label}
            </button>
          ))}
        </nav>
        <div className="right">
          {usable && (
            <div className="kill">
              {killEngaged === true ? (
                <ConfirmButton
                  variant="danger-solid"
                  size="sm"
                  confirmLabel="Confirm disengage?"
                  disabled={killBusy}
                  onConfirm={() => void toggleKill(false)}
                >
                  Disengage kill switch
                </ConfirmButton>
              ) : (
                <>
                  <span>emergency stop</span>
                  <Button
                    variant="danger"
                    size="sm"
                    title="Stops every run, parks schedulers, and makes approvals read-only. Takes effect immediately."
                    disabled={killBusy || killEngaged === null}
                    onClick={() => void toggleKill(true)}
                  >
                    Kill switch
                  </Button>
                </>
              )}
            </div>
          )}
          <span className={`conn ${usable ? "online" : "offline"}`}>
            {!connected ? "daemon unreachable — retrying"
              : blocked !== null ? "daemon reachable — not authorised"
              : "daemon connected"}
          </span>
        </div>
      </header>
      {!connected ? (
        <section className="offline-hero">
          <span className="dot" />
          <h1>The núcleo isn&apos;t running.</h1>
          <p>The shell can&apos;t reach the local daemon. Start the NucleOS desktop app; it retries every 3 seconds.</p>
          <p>Nothing was lost: paused runs stay parked, and the approval queue will be where you left it.</p>
        </section>
      ) : blocked !== null ? (
        <section className="offline-hero">
          <span className="dot" />
          <h1>The núcleo is running, but won&apos;t take orders from here.</h1>
          <p>{blocked}</p>
          <p>The shell keeps trying every 3 seconds; nothing is lost while it can&apos;t get in, and no run was affected.</p>
        </section>
      ) : (
        <main className="page" data-tab={tab}>
          {tab === "home" && (
            <Home
              token={token}
              connection={connection}
              status={status}
              killEngaged={killEngaged}
              onOpenAutopilot={() => setTab("autopilot")}
            />
          )}
          {tab === "autopilot" && (
            <Autopilot
              token={token}
              connection={connection}
              killEngaged={killEngaged}
              killBusy={killBusy}
              toggleKill={toggleKill}
            />
          )}
          {tab === "runs" && <Runs token={token} connection={connection} />}
          {tab === "projects" && <Projects token={token} connection={connection} />}
          {tab === "assistant" && (
            <Assistant
              token={token}
              connection={connection}
              turns={assistantTurns}
              setTurns={setAssistantTurns}
            />
          )}
          {tab === "mail" && <Mail token={token} connection={connection} />}
          {tab === "system" && <System token={token} connection={connection} />}
        </main>
      )}
    </div>
  );
}

export default App;
