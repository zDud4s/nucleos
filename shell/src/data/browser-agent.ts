// Agent-browser glue: ensures the agent's browser env, polls embedded-tab
// requests from the daemon, and reports closed agent tabs back.
import { invoke } from "@tauri-apps/api/core";
import {
  embeddedClosed,
  embeddedPrepare,
  embeddedRegister,
  embeddedRequests,
  type EmbeddedRequest,
} from "./browser-embedded";
import { scopeProject } from "./browser-tabs";

export type AgentEnvResult =
  | { ok: true; cdpHttp: string }
  | { ok: false; reason: string };

const inFlightEnv = new Map<string, Promise<AgentEnvResult>>();
// Fresh runs queued behind an in-flight one and not started yet, so a burst of fresh requests
// shares one queued run.
const queuedFresh = new Map<string, Promise<AgentEnvResult>>();

function messageOf(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e) ?? String(e);
  } catch {
    return String(e);
  }
}

async function runEnsure(key: string): Promise<AgentEnvResult> {
  try {
    const { proxy_addr } = await embeddedPrepare(key);
    const env = await invoke<{ cdp_http: string }>("browser_env_ensure", {
      project: scopeProject(key),
      role: "agent",
      proxyAddr: proxy_addr,
    });
    await embeddedRegister(key, env.cdp_http);
    return { ok: true, cdpHttp: env.cdp_http };
  } catch (e) {
    return { ok: false, reason: messageOf(e) };
  }
}

/**
 * Prepare the agent browser env for a scope key (a project id or `NO_PROJECT`). Never throws;
 * concurrent calls share one run.
 *
 * With `fresh`, an in-flight run is not joined: a new run starts after it settles. Use it when
 * the env the in-flight run may be registering is already known to be gone (env retired).
 */
export function ensureAgentEnv(
  project: string,
  opts?: { fresh?: boolean },
): Promise<AgentEnvResult> {
  const existing = inFlightEnv.get(project);
  if (existing && !opts?.fresh) return existing;
  if (existing) {
    const queued = queuedFresh.get(project);
    if (queued) return queued;
  }
  // `existing` never rejects (runEnsure catches), so chaining on it always starts the new run.
  let p: Promise<AgentEnvResult>;
  const run = existing
    ? existing.then(() => {
        if (queuedFresh.get(project) === p) queuedFresh.delete(project);
        return runEnsure(project);
      })
    : runEnsure(project);
  p = run.finally(() => {
    if (inFlightEnv.get(project) === p) inFlightEnv.delete(project);
  });
  inFlightEnv.set(project, p);
  if (existing) queuedFresh.set(project, p);
  return p;
}

export type AgentPollerHandlers = {
  onOpen(req: EmbeddedRequest & { url: string }): Promise<void> | void;
  onClose(req: EmbeddedRequest & { session_id: string }): Promise<void> | void;
  /** Bring an agent tab to the front. Fire-and-forget: the daemon needs no ack. */
  onFocus?(req: EmbeddedRequest & { session_id: string }): Promise<void> | void;
  /** GET /requests failed after having worked (e.g. 404/409/410): re-register the agent env. */
  onLost?(): Promise<void> | void;
};

const HANDLED_CAP = 500;

export function startAgentPoller(
  handlers: AgentPollerHandlers,
  opts?: { intervalMs?: number },
): () => void {
  const intervalMs = opts?.intervalMs ?? 1000;
  const handled = new Set<string>();
  let stopped = false;
  let busy = false;

  const markHandled = (id: string) => {
    handled.add(id);
    while (handled.size > HANDLED_CAP) {
      const oldest = handled.values().next().value;
      if (oldest === undefined) break;
      handled.delete(oldest);
    }
  };

  // Handlers run detached: a slow ack (up to 10 s) must not stall GET /requests, or the
  // daemon's 30 s registration lapses. A rejection or throw is swallowed per handler.
  const dispatch = (run: () => Promise<void> | void) => {
    try {
      void Promise.resolve(run()).catch(() => {});
    } catch {
      // A throwing handler must not stop the rest of the batch.
    }
  };

  // True once a GET /requests succeeded; reset when it fails, so onLost fires once per outage.
  let worked = false;

  const tick = async () => {
    if (stopped || busy) return;
    busy = true;
    try {
      let reqs: EmbeddedRequest[];
      try {
        reqs = await embeddedRequests();
        worked = true;
      } catch {
        // Any failure (network, 404/409/410: registration gone) after a working poll:
        // ask the owner to prepare/ensure/register again.
        if (worked && !stopped) {
          worked = false;
          dispatch(() => handlers.onLost?.());
        }
        return;
      }
      for (const req of reqs ?? []) {
        if (stopped) return;
        if (handled.has(req.request_id)) continue;
        if (req.kind === "open" && req.url) {
          markHandled(req.request_id);
          const r = req as EmbeddedRequest & { url: string };
          dispatch(() => handlers.onOpen(r));
        } else if (req.kind === "close" && req.session_id) {
          markHandled(req.request_id);
          const r = req as EmbeddedRequest & { session_id: string };
          dispatch(() => handlers.onClose(r));
        } else if (req.kind === "focus" && req.session_id) {
          markHandled(req.request_id);
          const r = req as EmbeddedRequest & { session_id: string };
          dispatch(() => handlers.onFocus?.(r));
        }
      }
    } finally {
      busy = false;
    }
  };

  const timer = setInterval(() => {
    void tick();
  }, intervalMs);

  return () => {
    stopped = true;
    clearInterval(timer);
  };
}

/** Fragment-safe random token (letters and digits only). */
export function mintNonce(): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === "function") {
    return "n" + c.randomUUID().replace(/-/g, "");
  }
  const bytes = new Uint8Array(16);
  if (c && typeof c.getRandomValues === "function") {
    c.getRandomValues(bytes);
  } else {
    for (let i = 0; i < bytes.length; i++) {
      bytes[i] = Math.floor(Math.random() * 256);
    }
  }
  return (
    "n" + Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("")
  );
}

export function agentTabUrl(nonce: string): string {
  return `about:blank#${nonce}`;
}

const reportedClosed = new Set<string>();

/** Tell the daemon an agent tab closed. Swallows errors; a failure allows a retry. */
export async function reportAgentTabClosed(sessionId: string): Promise<void> {
  if (reportedClosed.has(sessionId)) return;
  reportedClosed.add(sessionId);
  try {
    await embeddedClosed(sessionId);
  } catch {
    reportedClosed.delete(sessionId);
  }
}
