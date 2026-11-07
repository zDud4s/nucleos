import { useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  isLoginFinished,
  useCancelLogin,
  useLogin,
  useStartAccount,
  useSubmitLoginCode,
  type LoginPhase,
  type LoginView,
} from "../../data/accounts";
import { isApiRefusal } from "../../data/client";
import { Button } from "../../ui";

const ENGINES = ["claude", "codex"] as const;

const PHASE_TEXT: Record<LoginPhase, string> = {
  starting: "Starting the login...",
  awaiting_browser: "Finish signing in in your browser.",
  awaiting_code: "Paste the code the browser showed.",
  verifying: "Verifying the account...",
  done: "Account added.",
  failed: "The login failed.",
  cancelled: "The login was cancelled.",
};

/** The `+ Add account` form: an engine and a label; starting it hands the login view up. */
export function AddAccount({ onStarted }: { onStarted: (view: LoginView) => void }) {
  const [open, setOpen] = useState(false);
  const [engine, setEngine] = useState<string>(ENGINES[0]);
  const [label, setLabel] = useState("");
  const start = useStartAccount();

  if (!open) return <Button onClick={() => setOpen(true)}>+ Add account</Button>;

  return (
    <form
      className="ac-add"
      onSubmit={(event) => {
        event.preventDefault();
        start.mutate({ engine, label: label.trim() }, { onSuccess: onStarted });
      }}
    >
      <label>
        Engine
        <select value={engine} onChange={(e) => setEngine(e.target.value)}>
          {ENGINES.map((e) => (
            <option key={e} value={e}>{e}</option>
          ))}
        </select>
      </label>
      <label>
        Label
        <input value={label} onChange={(e) => setLabel(e.target.value)} />
      </label>
      <Button type="submit" variant="primary" disabled={label.trim() === "" || start.isPending}>Start login</Button>
      <Button onClick={() => setOpen(false)}>Cancel</Button>
      {start.isError ? (
        <div role="alert" className="ac-error">
          {isApiRefusal(start.error) ? (start.error.detail || start.error.code) : "the daemon did not answer"}
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
        <strong>{view.label}</strong> ({view.engine}): {PHASE_TEXT[view.phase]}
      </p>
      {view.message ? <p>{view.message}</p> : null}
      {view.auth_url && !finished ? (
        <p>
          Sign-in link: <a href={view.auth_url} onClick={(e) => { e.preventDefault(); void openUrl(view.auth_url ?? "").catch(() => {}); }}>{view.auth_url}</a>
        </p>
      ) : null}
      {!finished ? (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            submit.mutate({ loginId: view.login_id, code: code.trim() }, { onSuccess: () => setCode("") });
          }}
        >
          <label>
            Paste code
            <input value={code} onChange={(e) => setCode(e.target.value)} autoComplete="off" />
          </label>
          <Button type="submit" variant="primary" disabled={code.trim() === "" || submit.isPending}>Submit code</Button>
        </form>
      ) : null}
      {submit.isError ? (
        <div role="alert" className="ac-error">
          {isApiRefusal(submit.error) ? (submit.error.detail || submit.error.code) : "the daemon did not answer"}
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
