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

export type AgentEnvResult =
  | { ok: true; cdpHttp: string }
  | { ok: false; reason: string };

const inFlightEnv = new Map<string, Promise<AgentEnvResult>>();

function messageOf(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e) ?? String(e);
  } catch {
    return String(e);
  }
}

async function runEnsure(project: string): Promise<AgentEnvResult> {
  try {
    const { proxy_addr } = await embeddedPrepare(project);
    const env = await invoke<{ cdp_http: string }>("browser_env_ensure", {
      project,
      role: "agent",
      proxyAddr: proxy_addr,
    });
    await embeddedRegister(project, env.cdp_http);
    return { ok: true, cdpHttp: env.cdp_http };
  } catch (e) {
    return { ok: false, reason: messageOf(e) };
  }
}

/** Prepare the agent browser env for a project. Never throws; concurrent calls share one run. */
export function ensureAgentEnv(project: string): Promise<AgentEnvResult> {
  const existing = inFlightEnv.get(project);
  if (existing) return existing;
  const p = runEnsure(project).finally(() => {
    if (inFlightEnv.get(project) === p) inFlightEnv.delete(project);
  });
  inFlightEnv.set(project, p);
  return p;
}

export type AgentPollerHandlers = {
  onOpen(req: EmbeddedRequest & { url: string }): Promise<void> | void;
  onClose(req: EmbeddedRequest & { session_id: string }): Promise<void> | void;
  /** Bring an agent tab to the front. Fire-and-forget: the daemon needs no ack. */
  onFocus?(req: EmbeddedRequest & { session_id: string }): Promise<void> | void;
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

  const tick = async () => {
    if (stopped || busy) return;
    busy = true;
    try {
      let reqs: EmbeddedRequest[];
      try {
        reqs = await embeddedRequests();
      } catch {
        return;
      }
      for (const req of reqs ?? []) {
        if (stopped) return;
        if (handled.has(req.request_id)) continue;
        try {
          if (req.kind === "open" && req.url) {
            markHandled(req.request_id);
            await handlers.onOpen(req as EmbeddedRequest & { url: string });
          } else if (req.kind === "close" && req.session_id) {
            markHandled(req.request_id);
            await handlers.onClose(
              req as EmbeddedRequest & { session_id: string },
            );
          } else if (req.kind === "focus" && req.session_id) {
            markHandled(req.request_id);
            await handlers.onFocus?.(
              req as EmbeddedRequest & { session_id: string },
            );
          }
        } catch {
          // A failing handler must not stop the loop or the rest of the batch.
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
