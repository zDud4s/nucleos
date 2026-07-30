import { useCallback, useEffect, useState } from "react";
import {
  API_TOKEN_LEVELS, createApiToken, getBackups, getHealthReadout, listApiTokens,
  restoreBackup, revokeApiToken, takeBackup,
  type ApiTokenLevel, type ApiTokenSummary, type BackupInfo, type ConnectionState,
  type CreatedApiToken, type HealthReadout, type StagedRestore,
} from "./api";
import {
  formatBytes, healthReasonLabel, healthTone, relativeTime, tokenLevelHint,
} from "./derive";
import { Badge, Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";

type Section = "health" | "backups" | "tokens";

const SECTIONS: { key: Section; label: string }[] = [
  { key: "health", label: "Health" },
  { key: "backups", label: "Backups" },
  { key: "tokens", label: "API keys" },
];

/**
 * The readiness readout, subsystem by subsystem.
 *
 * Two things it deliberately does not claim. A sidecar row describes configuration and binary
 * presence, not whether that process is running right now — the supervisor is a fire-and-forget
 * restart loop with no status surface, and the `*_sidecar_binary` names carry that limit. And a
 * `disabled` subsystem is not a fault: an optional pillar nobody turned on drags nothing down, which
 * is why it recedes here instead of colouring the aggregate.
 */
function Health({ token }: { token: string }) {
  const [readout, setReadout] = useState<HealthReadout | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      const next = await getHealthReadout(token);
      if (cancelled) return;
      setReadout(next);
      setLoading(false);
    };
    void load();
    const id = setInterval(() => void load(), 3000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [token]);

  return (
    <Panel
      title="Readiness"
      aside={readout === null ? undefined : <Badge tone={healthTone(readout.status)}>{readout.status}</Badge>}
    >
      {loading && readout === null && <p className="a-note">Probing…</p>}
      {!loading && readout === null && (
        <ErrorNote>Could not read the health readout from the daemon.</ErrorNote>
      )}
      {readout !== null && (
        <ul className="subsystems">
          {readout.subsystems.map((subsystem) => (
            <li key={subsystem.name}>
              <Badge tone={healthTone(subsystem.status)}>{subsystem.status}</Badge>
              <span className="s-name">{subsystem.name}</span>
              <span className="s-reason">{healthReasonLabel(subsystem.reason) ?? ""}</span>
            </li>
          ))}
        </ul>
      )}
      <p className="a-note">
        A sidecar row says what is configured and installed — not whether that process is alive right
        now. Nothing here reports liveness, because the supervisor that restarts sidecars keeps no
        status to report.
      </p>
    </Panel>
  );
}

/**
 * Snapshots of the daemon's database.
 *
 * Restoring is STAGED, not performed: the daemon prepares the swap and it happens on the next
 * daemon start. That distinction is the whole reason this screen names when it applies — a plain
 * "restored" would have someone believe their data was already back.
 */
function Backups({ token }: { token: string }) {
  const [backups, setBackups] = useState<BackupInfo[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [staged, setStaged] = useState<StagedRestore | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [failed, setFailed] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const next = await getBackups(token);
    setBackups(next);
    setLoading(false);
  }, [token]);

  useEffect(() => { void refresh(); }, [refresh]);

  async function take() {
    setBusy(true);
    setFailed(null);
    setNote(null);
    setStaged(null);
    const info = await takeBackup(token);
    setBusy(false);
    if (info === null) {
      setFailed("Could not take a backup.");
      return;
    }
    setNote(`Took ${info.name} (${formatBytes(info.size_bytes)}).`);
    await refresh();
  }

  async function restore(name: string) {
    setBusy(true);
    setFailed(null);
    setNote(null);
    const result = await restoreBackup(token, name);
    setBusy(false);
    if (result === null) {
      setFailed(`Could not stage a restore from ${name}.`);
      return;
    }
    setStaged(result);
  }

  return (
    <Panel title="Backups" aside={backups === null ? undefined : `${backups.length} kept`}>
      <div className="form-actions">
        <Button disabled={busy} onClick={() => void take()}>
          {busy ? "Working…" : "Take a backup now"}
        </Button>
        <span className="cta-note">
          Older snapshots are pruned automatically; backups are never overwritten.
        </span>
      </div>
      {staged !== null && (
        <p className="gate-note">
          Restore from <b>{staged.name}</b> is staged — it applies <b>{staged.applies}</b>. Nothing
          has been swapped yet; the current database is still the one in use.
        </p>
      )}
      {note !== null && <p className="gate-note">{note}</p>}
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
      {loading && backups === null && <p className="a-note">Loading…</p>}
      {!loading && backups === null && (
        <ErrorNote>Could not list backups. The daemon may not know where its database lives.</ErrorNote>
      )}
      {backups !== null && backups.length === 0 && (
        <Teach title="No backups yet.">
          A backup is one consistent snapshot of the daemon&apos;s database, taken while it runs.
          Take one before anything you would want to undo.
        </Teach>
      )}
      <ul className="backups">
        {(backups ?? []).map((backup) => (
          <li key={backup.name}>
            <span className="b-name">{backup.name}</span>
            <span className="b-meta">
              {formatBytes(backup.size_bytes)}
              {backup.migration_version !== null && ` · schema v${backup.migration_version}`}
            </span>
            <ConfirmButton
              size="sm"
              variant="danger"
              confirmLabel="Stage this restore?"
              disabled={busy}
              onConfirm={() => void restore(backup.name)}
            >
              Restore
            </ConfirmButton>
          </li>
        ))}
      </ul>
    </Panel>
  );
}

/**
 * The durable keys other programs use to reach the daemon.
 *
 * The secret is shown exactly once, at creation, and never again by listing — so this screen keeps
 * the freshly minted one on screen until it is dismissed, rather than clearing it on the next
 * refresh. Losing it means revoking and minting another; there is no way to read it back.
 */
function Tokens({ token }: { token: string }) {
  const [tokens, setTokens] = useState<ApiTokenSummary[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [name, setName] = useState("");
  const [level, setLevel] = useState<ApiTokenLevel>("read-only");
  const [busy, setBusy] = useState(false);
  const [minted, setMinted] = useState<CreatedApiToken | null>(null);
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const next = await listApiTokens(token);
    setTokens(next);
    setLoading(false);
  }, [token]);

  useEffect(() => { void refresh(); }, [refresh]);

  async function mint() {
    setBusy(true);
    setFailed(null);
    setCopied(false);
    const result = await createApiToken(token, name.trim(), level);
    setBusy(false);
    if (!result.ok) {
      setFailed(
        result.status === 409
          ? "A key with that name already exists. Revoke it first, or pick another name."
          : result.status === 400
            ? "That name was rejected. Use letters, digits, dashes and underscores."
            : result.status === 403
              ? "This token is not allowed to mint keys — that needs an admin key."
              : "Could not create the key.",
      );
      return;
    }
    setMinted(result.value);
    setName("");
    await refresh();
  }

  async function revoke(tokenName: string) {
    setBusy(true);
    setFailed(null);
    const gone = await revokeApiToken(token, tokenName);
    setBusy(false);
    if (!gone) {
      setFailed(`Could not revoke ${tokenName}.`);
      return;
    }
    if (minted?.name === tokenName) setMinted(null);
    await refresh();
  }

  return (
    <>
      <Panel title="Mint a key">
        <form
          className="filters"
          onSubmit={(event) => {
            event.preventDefault();
            if (name.trim() === "" || busy) return;
            void mint();
          }}
        >
          <label>
            Name
            <input value={name} placeholder="agent-cli" onChange={(event) => setName(event.target.value)} />
          </label>
          <label>
            Level
            <select
              value={level}
              onChange={(event) => setLevel(event.target.value as ApiTokenLevel)}
            >
              {API_TOKEN_LEVELS.map((option) => (
                <option key={option} value={option}>{option}</option>
              ))}
            </select>
          </label>
          <div className="form-actions">
            <Button type="submit" variant="approve" disabled={name.trim() === "" || busy}>
              {busy ? "Minting…" : "Mint key"}
            </Button>
          </div>
        </form>
        <p className="cta-note">{tokenLevelHint(level)}</p>
        {minted !== null && (
          <div className="minted">
            <p className="gate-note">
              This is the only time <b>{minted.name}</b> can be read. Copy it now — listing never
              returns it again, and a lost key can only be replaced, not recovered.
            </p>
            <code className="secret">{minted.token}</code>
            <div className="a-actions">
              <Button
                size="sm"
                onClick={() => {
                  void navigator.clipboard?.writeText(minted.token).then(() => setCopied(true));
                }}
              >
                {copied ? "Copied" : "Copy"}
              </Button>
              <Button size="sm" onClick={() => { setMinted(null); setCopied(false); }}>
                Dismiss
              </Button>
            </div>
          </div>
        )}
        {failed !== null && <ErrorNote>{failed}</ErrorNote>}
      </Panel>
      <Panel title="Keys" aside={tokens === null ? undefined : `${tokens.length} active`}>
        {loading && tokens === null && <p className="a-note">Loading…</p>}
        {!loading && tokens === null && (
          <ErrorNote>Could not list keys. Reading them needs an admin token.</ErrorNote>
        )}
        {tokens !== null && tokens.length === 0 && (
          <Teach title="No keys yet.">
            A key lets another program — an agent CLI, a script — talk to the daemon without the
            shell&apos;s own credential. Give each one the narrowest level that does its job.
          </Teach>
        )}
        <ul className="tokens">
          {(tokens ?? []).map((entry) => (
            <li key={entry.name}>
              <span className="t-name">{entry.name}</span>
              <Badge tone={entry.level === "admin" ? "pending" : entry.level === "run-creating" ? "paused" : "off"}>
                {entry.level}
              </Badge>
              <time dateTime={entry.created_at} title={entry.created_at}>
                {relativeTime(entry.created_at)}
              </time>
              <ConfirmButton
                size="sm"
                variant="danger"
                confirmLabel="Confirm revoke?"
                disabled={busy}
                onConfirm={() => void revoke(entry.name)}
              >
                Revoke
              </ConfirmButton>
            </li>
          ))}
        </ul>
      </Panel>
    </>
  );
}

interface SystemProps {
  token: string | null;
  connection: ConnectionState;
}

/**
 * The machine's own upkeep: is it well, can it be rolled back, and who else may talk to it.
 *
 * One tab with three sections rather than three tabs, because none of them is a place anyone spends
 * time — they are visited when something is wrong, when something is about to change, or when a new
 * program needs a key.
 */
function System({ token, connection }: SystemProps) {
  const [section, setSection] = useState<Section>("health");

  if (connection !== "connected" || token === null) {
    return (
      <section className="system">
        <Teach title="System is waiting for the daemon.">
          Connect to the daemon to read its health, take a backup, or manage the keys other programs
          use to reach it.
        </Teach>
      </section>
    );
  }

  return (
    <section className="system">
      <h1 className="headline">The núcleo&apos;s own upkeep.</h1>
      <nav className="subnav" aria-label="System sections">
        {SECTIONS.map((entry) => (
          <button
            type="button"
            key={entry.key}
            className="subtab"
            aria-current={section === entry.key ? "page" : undefined}
            onClick={() => setSection(entry.key)}
          >
            {entry.label}
          </button>
        ))}
      </nav>
      <div className="stack">
        {section === "health" && <Health token={token} />}
        {section === "backups" && <Backups token={token} />}
        {section === "tokens" && <Tokens token={token} />}
      </div>
    </section>
  );
}

export default System;
