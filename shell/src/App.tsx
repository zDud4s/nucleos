import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  checkHealth, getKillSwitch, getStatus, setKillSwitch, type ConnectionState,
} from "./api";
import Autopilot from "./Autopilot";
import Home from "./Home";
import { Button, ConfirmButton } from "./ui";
import "./App.css";

type Tab = "home" | "autopilot";

function App() {
  const [connection, setConnection] = useState<ConnectionState>("checking");
  const [token, setToken] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [killEngaged, setKillEngaged] = useState<boolean | null>(null);
  const [killBusy, setKillBusy] = useState(false);
  const [tab, setTab] = useState<Tab>("home");
  const tokenRequest = useRef<Promise<string> | null>(null);

  useEffect(() => {
    let cancelled = false;
    const check = async () => {
      const health = await checkHealth();
      if (cancelled) return;
      setConnection(health);
      if (health === "connected") {
        if (tokenRequest.current === null) {
          tokenRequest.current = invoke<string>("get_daemon_token");
        }
        const daemonToken = await tokenRequest.current;
        if (cancelled) return;
        setToken(daemonToken);
        const [nextStatus, nextKill] = await Promise.all([
          getStatus(daemonToken),
          getKillSwitch(daemonToken),
        ]);
        if (cancelled) return;
        setStatus(nextStatus);
        setKillEngaged(nextKill);
      } else {
        setStatus(null);
        setKillEngaged(null);
      }
    };
    check();
    const interval = setInterval(check, 3000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

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

  return (
    <div className="shell-root">
      <header className="command">
        <span className="wordmark">NucleOS</span>
        <nav className="tabs" aria-label="NucleOS views">
          <button className="tab" type="button" aria-current={tab === "home" ? "page" : undefined} onClick={() => setTab("home")}>Home</button>
          <button className="tab" type="button" aria-current={tab === "autopilot" ? "page" : undefined} onClick={() => setTab("autopilot")}>Autopilot</button>
        </nav>
        <div className="right">
          {connected && (
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
          <span className={`conn ${connected ? "online" : "offline"}`}>
            {connected ? "daemon connected" : "daemon unreachable — retrying"}
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
      ) : (
        <main className="page" data-tab={tab}>
          {tab === "home"
            ? <Home
                token={token}
                connection={connection}
                status={status}
                killEngaged={killEngaged}
                onOpenAutopilot={() => setTab("autopilot")}
              />
            : <Autopilot
                token={token}
                connection={connection}
                killEngaged={killEngaged}
                killBusy={killBusy}
                toggleKill={toggleKill}
              />}
        </main>
      )}
    </div>
  );
}

export default App;
