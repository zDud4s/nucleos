import { useState } from "react";
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
import { percentText } from "../../lib/format";
import { Badge, Button, ErrorNote, Quiet, RefusalNote, Skeleton } from "../../ui";
import { AddAccount, LoginPanel } from "./AddAccount";
import "./accounts.css";

/** The default selector's options: Auto, then each provider, then each concrete account. */
export function defaultOptions(accounts: PoolAccount[]): { value: string; label: string }[] {
  const providers = [...new Set(accounts.map((a) => a.provider))];
  return [
    { value: "auto", label: "Auto" },
    ...providers.map((p) => ({ value: `provider:${p}`, label: `Any ${p} account` })),
    ...accounts.map((a) => ({ value: `account:${a.id}`, label: `${a.label} (${a.provider})` })),
  ];
}

/** "42%" or an em dash for a window the account has no reading for. */
function BarCell({ account, name }: { account: PoolAccount; name: string }) {
  const window = windowOf(account, name);
  const fraction = window?.used_fraction ?? null;
  const text = percentText(fraction);
  return (
    <div className="ac-bar" title={window?.resets_at ? `Resets ${window.resets_at}` : undefined}>
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
      <span>{text}</span>
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

function AccountRow({ account, onLogin }: { account: PoolAccount; onLogin: (view: LoginView) => void }) {
  const { rename, pause, resume, remove } = useAccountActions();
  const relogin = useReloginAccount();
  const [open, setOpen] = useState(false);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const fail = (e: unknown) => setError(isApiRefusal(e) ? e.code : "the daemon did not answer");
  const close = () => setOpen(false);

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

  return (
    <tr>
      <td>
        {renaming !== null ? (
          <form
            onSubmit={(event) => {
              event.preventDefault();
              rename.mutate(
                { id: account.id, label: renaming },
                { onSuccess: () => setRenaming(null), onError: fail },
              );
            }}
          >
            <input aria-label="New label" value={renaming} onChange={(e) => setRenaming(e.target.value)} />
            <Button type="submit" variant="primary" disabled={renaming.trim() === ""}>Save</Button>
            <Button onClick={() => setRenaming(null)}>Cancel</Button>
          </form>
        ) : (
          <>
            <strong>{account.label}</strong> <Badge tone={STATE_TONE[account.state]}>{account.state.replace("_", " ")}</Badge>
          </>
        )}
        {error !== null ? <div role="alert" className="ac-error">{error}</div> : null}
        {confirm !== null ? (
          <div role="alert" className="ac-confirm">
            {confirm} live session{confirm === 1 ? "" : "s"} use this account.{" "}
            <Button intent="stop" onClick={() => tryRemove(true)}>Remove anyway</Button>
            <Button onClick={() => setConfirm(null)}>Keep</Button>
          </div>
        ) : null}
      </td>
      <td>{account.provider}</td>
      <td>{account.kind === "api_key" ? "API key" : "Subscription"}</td>
      <td><BarCell account={account} name="5h" /></td>
      <td><BarCell account={account} name="7d" /></td>
      <td>{account.live_sessions}</td>
      <td>{readingText(account)}</td>
      <td className="ac-menu">
        <Button aria-label={`Actions for ${account.label}`} aria-expanded={open} onClick={() => setOpen(!open)}>
          ...
        </Button>
        {open ? (
          <div role="menu" className="ac-menu-list">
            <Button role="menuitem" onClick={() => { setRenaming(account.label); close(); }}>Rename</Button>
            <Button
              role="menuitem"
              onClick={() => {
                relogin.mutate(account.id, { onSuccess: onLogin, onError: fail });
                close();
              }}
            >
              Redo login
            </Button>
            {account.state === "paused" ? (
              <Button role="menuitem" onClick={() => { resume.mutate(account.id, { onError: fail }); close(); }}>Resume</Button>
            ) : (
              <Button role="menuitem" onClick={() => { pause.mutate(account.id, { onError: fail }); close(); }}>Pause</Button>
            )}
            {account.is_default ? null : (
              <Button role="menuitem" intent="stop" onClick={() => { tryRemove(false); close(); }}>Remove</Button>
            )}
          </div>
        ) : null}
      </td>
    </tr>
  );
}

/** System > Accounts: the pool's default, its accounts, and the way to add one. */
export function AccountsView() {
  const accounts = useAccounts();
  const chosen = useAccountDefault();
  const setDefault = useSetAccountDefault();
  const [login, setLogin] = useState<LoginView | null>(null);

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
      <label className="ac-default">
        Default account
        <select
          value={options.some((o) => o.value === current) ? current : "auto"}
          onChange={(event) => setDefault.mutate(event.target.value)}
        >
          {options.map((o) => (
            <option key={o.value} value={o.value}>{o.label}</option>
          ))}
        </select>
      </label>
      {setDefault.isError ? <div role="alert" className="ac-error">The default could not be saved.</div> : null}

      {list.length === 0 ? (
        <Quiet says="no accounts yet: add one to start the pool" />
      ) : (
        <table className="ac-table">
          <thead>
            <tr>
              <th>Account</th>
              <th>Provider</th>
              <th>Kind</th>
              <th>5h</th>
              <th>7d</th>
              <th>Live sessions</th>
              <th>Reading</th>
              <th><span className="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {list.map((account) => (
              <AccountRow key={account.id} account={account} onLogin={setLogin} />
            ))}
            <tr className="ac-disabled">
              <td>API key</td>
              <td colSpan={6}>Not available yet</td>
              <td>
                <Button disabled>Add</Button>
              </td>
            </tr>
          </tbody>
        </table>
      )}

      {login !== null ? <LoginPanel initial={login} onClose={() => setLogin(null)} /> : <AddAccount onStarted={setLogin} />}
    </div>
  );
}
