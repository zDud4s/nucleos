import { invoke } from "@tauri-apps/api/core";

/**
 * The núcleo, on loopback. There is no second daemon URL for the desktop shell:
 * the shell and the core are the same install, talking over 127.0.0.1.
 */
export const DAEMON_URL = "http://127.0.0.1:8791";

/**
 * How the client reaches the daemon.
 *
 * The desktop shell talks to loopback with the control token; the remote shell
 * talks same-origin to the gateway with a cookie. Everything else in the client
 * (refusals, unavailability, streams) is identical, so only this seam differs.
 */
export interface Transport {
  /** Prefix for every path: the daemon URL, or `/api` behind the gateway. */
  baseUrl: string;
  credentials: RequestCredentials;
  /** The headers that authenticate a request. A rejection means no credential could be had. */
  headers(): Promise<Record<string, string>>;
  /**
   * Called after a 401. `"retry"` asks the client to send the request once more
   * (the credential was refreshed); `"fail"` makes the 401 final.
   */
  onUnauthorized(): Promise<"retry" | "fail">;
  /** What the connection screen names instead of a literal URL. */
  label: string;
  /**
   * When set, a final 401 is reported under this refusal code instead of the
   * generic status derivation (the remote session is gone, not a token stale).
   */
  unauthorizedCode?: string;
  /**
   * Headers for the credential-free probes (`/health`). Absent means the probe
   * is sent bare, exactly as the desktop always did.
   */
  probeHeaders?: Record<string, string>;
}

/**
 * The credential, fetched once for the life of the window.
 *
 * The token lives in the OS Credential Manager and the daemon rotates it on
 * restart, not per request. The promise is memoised rather than the string, so
 * a burst of calls at startup shares one read.
 *
 * A *rejection* is not memoised: a keychain can be momentarily locked, and a
 * cached rejection would turn a lock that lasts one second into a session that
 * can never authenticate.
 */
let tokenRequest: Promise<string> | null = null;

function daemonToken(): Promise<string> {
  if (tokenRequest === null) {
    const attempt = invoke<string>("get_daemon_token");
    attempt.catch(() => {
      if (tokenRequest === attempt) tokenRequest = null;
    });
    tokenRequest = attempt;
  }
  return tokenRequest;
}

/**
 * A token read that failed, before the client has its error classes in scope.
 * The client turns it into `ApiUnavailable("token", ...)`; this module cannot
 * import the client without a cycle.
 */
export class TokenReadError extends Error {
  readonly reason: unknown;

  constructor(reason: unknown) {
    super("the daemon token could not be read");
    this.name = "TokenReadError";
    this.reason = reason;
  }
}

export const desktopTransport: Transport = {
  baseUrl: DAEMON_URL,
  credentials: "same-origin",
  async headers() {
    let token: string;
    try {
      token = await daemonToken();
    } catch (cause) {
      throw new TokenReadError(cause);
    }
    return { Authorization: `Bearer ${token}` };
  },
  // A 401 most likely means the daemon restarted and rotated its token, so the
  // memoised one is stale: drop it and ask for one retry. The client retries
  // once only; a second 401 is a real refusal.
  async onUnauthorized() {
    tokenRequest = null;
    return "retry";
  },
  label: DAEMON_URL,
};

let current: Transport = desktopTransport;

export function setTransport(transport: Transport): void {
  current = transport;
}

export function getTransport(): Transport {
  return current;
}

type SessionLostListener = () => void;

const sessionLostListeners = new Set<SessionLostListener>();

/** Subscribe to "the remote session is gone". Returns the unsubscribe. */
export function onSessionLost(listener: SessionLostListener): () => void {
  sessionLostListeners.add(listener);
  return () => {
    sessionLostListeners.delete(listener);
  };
}

export function emitSessionLost(): void {
  for (const listener of [...sessionLostListeners]) {
    try {
      listener();
    } catch (error) {
      console.error("transport: a session-lost listener threw", error);
    }
  }
}
