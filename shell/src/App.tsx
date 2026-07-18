import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { checkHealth, getStatus, type ConnectionState } from "./api";
import "./App.css";

function App() {
  const [state, setState] = useState<ConnectionState>("checking");
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const check = async () => {
      const health = await checkHealth();
      if (cancelled) return;
      setState(health);
      if (health === "connected") {
        const token = await invoke<string>("get_daemon_token");
        const s = await getStatus(token);
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
      <p data-testid="connection-state">Núcleo: {state}</p>
      {status && <p data-testid="daemon-status">{status}</p>}
    </main>
  );
}

export default App;
