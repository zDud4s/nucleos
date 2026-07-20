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

  return (
    <main className="container">
      <h1>NucleOS</h1>
      <nav className="tabs" aria-label="NucleOS views">
        <button
          className={`tab${tab === "home" ? " active" : ""}`}
          type="button"
          onClick={() => setTab("home")}
        >
          Home
        </button>
        <button
          className={`tab${tab === "autopilot" ? " active" : ""}`}
          type="button"
          onClick={() => setTab("autopilot")}
        >
          Autopilot
        </button>
      </nav>
      {tab === "home" ? (
        <Home connection={connection} status={status} />
      ) : (
        <Autopilot token={token} connection={connection} />
      )}
    </main>
  );
}

export default App;
