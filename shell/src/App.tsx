import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { checkHealth, getStatus, type ConnectionState } from "./api";
import Autopilot from "./Autopilot";
import Home from "./Home";
import "./App.css";

type Tab = "home" | "autopilot";

function App() {
  const [connection, setConnection] = useState<ConnectionState>("checking");
  const [token, setToken] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
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
        const s = await getStatus(daemonToken);
        if (!cancelled) setStatus(s);
      } else {
        setStatus(null);
      }
    };
    check();
    const interval = setInterval(check, 3000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  const connected = connection === "connected";

  return (
    <div className="shell-root">
      <header className="command">
        <span className="wordmark">NucleOS</span>
        <nav className="tabs" aria-label="NucleOS views">
          <button className="tab" type="button" aria-selected={tab === "home"} onClick={() => setTab("home")}>Home</button>
          <button className="tab" type="button" aria-selected={tab === "autopilot"} onClick={() => setTab("autopilot")}>Autopilot</button>
        </nav>
        <div className="right">
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
        <main className="page" data-state="normal" data-tab={tab}>
          {tab === "home" ? <Home connection={connection} status={status} /> : <Autopilot token={token} connection={connection} />}
        </main>
      )}
    </div>
  );
}

export default App;
