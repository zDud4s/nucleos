import type { ConnectionState } from "./api";

interface AutopilotProps {
  token: string | null;
  connection: ConnectionState;
}

function Autopilot({ token, connection }: AutopilotProps) {
  const unavailable = connection !== "connected" || token === null;

  return (
    <section>
      <h2>Autopilot</h2>
      {unavailable && (
        <p className="muted">
          Connect to the daemon (open the desktop app) to load Autopilot.
        </p>
      )}
    </section>
  );
}

export default Autopilot;
