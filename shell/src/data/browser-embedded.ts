/**
 * Client for the daemon's embedded-browser handshake routes.
 *
 * Error policy: nothing in here swallows an error. Every function propagates
 * whatever {@link apiFetch} throws, so a caller sees an `ApiRefusal` for a
 * refusal and an `ApiUnavailable` for an absence. Two refusals matter here:
 *  - 409 `{refusal:"pillar_off"}` surfaces as an `ApiRefusal` with code
 *    "pillar_off" (see {@link isPillarOff});
 *  - 410 (the request expired) surfaces as an `ApiRefusal` with the derived
 *    code "http_410" (see {@link isRequestGone}).
 */
import { apiFetch, isApiRefusal } from "./client";
import { scopeProject } from "./browser-tabs";

export type EmbeddedRequest = {
  request_id: string;
  /** "open" (url), "close" (session_id) or "focus" (session_id: bring that tab to the front). */
  kind: "open" | "close" | "focus" | string;
  /** The project id, or `null` for the no-project scope (map it with `scopeKey`). */
  project: string | null;
  url?: string;
  session_id?: string;
};

function post(body: unknown): RequestInit {
  return { method: "POST", body: JSON.stringify(body) };
}

/** `key` is a scope key (`scopeKey`); the body carries `project: null` for no project. */
export function embeddedPrepare(key: string): Promise<{ proxy_addr: string }> {
  return apiFetch<{ proxy_addr: string }>(
    "/browser/embedded/prepare",
    post({ project: scopeProject(key) }),
  );
}

/** `key` is a scope key (`scopeKey`); the body carries `project: null` for no project. */
export function embeddedRegister(key: string, cdpHttp: string): Promise<void> {
  return apiFetch<void>(
    "/browser/embedded/register",
    post({ project: scopeProject(key), cdp_http: cdpHttp }),
  );
}

export function embeddedRequests(): Promise<EmbeddedRequest[]> {
  return apiFetch<EmbeddedRequest[]>("/browser/embedded/requests");
}

export function embeddedAck(requestId: string, nonce: string): Promise<{ session_id: string }> {
  return apiFetch<{ session_id: string }>(
    `/browser/embedded/requests/${encodeURIComponent(requestId)}/ack`,
    post({ nonce }),
  );
}

export function embeddedClosed(sessionId: string): Promise<void> {
  return apiFetch<void>(
    `/browser/embedded/sessions/${encodeURIComponent(sessionId)}/closed`,
    post({}),
  );
}

/** Reply to an owner switch: nothing for "human" (204), the grant chain for "agent". */
export type OwnerReply = { chain: string[]; row_id: number } | undefined;

/** Hand a session to the person ("human") or back to the agent ("agent"). */
export function embeddedOwner(
  sessionId: number | string,
  owner: "human" | "agent",
): Promise<OwnerReply> {
  return apiFetch<OwnerReply>(
    `/browser/embedded/sessions/${encodeURIComponent(String(sessionId))}/owner`,
    post({ owner }),
  );
}

/** Keep (or release) a session's grant chain, optionally writable. */
export function embeddedKeep(
  sessionId: number | string,
  keep: boolean,
  writable: boolean,
): Promise<{ granted: string[] }> {
  return apiFetch<{ granted: string[] }>(
    "/browser/keep",
    post({ session_id: sessionId, keep, writable }),
  );
}

/** The browser pillar is switched off (409 `pillar_off`). */
export function isPillarOff(e: unknown): boolean {
  return isApiRefusal(e) && e.code === "pillar_off";
}

/** The request is gone: it expired or was already answered (410). */
export function isRequestGone(e: unknown): boolean {
  return isApiRefusal(e) && e.status === 410;
}
