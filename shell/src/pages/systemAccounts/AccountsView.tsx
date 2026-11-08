import { useState } from "react";
import { MoreHorizontal } from "lucide-react";
import {
  useAccountActions,
  useAccountDefault,
  useAccounts,
  useReloginAccount,
  useSetAccountDefault,
  windowOf,
  type LoginView,
  type PoolAccount,
} from "../../data/accounts";
import { isApiRefusal } from "../../data/client";
import { resetsIn } from "../../chats/AccountChip";
import { percentText } from "../../lib/format";
import { Badge, Button, ErrorNote, Field, Quiet, RefusalNote, Select, Skeleton } from "../../ui";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "../../ui/vendor/dropdown-menu";
import { AddAccountDialog } from "./AddAccount";
import { engineName } from "./engines";
import "./accounts.css";

/** The default selector's options: Auto, then each engine, then each concrete account. */
export function defaultOptions(accounts: PoolAccount[]): { value: string; label: string }[] {
  const providers = [...new Set(accounts.map((a) => a.provider))];
  return [
    { value: "auto", label: "Auto" },
    ...providers.map((p) => ({ value: `provider:${p}`, label: `Any ${engineName(p)} account` })),
    ...accounts.map((a) => ({ value: `account:${a.id}`, label: `${a.label} (${engineName(a.engine)})` })),
  ];
}

/** One usage window as a meter: its name, the percent, and when it resets if the provider said. */
function Meter({ account, name }: { account: PoolAccount; name: string }) {
  const window = windowOf(account, name);
  const fraction = window?.used_fraction ?? null;
  const reset = resetsIn(window?.resets_at);
  return (
    <div className="ac-meter" title={window?.resets_at ? `Resets ${window.resets_at}` : undefined}>
      <div className="ac-meter-head">
        <span className="ac-meter-name">{name}</span>
        <span>{percentText(fraction)}</span>
      </div>
      <div
        className={`ac-bar-track${window?.stale ? " ac-stale" : ""}`}
        role="progressbar"
        aria-label={`${account.label} ${name} usage`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={fraction === null ? undefined : Math.round(fraction * 100)}
      >
        <div className="ac-bar-fill" style={{ width: `${Math.min(100, Math.round((fraction ?? 0) * 100))}%` }} />
      </div>
      {reset !== null ? <span className="ac-meter-reset">resets in {reset}</span> : null}
    </div>
  );
}

/** "official, 3 min ago", or "not measured". */
export function readingText(account: PoolAccount, now: number = Date.now()): string {
  const usage = account.usage;
  if (!usage || usage.source === "unmeasured") return "not measured";
  if (!usage.read_at) return usage.source;
  const minutes = Math.max(0, Math.round((now - Date.parse(usage.read_at)) / 60_000));
  return `${usage.source}, ${minutes < 1 ? "just now" : minutes < 60 ? `${minutes} min ago` : `${Math.round(minutes / 60)} h ago`}`;
}

const STATE_TONE = { active: "active", paused: "pending", needs_login: "danger" } as const;
const STATE_TEXT = { active: "Active", paused: "Paused", needs_login: "Needs login" } as const;

function AccountCard({ account, onLogin }: { account: PoolAccount; onLogin: (view: LoginView) => void }) {
  const { rename, pause, resume, remove, logout, setToken } = useAccountActions();
  const relogin = useReloginAccount();
  const [confirmLogout, setConfirmLogout] = useState(false);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [replacing, setReplacing] = useState<string | null>(null);
  const isToken = account.kind === "token";

  const fail = (e: unknown) => setError(isApiRefusal(e) ? e.code : "the daemon did not answer");
  const signIn = () =>
    isToken ? setReplacing("") : relogin.mutate(account.id, { onSuccess: onLogin, onError: fail });

  const tryRemove = (force: boolean) => {
    remove.mutate(
      { id: account.id, force },
      {
        onError: (e) => {
          const body = isApiRefusal(e) ? (e.body as { live_sessions?: number } | undefined) : undefined;
          if (isApiRefusal(e) && e.code === "live_sessions" && body?.live_sessions !== undefined) {
            setConfirm(body.live_sessions);
          } else fail(e);
        },
        onSuccess: () => setConfirm(null),
      },
    );
  };

  const five = windowOf(account, "5h")?.used_fraction ?? null;
  const seven = windowOf(account, "7d")?.used_fraction ?? null;
  const measured = five !== null || seven !== null;

  return (
    <article className="ac-card" aria-label={`${account.label} account`}>
      <header className="ac-card-head">
        <div className="ac-card-title">
          <span className="ac-engine-tag">{engineName(account.engine)}</span>
          {renaming === null ? <strong className="ac-card-label">{account.label}</strong> : null}
        </div>
        <Badge tone={STATE_TONE[account.state]}>{STATE_TEXT[account.state]}</Badge>
        <DropdownMenu>
          <DropdownMenuTrigger
            className="ui-icon-button"
            aria-label={`Actions for ${account.label}`}
            title={`Actions for ${account.label}`}
          >
            <MoreHorizontal className="ui-icon-button-glyph" strokeWidth={1.5} aria-hidden="true" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="ac-menu">
            <DropdownMenuItem onSelect={() => setRenaming(account.label)}>Rename</DropdownMenuItem>
            <DropdownMenuItem onSelect={signIn}>{isToken ? "Replace token" : "Redo login"}</DropdownMenuItem>
            {account.state === "needs_login" ? null : (
              <DropdownMenuItem onSelect={() => setConfirmLogout(true)}>Log out</DropdownMenuItem>
            )}
            {account.state === "paused" ? (
              <DropdownMenuItem onSelect={() => resume.mutate(account.id, { onError: fail })}>Resume</DropdownMenuItem>
            ) : (
              <DropdownMenuItem onSelect={() => pause.mutate(account.id, { onError: fail })}>Pause</DropdownMenuItem>
            )}
            {account.is_default ? null : (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuItem variant="destructive" onSelect={() => tryRemove(false)}>Remove</DropdownMenuItem>
              </>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      </header>

      {renaming !== null ? (
        <form
          className="ac-rename"
          onSubmit={(event) => {
            event.preventDefault();
            rename.mutate({ id: account.id, label: renaming }, { onSuccess: () => setRenaming(null), onError: fail });
          }}
        >
          <input aria-label="New label" value={renaming} onChange={(e) => setRenaming(e.target.value)} />
          <Button type="submit" variant="primary" disabled={renaming.trim() === ""}>Save</Button>
          <Button onClick={() => setRenaming(null)}>Cancel</Button>
        </form>
      ) : null}

      {replacing !== null ? (
        <form
          className="ac-token"
          onSubmit={(event) => {
            event.preventDefault();
            const token = replacing.trim();
            if (token === "") return;
            // The token leaves state as soon as it is sent, whatever the answer.
            setReplacing(null);
            setToken.mutate({ id: account.id, token }, { onError: fail });
          }}
        >
          <input
            type="password"
            aria-label="New token"
            autoComplete="off"
            value={replacing}
            onChange={(e) => setReplacing(e.target.value)}
          />
          <Button type="submit" variant="primary" disabled={replacing.trim() === "" || setToken.isPending}>Save</Button>
          <Button onClick={() => setReplacing(null)}>Cancel</Button>
        </form>
      ) : null}

      {measured ? (
        <div className="ac-meters">
          <Meter account={account} name="5h" />
          <Meter account={account} name="7d" />
        </div>
      ) : (
        <p className="ac-muted">No usage reading yet</p>
      )}

      {account.live_sessions > 0 ? (
        <p className="ac-muted">
          {account.live_sessions} live session{account.live_sessions === 1 ? "" : "s"}
        </p>
      ) : null}

      {error !== null ? <div role="alert" className="ac-error">{error}</div> : null}
      {confirm !== null ? (
        <div role="alert" className="ac-confirm">
          {confirm} live session{confirm === 1 ? "" : "s"} use this account.{" "}
          <Button intent="stop" onClick={() => tryRemove(true)}>Remove anyway</Button>
          <Button onClick={() => setConfirm(null)}>Keep</Button>
        </div>
      ) : null}
      {confirmLogout ? (
        <div role="alert" className="ac-confirm">
          Sign this account out? Its history stays; use Redo login to sign back in.{" "}
          <Button
            intent="stop"
            onClick={() => logout.mutate(account.id, { onSuccess: () => setConfirmLogout(false), onError: (e) => { setConfirmLogout(false); fail(e); } })}
          >
            Log out
          </Button>
          <Button onClick={() => setConfirmLogout(false)}>Keep</Button>
        </div>
      ) : null}

      {account.state === "needs_login" ? (
        <div className="ac-card-primary">
          <Button variant="primary" onClick={signIn} disabled={relogin.isPending}>
            {isToken ? "Replace token" : "Sign in"}
          </Button>
        </div>
      ) : account.state === "paused" ? (
        <div className="ac-card-primary">
          <Button variant="primary" onClick={() => resume.mutate(account.id, { onError: fail })}>Resume</Button>
        </div>
      ) : null}
    </article>
  );
}

/** System > Accounts: the pool's default, its accounts as cards, and the way to add one. */
export function AccountsView() {
  const accounts = useAccounts();
  const chosen = useAccountDefault();
  const setDefault = useSetAccountDefault();
  const [login, setLogin] = useState<LoginView | null>(null);
  const [adding, setAdding] = useState(false);

  if (accounts.isError) {
    if (isApiRefusal(accounts.error)) return <RefusalNote refusal={accounts.error} />;
    return (
      <ErrorNote onRetry={() => void accounts.refetch()}>
        the daemon is not responding: nothing is known about the accounts
      </ErrorNote>
    );
  }
  if (!accounts.data) return <Skeleton label="Reading the accounts" variant="cards" count={2} />;

  const list = accounts.data;
  const options = defaultOptions(list);
  const current = chosen.data ?? "auto";

  return (
    <div className="ac-page">
      <div className="ac-toolbar">
        <Field label="Default account" helper="Used when a chat's account chip is on Auto.">
          <Select
            value={options.some((o) => o.value === current) ? current : "auto"}
            onChange={(event) => setDefault.mutate(event.target.value)}
          >
            {options.map((o) => (
              <option key={o.value} value={o.value}>{o.label}</option>
            ))}
          </Select>
        </Field>
        <Button variant="primary" onClick={() => setAdding(true)}>+ Add account</Button>
      </div>
      {setDefault.isError ? <div role="alert" className="ac-error">The default could not be saved.</div> : null}

      {list.length === 0 ? (
        <Quiet says="no accounts yet: add one to start the pool" />
      ) : (
        <div className="ac-grid">
          {list.map((account) => (
            <AccountCard key={account.id} account={account} onLogin={setLogin} />
          ))}
        </div>
      )}

      <AddAccountDialog
        open={adding || login !== null}
        login={login}
        accounts={list}
        onStarted={setLogin}
        onClose={() => {
          setAdding(false);
          setLogin(null);
        }}
      />
    </div>
  );
}
