import { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import type { StepUpOutcome } from "../data/transport";
import { Button } from "../ui/Button";
import { ErrorNote } from "../ui/ErrorNote";
import { Modal } from "../ui/Modal";

/**
 * Passkeys for the remote shell: the first one after an owner pairs, and the confirmation a grave
 * action asks for. The ceremonies run on the gateway; this file only carries the options to the
 * browser's authenticator and the answer back. Plain `fetch` with the CSRF header, like `session.ts`:
 * the data client would route a 403 back into the step-up it is in the middle of.
 *
 * A browser only lets `navigator.credentials` run from a user gesture, so every ceremony starts from a
 * click: the options are fetched BEFORE the click (`usePrepared`), and the click handler calls the
 * authenticator synchronously, with no await between the two.
 */

const MAIN_MACHINE =
  "This action needs a passkey confirmation and this browser has none. Do it on the main machine.";
const WINDOW_CLOSED =
  "A passkey has to be registered within 15 minutes of pairing, and that time has passed. Create a new owner pairing code on the main machine and pair this browser again.";
const UNSUPPORTED =
  "This browser cannot use passkeys here (it needs a secure connection and passkey support). Nothing was done; do it on the main machine.";
const CANCELLED = "The passkey was not confirmed (cancelled or timed out). Nothing was done.";

/** Options fetched this long ago are fetched again on the click (a challenge does not live forever). */
const OPTIONS_TTL_MS = 45_000;

type Kind = "register" | "verify";
type Failure = { code: string; detail: string };
type Refused = { ok: false } & Failure;
type Ceremony = { ok: true } | Refused;

/** Options already fetched and decoded, waiting for a click. */
type Prepared =
  | { kind: "register"; options: PublicKeyCredentialCreationOptions; at: number }
  | { kind: "verify"; options: PublicKeyCredentialRequestOptions; at: number };

const HEADERS = { "X-Nucleos-Csrf": "1", "Content-Type": "application/json" };

function fail(code: string, detail: string): Refused {
  return { ok: false, code, detail };
}

function b64urlToBuffer(value: string): ArrayBuffer {
  const padded = value.replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(padded + "=".repeat((4 - (padded.length % 4)) % 4));
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i += 1) bytes[i] = raw.charCodeAt(i);
  return bytes.buffer;
}

function bufferToB64url(buffer: ArrayBuffer): string {
  let raw = "";
  for (const byte of new Uint8Array(buffer)) raw += String.fromCharCode(byte);
  return btoa(raw).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

interface Descriptor {
  id: string;
  [key: string]: unknown;
}

function decodeDescriptors(list: unknown): PublicKeyCredentialDescriptor[] | undefined {
  if (!Array.isArray(list)) return undefined;
  return (list as Descriptor[]).map((entry) => ({ ...entry, id: b64urlToBuffer(entry.id) })) as PublicKeyCredentialDescriptor[];
}

function creationOptions(wire: Record<string, unknown>): PublicKeyCredentialCreationOptions {
  const user = wire.user as { id: string };
  return {
    ...wire,
    challenge: b64urlToBuffer(wire.challenge as string),
    user: { ...user, id: b64urlToBuffer(user.id) },
    excludeCredentials: decodeDescriptors(wire.excludeCredentials),
  } as unknown as PublicKeyCredentialCreationOptions;
}

function requestOptions(wire: Record<string, unknown>): PublicKeyCredentialRequestOptions {
  return {
    ...wire,
    challenge: b64urlToBuffer(wire.challenge as string),
    allowCredentials: decodeDescriptors(wire.allowCredentials),
  } as unknown as PublicKeyCredentialRequestOptions;
}

/** The binary members of an authenticator response. They are prototype getters, so `Object.entries` finds none. */
const RESPONSE_FIELDS = ["clientDataJSON", "attestationObject", "authenticatorData", "signature", "userHandle"] as const;

function asBuffer(value: unknown): ArrayBuffer | null {
  if (value instanceof ArrayBuffer) return value;
  if (ArrayBuffer.isView(value)) {
    return value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength) as ArrayBuffer;
  }
  return null;
}

/** `credential.toJSON()` where the browser has it, the same shape by hand where it does not. */
export function credentialJson(credential: PublicKeyCredential): unknown {
  if (typeof (credential as { toJSON?: unknown }).toJSON === "function") return credential.toJSON();
  const response = credential.response as unknown as Record<string, unknown>;
  const encoded: Record<string, string> = {};
  for (const name of RESPONSE_FIELDS) {
    const buffer = asBuffer(response[name]);
    if (buffer !== null) encoded[name] = bufferToB64url(buffer);
  }
  return { id: credential.id, rawId: bufferToB64url(credential.rawId), type: credential.type, response: encoded };
}

async function word(response: Response): Promise<string> {
  try {
    const body = (await response.json()) as { error?: unknown; refusal?: unknown };
    if (typeof body.error === "string") return body.error;
    if (typeof body.refusal === "string") return body.refusal;
  } catch {
    // An answer without a readable body still has a status.
  }
  return `http_${response.status}`;
}

async function gateway(method: "GET" | "POST", path: string, body?: unknown): Promise<Response> {
  return fetch(path, {
    method,
    credentials: "same-origin",
    headers: HEADERS,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
}

/** What a gateway refusal on a ceremony step means to the person. */
function refusal(status: number, name: string): Failure {
  if (name === "pairing_window_closed") return { code: name, detail: WINDOW_CLOSED };
  if (name === "no_passkey" || name === "not_an_owner") return { code: name, detail: MAIN_MACHINE };
  if (name === "session_invalid") return { code: name, detail: "This session has ended. Pair this browser again." };
  if (status === 429 || name === "slow_down") {
    return { code: "slow_down", detail: "Too many passkey attempts. Wait a moment and try again. Nothing was done." };
  }
  if (name === "step_up_required") {
    return { code: name, detail: "Confirm with an existing passkey first. Nothing was done." };
  }
  return { code: "passkey_refused", detail: "The passkey was refused. Nothing was done." };
}

/** What the browser's authenticator threw. A cancel is a person's answer, not an error to report loudly. */
function authenticatorFailure(error: unknown): Failure {
  const name = error instanceof Error || error instanceof DOMException ? error.name : "";
  if (name === "NotAllowedError" || name === "AbortError") return { code: "passkey_cancelled", detail: CANCELLED };
  return { code: "passkey_failed", detail: "The passkey could not be used. Nothing was done." };
}

function credentials(): CredentialsContainer | null {
  const container = typeof navigator === "undefined" ? undefined : navigator.credentials;
  return container === undefined || container === null ? null : container;
}

/** Step one, before any click: ask the gateway for the options and decode them. Never throws. */
async function begin(kind: Kind): Promise<{ ok: true; prepared: Prepared } | Refused> {
  try {
    const response = await gateway("POST", `/auth/passkey/${kind}/begin`);
    if (!response.ok) {
      const failure = refusal(response.status, await word(response));
      return fail(failure.code, failure.detail);
    }
    const wire = ((await response.json()) as { publicKey: Record<string, unknown> }).publicKey;
    const at = Date.now();
    if (kind === "register") return { ok: true, prepared: { kind, options: creationOptions(wire), at } };
    return { ok: true, prepared: { kind, options: requestOptions(wire), at } };
  } catch {
    return fail("passkey_unreachable", "The gateway could not be reached. Nothing was done.");
  }
}

/**
 * Step two, called synchronously from a click: hand the options to the authenticator. Deliberately not
 * `async`, so `navigator.credentials` is reached inside the user gesture.
 */
function start(container: CredentialsContainer, prepared: Prepared): Promise<Credential | null> {
  try {
    return prepared.kind === "register"
      ? container.create({ publicKey: prepared.options })
      : container.get({ publicKey: prepared.options });
  } catch (error) {
    return Promise.reject(error);
  }
}

/** Step three: wait for the authenticator, then give its answer to the gateway. Never throws. */
async function complete(prepared: Prepared, pending: Promise<Credential | null>): Promise<Ceremony> {
  let credential: Credential | null;
  try {
    credential = await pending;
  } catch (error) {
    const failure = authenticatorFailure(error);
    return fail(failure.code, failure.detail);
  }
  if (credential === null) return fail("passkey_cancelled", CANCELLED);
  try {
    const finish = await gateway("POST", `/auth/passkey/${prepared.kind}/finish`, credentialJson(credential as PublicKeyCredential));
    if (!finish.ok) {
      const failure = refusal(finish.status, await word(finish));
      return fail(failure.code, failure.detail);
    }
    return { ok: true };
  } catch {
    return fail("passkey_unreachable", "The gateway could not be reached. Nothing was done.");
  }
}

type PreparedState =
  | { status: "loading" }
  | { status: "ready"; prepared: Prepared }
  | { status: "failed"; failure: Failure };

/**
 * Fetches the options for `kind` as soon as it is set (and again on `reload`), so a click finds them
 * waiting. `kind === null` fetches nothing. A stale answer never overwrites a newer one.
 */
function usePrepared(kind: Kind | null): { state: PreparedState; reload: (kind: Kind) => Promise<void> } {
  const [state, setState] = useState<PreparedState>({ status: "loading" });
  const generation = useRef(0);

  const reload = useCallback(async (next: Kind) => {
    generation.current += 1;
    const mine = generation.current;
    setState({ status: "loading" });
    const result = await begin(next);
    if (mine !== generation.current) return;
    setState(result.ok ? { status: "ready", prepared: result.prepared } : { status: "failed", failure: result });
  }, []);

  useEffect(() => {
    if (kind === null) return undefined;
    void reload(kind);
    return () => {
      generation.current += 1;
    };
  }, [kind, reload]);

  return { state, reload };
}

function expired(prepared: Prepared): boolean {
  return Date.now() - prepared.at > OPTIONS_TTL_MS;
}

interface PasskeyStatus {
  registered?: boolean;
  can_register?: boolean;
}

/**
 * The dialog a grave action's refusal opens. A device with a passkey sees one button; one without,
 * and allowed to enrol, registers first and then confirms with a second click of its own.
 */
function StepUpDialog({ registered, onDone }: { registered: boolean; onDone: (outcome: StepUpOutcome) => void }) {
  const [phase, setPhase] = useState<Kind>(registered ? "verify" : "register");
  const [busy, setBusy] = useState(false);
  const { state, reload } = usePrepared(phase);

  useEffect(() => {
    if (state.status === "failed") onDone({ kind: "failed", ...state.failure });
  }, [state, onDone]);

  const ready = state.status === "ready" && state.prepared.kind === phase;

  function cancel() {
    onDone({ kind: "failed", code: "passkey_cancelled", detail: CANCELLED });
  }

  function confirm() {
    if (busy || state.status !== "ready" || state.prepared.kind !== phase) return;
    const prepared = state.prepared;
    if (expired(prepared)) {
      void reload(phase);
      return;
    }
    const container = credentials();
    if (container === null) {
      onDone({ kind: "failed", code: "passkey_unsupported", detail: UNSUPPORTED });
      return;
    }
    const pending = start(container, prepared);
    setBusy(true);
    void complete(prepared, pending).then((result) => {
      if (!result.ok) {
        onDone({ kind: "failed", code: result.code, detail: result.detail });
      } else if (prepared.kind === "register") {
        // Registered. Confirming is a second click of the person's own, never chained from this one.
        setBusy(false);
        setPhase("verify");
      } else {
        onDone({ kind: "verified" });
      }
    });
  }

  const registering = phase === "register";
  let label = registering ? "Register a passkey" : "Confirm with passkey";
  if (busy) label = "Waiting for the passkey…";
  else if (!ready) label = "Preparing…";

  return (
    <Modal
      open
      onOpenChange={(open) => {
        if (!open && !busy) cancel();
      }}
      title={registering ? "Register a passkey first" : "Confirm this action"}
      description={
        registering
          ? "This action needs a passkey confirmation and this browser has no passkey yet. Register one, then confirm."
          : "This action needs your confirmation with a passkey. Nothing happens until you confirm."
      }
      footer={
        <>
          <Button type="button" disabled={busy} onClick={cancel}>
            Cancel
          </Button>
          <Button type="button" variant="primary" disabled={busy || !ready} onClick={confirm}>
            {label}
          </Button>
        </>
      }
    />
  );
}

/** Mounts the dialog on its own root, so the non-React transport can ask a person; unmounts it when answered. */
function present(registered: boolean): Promise<StepUpOutcome> {
  return new Promise((resolve) => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    let answered = false;
    const onDone = (outcome: StepUpOutcome) => {
      if (answered) return;
      answered = true;
      resolve(outcome);
      queueMicrotask(() => {
        root.unmount();
        host.remove();
      });
    };
    root.render(<StepUpDialog registered={registered} onDone={onDone} />);
  });
}

async function runStepUp(): Promise<StepUpOutcome> {
  let status: PasskeyStatus;
  try {
    const response = await gateway("GET", "/auth/passkey/status");
    if (!response.ok) {
      const failure = refusal(response.status, await word(response));
      return { kind: "failed", ...failure };
    }
    status = (await response.json()) as PasskeyStatus;
  } catch {
    return { kind: "failed", code: "passkey_unreachable", detail: "The gateway could not be reached. Nothing was done." };
  }
  const registered = status.registered === true;
  // No passkey and no way to enrol one from here: do not prompt, point at the main machine.
  if (!registered && status.can_register !== true) return { kind: "failed", code: "no_passkey", detail: MAIN_MACHINE };
  if (credentials() === null) return { kind: "failed", code: "passkey_unsupported", detail: UNSUPPORTED };
  return present(registered);
}

let inFlight: Promise<StepUpOutcome> | null = null;

/** The remote transport's answer to `step_up_required`. Concurrent refusals share one dialog. */
export function stepUp(): Promise<StepUpOutcome> {
  if (inFlight === null) {
    const attempt = runStepUp().finally(() => {
      if (inFlight === attempt) inFlight = null;
    });
    inFlight = attempt;
  }
  return inFlight;
}

/**
 * The step after an owner pairs. Skipping is allowed: the session works, grave actions stay refused
 * until a passkey exists. Any authenticator, the phone's included (no attachment is forced).
 */
export function PasskeyEnrol({ onDone }: { onDone: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { state, reload } = usePrepared("register");

  function register() {
    if (busy) return;
    if (state.status === "failed") {
      setError(null);
      void reload("register");
      return;
    }
    if (state.status !== "ready") return;
    const prepared = state.prepared;
    if (expired(prepared)) {
      void reload("register");
      return;
    }
    const container = credentials();
    if (container === null) {
      setError(UNSUPPORTED);
      return;
    }
    setError(null);
    setBusy(true);
    // The authenticator is reached here, inside the click; everything after it may wait.
    const pending = start(container, prepared);
    void complete(prepared, pending).then((result) => {
      setBusy(false);
      if (result.ok) {
        onDone();
      } else {
        setError(result.detail);
        void reload("register");
      }
    });
  }

  const shown = error ?? (state.status === "failed" ? state.failure.detail : null);
  let label = "Register a passkey";
  if (busy) label = "Waiting for the passkey…";
  else if (state.status === "loading") label = "Preparing…";

  return (
    <main className="remote-pairing">
      <section className="remote-pairing-form" aria-label="Register a passkey">
        <h1>Register a passkey</h1>
        <p>
          Grave actions from this browser ask for a passkey. Use this device, or choose your phone if this one has
          no authenticator.
        </p>
        <p>
          If you skip, this browser works but those actions stay refused; do them on the main machine. A passkey can
          only be registered within 15 minutes of pairing.
        </p>
        {shown === null ? null : <ErrorNote>{shown}</ErrorNote>}
        <Button type="button" variant="primary" disabled={busy || state.status === "loading"} onClick={register}>
          {label}
        </Button>
        <Button type="button" disabled={busy} onClick={onDone}>
          Skip for now
        </Button>
      </section>
    </main>
  );
}
