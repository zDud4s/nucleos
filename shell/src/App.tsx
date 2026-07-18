import { useEffect, useState } from "react";
import "./App.css";

type ConnectionState = "checking" | "connected" | "disconnected";

function App() {
  const [state, setState] = useState<ConnectionState>("checking");

  useEffect(() => {
    let cancelled = false;
    const check = async () => {
      try {
        const res = await fetch("http://127.0.0.1:8791/health");
        if (!cancelled) setState(res.ok ? "connected" : "disconnected");
      } catch {
        if (!cancelled) setState("disconnected");
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
    </main>
  );
}

export default App;
