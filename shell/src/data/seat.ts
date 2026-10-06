import { useSyncExternalStore } from "react";
import { apiFetch } from "./client";
import type { InputEvent, PromptAnswer } from "./liveRecords";

/**
 * The seat nonces of the browser sessions this shell is driving.
 *
 * Memory only, on purpose: the nonce is a credential (spec §4.3), so it is
 * never written to storage. The cost is that a reload loses the ability to
 * drive a session — it can still be viewed, given back or closed.
 */
const nonces = new Map<number, string>();
const listeners = new Set<() => void>();

function notify() {
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function rememberSeat(sessionId: number, nonce: string) {
  nonces.set(sessionId, nonce);
  notify();
}

export function forgetSeat(sessionId: number) {
  if (nonces.delete(sessionId)) notify();
}

export function seatNonce(sessionId: number): string | undefined {
  return nonces.get(sessionId);
}

export function useSeatNonce(sessionId: number): string | undefined {
  return useSyncExternalStore(
    subscribe,
    () => nonces.get(sessionId),
    () => undefined,
  );
}

/** Send input events to the page the person is driving. */
export function postInput(sessionId: number, nonce: string, events: InputEvent[]) {
  return apiFetch<unknown>(`/browser/sessions/${sessionId}/input`, {
    method: "POST",
    body: JSON.stringify({ seat_nonce: nonce, events }),
  });
}

/** Answer a prompt (dialog, select, file chooser, auth) the page opened. */
export function postAnswer(sessionId: number, nonce: string, prompt: string, answer: PromptAnswer) {
  return apiFetch<unknown>(`/browser/sessions/${sessionId}/answer`, {
    method: "POST",
    body: JSON.stringify({ seat_nonce: nonce, prompt, answer }),
  });
}
