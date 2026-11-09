import { useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  isLoginFinished,
  useAddTokenAccount,
  useCancelLogin,
  useLogin,
  useStartAccount,
  usePooledEngines,
  useSubmitLoginCode,
  type LoginPhase,
  type LoginView,
  type PoolAccount,
} from "../../data/accounts";
import { isApiRefusal } from "../../data/client";
import { Button, Field, Modal } from "../../ui";
import { engineName, suggestLabel } from "./engines";

/** Used only when the daemon does not name its engines (an older daemon, or not loaded yet). */
const FALLBACK_ENGINES: readonly string[] = ["claude", "codex"];

const PHASE_TEXT: Record<LoginPhase, string> = {
  starting: "Starting the login...",
  awaiting_browser: "Finish signing in in your browser.",
  awaiting_code: "Paste the code the browser showed.",
  verifying: "Verifying the account...",
  done: "Account added.",
  failed: "The login failed.",
  cancelled: "The login was cancelled.",
};

/**
 * The `+ Add account` dialog. Step 1 picks an engine and names the account; starting it hands the
 * login view up, and step 2 (the same dialog) is the login itself.
 */
export function AddAccountDialog({
  open,
  login,
  accounts,
  onStarted,
  onClose,
}: {
  open: boolean;
  login: LoginView | null;
  accounts: PoolAccount[];
  onStarted: (view: LoginView) => void;
  onClose: () => void;
}) {
  return (
    <Modal
      open={open}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
      title={login === null ? "Add account" : "Sign in"}
      size="md"
    >
      {login !== null ? (
        <LoginPanel initial={login} onClose={onClose} />
      ) : (
        <AddAccountStep accounts={accounts} onStarted={onStarted} onClose={onClose} />
      )}
    </Modal>
  );
}

function AddAccountStep({
  accounts,
  onStarted,
  onClose,
}: {
  accounts: PoolAccount[];
  onStarted: (view: LoginView) => void;
  onClose: () => void;
}) {
  const pooled = usePooledEngines().data;
  const engines = pooled && pooled.length > 0 ? pooled : FALLBACK_ENGINES;
  const [picked, setEngine] = useState<string>(engines[0]);
  // The list can change under the form (it loads after the first render); keep a valid choice.
  const engine = engines.includes(picked) ? picked : engines[0];
  const taken = accounts.map((a) => a.label);
  // Until the owner types, the name follows the engine: `claude`, or `claude-2` once that is taken.
  const [typed, setTyped] = useState<string | null>(null);
  const label = typed ?? suggestLabel(engine, taken);
  const trimmed = label.trim();
  const start = useStartAccount();
  const addToken = useAddTokenAccount();
  const [wanted, setMethod] = useState<"signin" | "token">("signin");
  // Only Claude takes a token; another engine falls back to the sign-in flow.
  const tokenOk = engine === "claude";
  const method = tokenOk ? wanted : "signin";
  const [token, setToken] = useState("");
  const pasting = method === "token";
  const failed = pasting ? addToken : start;

  const hint =
    trimmed === ""
      ? "Give the account a name to continue."
      : taken.some((t) => t.trim().toLowerCase() === trimmed.toLowerCase())
        ? `An account named "${trimmed}" already exists: pick another name.`
        : null;

  return (
    <form
      className="ac-add"
      onSubmit={(event) => {
        event.preventDefault();
        if (hint !== null) return;
        if (pasting) {
          const pasted = token.trim();
          if (pasted === "") return;
          // The token leaves state as soon as it is sent, whatever the answer.
          setToken("");
          addToken.mutate({ engine, label: trimmed, token: pasted }, { onSuccess: onClose });
          return;
        }
        start.mutate({ engine, label: trimmed }, { onSuccess: onStarted });
      }}
    >
      <div className="ac-engines" role="radiogroup" aria-label="Engine">
        {engines.map((id) => (
          <button
            key={id}
            type="button"
            role="radio"
            aria-checked={id === engine}
            className={`ac-engine${id === engine ? " ac-engine-on" : ""}`}
            onClick={() => setEngine(id)}
          >
            <strong>{engineName(id)}</strong>
            <span className="ac-engine-id">{id}</span>
          </button>
        ))}
      </div>
      <Field label="Account name" helper="Shown on the account's card and in the chat account chip.">
        <input value={label} onChange={(e) => setTyped(e.target.value)} autoComplete="off" />
      </Field>
      <div className="ac-methods" role="group" aria-label="Method">
        <button
          type="button"
          aria-pressed={!pasting}
          className={`ac-engine${!pasting ? " ac-engine-on" : ""}`}
          onClick={() => setMethod("signin")}
        >
          <strong>Sign in</strong>
        </button>
        <button
          type="button"
          aria-pressed={pasting}
          disabled={!tokenOk}
          className={`ac-engine${pasting ? " ac-engine-on" : ""}`}
          onClick={() => setMethod("token")}
        >
          <strong>Paste token</strong>
        </button>
      </div>
      {!tokenOk ? <p className="ac-hint">Only Claude accounts can use a token.</p> : null}
      {pasting ? (
        <Field label="Token" helper="Run `claude setup-token` in a terminal and paste the token it prints.">
          <input type="password" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" />
        </Field>
      ) : null}
      <div className="ac-add-actions">
        <Button onClick={onClose}>Cancel</Button>
        <Button
          type="submit"
          variant="primary"
          disabled={hint !== null || start.isPending || addToken.isPending || (pasting && token.trim() === "")}
          aria-describedby={hint === null ? undefined : "ac-add-hint"}
        >
          {pasting ? "Add account" : "Start login"}
        </Button>
      </div>
      {hint !== null ? <p id="ac-add-hint" className="ac-hint">{hint}</p> : null}
      {failed.isError ? (
        <div role="alert" className="ac-error">
          {isApiRefusal(failed.error) ? (failed.error.detail || failed.error.code) : "the background service is not responding"}
        </div>
      ) : null}
    </form>
  );
}

/** A login in progress: shows its phase, the auth URL (opened once), and the paste-code field. */
export function LoginPanel({ initial, onClose }: { initial: LoginView; onClose: () => void }) {
  const polled = useLogin(initial.login_id);
  const view = polled.data ?? initial;
  const submit = useSubmitLoginCode();
  const cancel = useCancelLogin();
  const [code, setCode] = useState("");
  const opened = useRef<string | null>(null);

  useEffect(() => {
    if (view.auth_url && opened.current !== view.auth_url) {
      opened.current = view.auth_url;
      void openUrl(view.auth_url).catch(() => {
        // The link stays visible below, so the owner can open it by hand.
      });
    }
  }, [view.auth_url]);

  const finished = isLoginFinished(view.phase);
  return (
    <section className="ac-login" aria-label="Account login">
      <p>
        <strong>{view.label}</strong> ({engineName(view.engine)}): {PHASE_TEXT[view.phase]}
      </p>
      {view.message ? <p>{view.message}</p> : null}
      {view.auth_url && !finished ? (
        <p>
          Sign-in link: <a href={view.auth_url} onClick={(e) => { e.preventDefault(); void openUrl(view.auth_url ?? "").catch(() => {}); }}>{view.auth_url}</a>
        </p>
      ) : null}
      {!finished ? (
        <form
          className="ac-code"
          onSubmit={(event) => {
            event.preventDefault();
            submit.mutate({ loginId: view.login_id, code: code.trim() }, { onSuccess: () => setCode("") });
          }}
        >
          <Field label="Paste code">
            <input value={code} onChange={(e) => setCode(e.target.value)} autoComplete="off" />
          </Field>
          <Button type="submit" variant="primary" disabled={code.trim() === "" || submit.isPending}>Submit code</Button>
        </form>
      ) : null}
      {submit.isError ? (
        <div role="alert" className="ac-error">
          {isApiRefusal(submit.error) ? (submit.error.detail || submit.error.code) : "the background service is not responding"}
        </div>
      ) : null}
      {finished ? (
        <Button onClick={onClose}>Close</Button>
      ) : (
        <Button onClick={() => cancel.mutate(view.login_id, { onSuccess: onClose })}>Cancel login</Button>
      )}
    </section>
  );
}
