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

export type EmbeddedRequest = {
  request_id: string;
  kind: "open" | "close" | string;
  project: string;
  url?: string;
  session_id?: string;
};

function post(body: unknown): RequestInit {
  return { method: "POST", body: JSON.stringify(body) };
}

export function embeddedPrepare(project: string): Promise<{ proxy_addr: string }> {
  return apiFetch<{ proxy_addr: string }>("/browser/embedded/prepare", post({ project }));
}

export function embeddedRegister(project: string, cdpHttp: string): Promise<void> {
  return apiFetch<void>("/browser/embedded/register", post({ project, cdp_http: cdpHttp }));
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

/** The browser pillar is switched off (409 `pillar_off`). */
export function isPillarOff(e: unknown): boolean {
  return isApiRefusal(e) && e.code === "pillar_off";
}

/** The request is gone: it expired or was already answered (410). */
export function isRequestGone(e: unknown): boolean {
  return isApiRefusal(e) && e.status === 410;
}
