const DAEMON_URL = "http://127.0.0.1:8791";

export type ConnectionState = "checking" | "connected" | "disconnected";

export async function checkHealth(): Promise<ConnectionState> {
  try {
    const res = await fetch(`${DAEMON_URL}/health`);
    return res.ok ? "connected" : "disconnected";
  } catch {
    return "disconnected";
  }
}

export async function getStatus(token: string): Promise<string | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/status`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.text();
  } catch {
    return null;
  }
}
