/**
 * Client idempotency (#19).
 *
 * One key per *user intent*: the key is minted when the person asks for the action and reused by
 * every retry or double click of that same intent, so the daemon can collapse duplicates. A new
 * intent (the next click after the first settled) gets a new key.
 *
 *   const intent = newIntent();                // once per intent
 *   headers: idempotencyHeaders(intent.key)    // → { "Idempotency-Key": "…" }
 *
 * `data/client.ts` (branch `idem`) is what attaches the header; this module only mints keys.
 */
export const IDEMPOTENCY_HEADER = "Idempotency-Key";

export function newIdempotencyKey(): string {
  const c = globalThis.crypto;
  if (c !== undefined && typeof c.randomUUID === "function") return c.randomUUID();
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}-${Math.random().toString(36).slice(2)}`;
}

export interface Intent {
  readonly key: string;
}

export function newIntent(): Intent {
  return { key: newIdempotencyKey() };
}

export function idempotencyHeaders(key: string): Record<string, string> {
  return { [IDEMPOTENCY_HEADER]: key };
}
