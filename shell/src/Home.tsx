import type { ConnectionState } from "./api";

interface HomeProps {
  connection: ConnectionState;
  status: string | null;
}

function Home({ connection, status }: HomeProps) {
  const connected = connection === "connected";
  return (
    <section className="home-digest">
      <h1 className="headline">
        {connected && status ? <>Good to have you here. <span className="ok">The núcleo is live.</span></> : connected ? <>The núcleo is connected. Its status is checking in.</> : <>The núcleo is checking in.</>}
      </h1>
      <div className="statusline">
        <span data-testid="connection-state">connection <b>{connection}</b></span>
        <span data-testid="daemon-status">daemon <b>{status ?? "waiting for status"}</b></span>
      </div>
      <div className="teach">
        <span className="t-title">Your operating picture will appear here.</span>
        Once Autopilot has projects and activity to report, this is where the quiet summary lands.
      </div>
    </section>
  );
}

export default Home;
