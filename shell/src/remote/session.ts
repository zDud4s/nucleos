/**
 * The three calls the gateway answers before there is an app: who am I, pair, sign out.
 *
 * Plain `fetch` rather than the data client, which assumes a session. Every request carries the CSRF
 * header the gateway requires on `/auth/*`, and the cookie travels by itself (HttpOnly).
 */
export interface RemoteSession {
  deviceId: string;
  name: string;
  accessLevel: string;
  expiresAt: string;
  /** An owner device: the only kind that may register a passkey. */
  owner: boolean;
}

export type SessionResult = { kind: "ok"; session: RemoteSession } | { kind: "none" } | { kind: "network" };

export type PairResult =
  | { kind: "ok"; session: RemoteSession }
  | { kind: "invalid_code" }
  | { kind: "slow_down"; retryAfter: number | null }
  | { kind: "network" };

const HEADERS = { "X-Nucleos-Csrf": "1" };

interface SessionWire {
  device_id?: string;
  name?: string;
  access_level?: string;
  expires_at?: string;
  owner?: boolean;
}

function toSession(wire: SessionWire): RemoteSession {
  return {
    deviceId: wire.device_id ?? "",
    name: wire.name ?? "",
    accessLevel: wire.access_level ?? "",
    expiresAt: wire.expires_at ?? "",
    owner: wire.owner === true,
  };
}

async function readJson(response: Response): Promise<SessionWire> {
  try {
    return ((await response.json()) ?? {}) as SessionWire;
  } catch {
    return {};
  }
}

export async function fetchSession(): Promise<SessionResult> {
  try {
    const response = await fetch("/auth/session", { credentials: "same-origin", headers: HEADERS });
    if (response.status === 401) return { kind: "none" };
    if (!response.ok) return { kind: "network" };
    return { kind: "ok", session: toSession(await readJson(response)) };
  } catch {
    return { kind: "network" };
  }
}

export async function pair(code: string, deviceName: string): Promise<PairResult> {
  try {
    const body: { code: string; device_name?: string } = { code };
    if (deviceName.trim() !== "") body.device_name = deviceName.trim();
    const response = await fetch("/auth/pair", {
      method: "POST",
      credentials: "same-origin",
      headers: { ...HEADERS, "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (response.status === 429) {
      const seconds = Number.parseInt(response.headers.get("Retry-After") ?? "", 10);
      return { kind: "slow_down", retryAfter: Number.isFinite(seconds) ? seconds : null };
    }
    if (response.status === 401) return { kind: "invalid_code" };
    if (!response.ok) return { kind: "network" };
    return { kind: "ok", session: toSession(await readJson(response)) };
  } catch {
    return { kind: "network" };
  }
}

export async function logout(): Promise<void> {
  try {
    await fetch("/auth/logout", { method: "POST", credentials: "same-origin", headers: HEADERS });
  } catch {
    // Signing out of a gateway that cannot be reached still leaves this page; the cookie expires.
  }
}
