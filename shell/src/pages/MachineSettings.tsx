import { useCallback, useState } from "react";
import { isApiRefusal } from "../data/client";
import { useForgetSecret, useMachineSecrets, useStoreSecret, type MachineSecret } from "../data/secrets";
import { useSettingsIndex, type SettingIndexRow } from "../data/settings";
import { SettingsForm } from "../settings/SettingsForm";
import { Button, ConfirmButton, ErrorNote, Panel, RefusalNote, Skeleton, StateBadge } from "../ui";
import { EngineOptIn } from "./EngineOptIn";
import { DistillerModel } from "./DistillerModel";
import { CaptureWait } from "./CaptureWait";
import { EmbeddingModel } from "./EmbeddingModel";
import { DAEMON_NAME } from "../copy";
import { plural } from "../lib/plural";

/**
 * This machine's settings: the documents whose author is the daemon rather than any project.
 *
 * Each one is drawn by `SettingsForm` from the schema the núcleo serves with it, so nothing here knows what a
 * pillar contains. What this page owns is the list: which documents exist, which has an unsaved draft, and the
 * credentials that sit beside the document they belong with.
 *
 * # What is fetched, and when
 *
 * The index comes first and is enough to draw the list. A document's own `GET` waits until its panel is first
 * opened, so opening this screen does not cost a request per pillar. Once opened a panel stays mounted and is only
 * hidden, so a half-written document survives a look at another one.
 */
export function MachineSettings() {
  const index = useSettingsIndex();
  const secrets = useMachineSecrets();
  const [chosen, setChosen] = useState<string | null>(null);
  // Every panel opened so far. The first document is open without anyone choosing it, so `choose` records the one
  // being left as well as the one being opened.
  const [opened, setOpened] = useState<ReadonlySet<string>>(new Set());
  // Which documents have an unsaved draft: without this a half-written one is invisible once another is chosen.
  const [dirtyAreas, setDirtyAreas] = useState<ReadonlySet<string>>(new Set());
  const markDirty = useCallback((areaName: string, dirty: boolean) => {
    setDirtyAreas((previous) => {
      if (previous.has(areaName) === dirty) return previous;
      const next = new Set(previous);
      if (dirty) next.add(areaName);
      else next.delete(areaName);
      return next;
    });
  }, []);

  if (index.isPending) return <Skeleton label="Reading this machine's settings" lines={6} />;
  if (index.error) {
    if (isApiRefusal(index.error)) return <RefusalNote refusal={index.error} />;
    return <ErrorNote>the {DAEMON_NAME} is not responding: nothing is known about this machine's settings</ErrorNote>;
  }

  const rows = index.data.filter((row) => row.scope === "machine");
  const area = chosen ?? rows[0]?.name ?? null;
  const choose = (name: string) => {
    setOpened((previous) => {
      const next = new Set(previous);
      if (area !== null) next.add(area);
      next.add(name);
      return next;
    });
    setChosen(name);
  };

  return (
    <>
      <Panel title="Brain">
        <div className="sy-machine-prefs">
          <DistillerModel />
          <CaptureWait />
          <EmbeddingModel />
        </div>
      </Panel>

      <Panel title="Engines">
        <EngineOptIn engine="agy" label="Antigravity CLI (agy)" />
      </Panel>

      <p className="sy-machine-intro">
        These settings belong to the daemon rather than to any project. Each is checked before anything is written: a
        change that would not read is refused with the daemon's own words, and what is stored is left alone.
      </p>

      {/* A list of the documents and one of them open, rather than nine forms stacked. */}
      <div className="sy-settings">
        <div className="sy-settings-nav">
        <ul className="sy-settings-list" aria-label="Settings documents">
          {rows.map((row) => {
            const own = (secrets.data?.secrets ?? []).filter((secret) => secret.area === row.name);
            const current = row.name === area;
            return (
              <li key={row.name}>
                <button
                  type="button"
                  className={current ? "sy-settings-item sy-settings-item-current" : "sy-settings-item"}
                  aria-current={current ? "true" : undefined}
                  aria-describedby={`marks-${row.name}`}
                  onClick={() => choose(row.name)}
                >
                  <span className="sy-settings-item-name">{row.name}</span>
                  {dirtyAreas.has(row.name) && (
                    <span className="sy-meta sy-settings-item-dirty" aria-hidden="true">
                      unsaved
                    </span>
                  )}
                  {/* The marks are drawn for the eye; this is what a screen reader gets, as the
                      button's description so its NAME stays the visible area. */}
                  <span className="sr-only" id={`marks-${row.name}`}>
                    {markSummary(row, own, dirtyAreas.has(row.name))}
                  </span>
                  {/* Two dots, document and credential: filled when there is one, hollow when not. */}
                  <span className="sy-settings-item-marks" aria-hidden="true">
                    <span
                      className={row.present ? "sy-mark sy-mark-on" : "sy-mark"}
                      title={row.present ? "written" : "never written"}
                    />
                    {own.map((secret) => (
                      <span
                        key={secret.key}
                        className={secret.present === true ? "sy-mark sy-mark-key sy-mark-on" : "sy-mark sy-mark-key"}
                        title={`${secret.key}: ${secret.present === true ? "set" : secret.present === false ? "not set" : "unknown"}`}
                      />
                    ))}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
        {/* The marks are aria-hidden words-in-waiting; the legend says them to the eye. */}
        <p className="sy-meta sy-settings-legend" aria-hidden="true">
          <span className="sy-settings-legend-item">
            <span className="sy-mark sy-mark-on" /> written
          </span>
          <span className="sy-settings-legend-item">
            <span className="sy-mark sy-mark-key sy-mark-on" /> credential set
          </span>
        </p>
        </div>

        <div className="sy-settings-detail">
          {rows
            .filter((row) => row.name === area || opened.has(row.name))
            .map((row) => (
              <div key={row.name} hidden={row.name !== area}>
                <SettingPanel
                  row={row}
                  onDirty={markDirty}
                  secrets={(secrets.data?.secrets ?? []).filter((secret) => secret.area === row.name)}
                />
              </div>
            ))}
        </div>
      </div>
    </>
  );
}

/** The document and credential marks as words, for the row's accessible description. */
function markSummary(row: SettingIndexRow, own: MachineSecret[], unsaved: boolean): string {
  const missing = own.filter((secret) => secret.present !== true).length;
  const parts = [row.present ? "written" : "never written"];
  if (own.length > 0) {
    parts.push(missing === 0 ? "credentials set" : `${plural(missing, "credential")} not set`);
  }
  if (unsaved) parts.push("unsaved changes");
  return parts.join(", ");
}

/** One document and, after it, the credentials that are the second half of the same decision. */
function SettingPanel({
  row,
  secrets,
  onDirty,
}: {
  row: SettingIndexRow;
  secrets: MachineSecret[];
  onDirty: (area: string, dirty: boolean) => void;
}) {
  const name = row.name;
  const onFormDirty = useCallback((dirty: boolean) => onDirty(name, dirty), [onDirty, name]);

  return (
    <Panel
      title={name}
      aside={<StateBadge domain="machine_file" state={row.present ? "configured" : "unconfigured"} />}
    >
      <SettingsForm scope="machine" name={name} takesEffect={row.takes_effect} onDirty={onFormDirty} />

      {secrets.map((secret) => (
        <SecretControl key={secret.key} secret={secret} />
      ))}
    </Panel>
  );
}

/**
 * One credential: whether it is set, a box to set it, and a way to forget it.
 *
 * It sits inside the panel for its own area rather than in a list of its own,
 * which the núcleo makes safe to rely on — a test there asserts every credential
 * names an area that has a settings file. The pairing is the useful one: a
 * mailbox and its password are one decision, and putting them on two screens is
 * how somebody configures half of a pillar and cannot see why it is still off.
 *
 * The value is write-only, everywhere. There is no route that serves one back,
 * the input is cleared the moment it is accepted, and `present` is the only
 * thing this ever renders about a stored credential.
 */
function SecretControl({ secret }: { secret: MachineSecret }) {
  const store = useStoreSecret();
  const forget = useForgetSecret();
  const [value, setValue] = useState("");
  // What just happened, said in a live region: an emptied input is not an announcement.
  const [outcome, setOutcome] = useState<"stored" | "forgotten" | null>(null);

  return (
    <div className="sy-secret">
      <div className="sy-secret-head">
        <span className="sy-secret-key" title={secret.what}>
          {secret.key}
        </span>
        {/* "unknown" is not the same as "not set", and acting on the two differs: one wants a
            credential pasted, the other wants somebody to look at the store. */}
        <StateBadge
          domain="credential"
          state={secret.present === true ? "set" : secret.present === false ? "unset" : "unknown"}
        />
      </div>
      <p className="sy-field-hint">{secret.what}</p>
      <div className="sy-setting-controls">
        <input
          type="password"
          className="sy-secret-input"
          aria-label={secret.key}
          autoComplete="off"
          spellCheck={false}
          value={value}
          placeholder={secret.present === true ? "replace it" : "paste it here"}
          onChange={(event) => {
            setValue(event.target.value);
            setOutcome(null);
          }}
        />
        <Button
          disabled={value === "" || store.isPending}
          onClick={() => {
            store.mutate(
              { key: secret.key, value },
              // Cleared on success only. Clearing it on failure would take away
              // what somebody pasted along with the error telling them why.
              { onSuccess: () => { setValue(""); setOutcome("stored"); } },
            );
          }}
        >
          {store.isPending ? "Storing…" : "Set"}
        </Button>
        {secret.present === true && (
          <ConfirmButton
            // Danger: forgetting a credential destroys the only copy the núcleo holds.
            variant="danger"
            intent="stop"
            label="Forget"
            confirmLabel="Forget it"
            /* Said, not drawn: the key is on the line above, and drawn into the armed
               label it made the button wider than the box it is for. */
            sayAs={`Forget ${secret.key}`}
            onConfirm={() => {
              forget.mutate(secret.key, { onSuccess: () => { setOutcome("forgotten"); } });
            }}
          />
        )}
      </div>
      <p className="sy-meta" role="status">
        {outcome === "stored" && "stored"}
        {outcome === "forgotten" && "forgotten"}
      </p>
      {store.error != null &&
        (isApiRefusal(store.error) ? (
          <RefusalNote refusal={store.error} />
        ) : (
          <ErrorNote>the {DAEMON_NAME} is not responding: nothing was stored</ErrorNote>
        ))}
      {/* The destructive half fails as loudly as the other: a Forget that silently did
          nothing leaves the credential set while the button goes back to normal. */}
      {forget.error != null &&
        (isApiRefusal(forget.error) ? (
          <RefusalNote refusal={forget.error} />
        ) : (
          <ErrorNote>the {DAEMON_NAME} is not responding: nothing was forgotten</ErrorNote>
        ))}
    </div>
  );
}
