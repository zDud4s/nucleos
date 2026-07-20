import type { ConnectionState } from "./api";

interface HomeProps {
  connection: ConnectionState;
  status: string | null;
}

function Home({ connection, status }: HomeProps) {
  return (
    <>
      <p data-testid="connection-state">Núcleo: {connection}</p>
      {status && <p data-testid="daemon-status">{status}</p>}
    </>
  );
}

export default Home;
