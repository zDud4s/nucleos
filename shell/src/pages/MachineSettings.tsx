import { useCallback, useEffect, useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  useForgetSecret,
  useMachineConfig,
  useMachineSecrets,
  useStoreSecret,
  useWriteMachineSetting,
  type MachineSecret,
  type MachineSetting,
} from "../data/machine-config";
import { Button, ConfirmButton, ErrorNote, Panel, RefusalNote, StateBadge } from "../ui";
import { DistillerModel } from "./DistillerModel";
import { CaptureWait } from "./CaptureWait";
import { EmbeddingModel } from "./EmbeddingModel";

/**
 * This machine's settings — the files whose author is the daemon rather than
 * any project.
 *
 * It replaces the read-only `ConfigIndex` that stood here, and in particular it
 * replaces that panel's honest confession: a hard-coded list naming `web`,
 * `browser`, `council` and `models` as areas the núcleo exposed no route for.
 * There were three config routes and all three were GET, so every pillar on
 * this machine was turned on by editing a YAML file by hand and restarting.
 *
 * # Why a validated text box and not nine forms
 *
 * A form per pillar is the better surface and it is the next slice, not this
 * one: nine forms is nine schemas duplicated in TypeScript, and a schema
 * duplicated is a schema that drifts from the Rust that actually parses it. The
 * daemon already owns the grammar and now answers with the parser's own words,
 * so a text box that cannot save something the daemon would refuse is honest,
 * complete on day one, and cannot fall out of step with `config.rs`. What it is
 * not is easy for somebody who does not know the file's keys — which is exactly
 * what `what` and the placeholder are for, and exactly what the forms will fix.
 *
 * # The two things this page must not hide
 *
 * **Where the file is.** There are twenty-odd worktrees on this machine and
 * every one has an `.ai/`, so the absolute path is shown per row rather than
 * the relative one. Editing settings with great confidence in the wrong
 * checkout is the failure this costs one line to prevent.
 *
 * **When the edit starts mattering.** Eight of the nine files are read once, at
 * startup. A page that reported "saved" and left it there would be claiming an
 * effect that has not happened — so the daemon's own sentence about it is
 * rendered on every row, and again after a save.
 */
export function MachineSettings() {
  const config = useMachineConfig();
  const secrets = useMachineSecrets();
  const [chosen, setChosen] = useState<string | null>(null);
  // Which files have an unsaved draft: every panel stays mounted, so without this a
  // half-written file is invisible once another one is chosen.
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

  if (config.isPending) return <p className="sy-loading">reading this machine's settings…</p>;
  if (config.error) {
    if (isApiRefusal(config.error)) return <RefusalNote refusal={config.error} />;
    return <ErrorNote>the núcleo did not answer — nothing is known about this machine's settings</ErrorNote>;
  }

  const area = chosen ?? config.data.settings[0]?.area ?? null;

  return (
    <>
      <DistillerModel />
      <CaptureWait />
      <EmbeddingModel />

      {/* The folder, as one line above the files rather than a panel of its own. */}
      <p className="sy-machine-intro">
        These files belong to the daemon rather than to any project, and live in{" "}
        <code className="sy-machine-root" title={config.data.root}>
          {config.data.root}
        </code>
        . Each is validated before a byte is written: a file that would not parse is refused with
        the parser's own words, and what is on disk is left alone.
      </p>

      {/*
        A list of the files and one of them open, rather than nine editors stacked. Every panel
        stays mounted and only the chosen one is shown, so a half-written file survives a look
        at another one.
      */}
      <div className="sy-settings">
        <ul className="sy-settings-list" aria-label="Settings files">
          {config.data.settings.map((setting) => {
            const own = (secrets.data?.secrets ?? []).filter((row) => row.area === setting.area);
            const current = setting.area === area;
            return (
              <li key={setting.path}>
                <button
                  type="button"
                  className={current ? "sy-settings-item sy-settings-item-current" : "sy-settings-item"}
                  aria-current={current ? "true" : undefined}
                  aria-describedby={`marks-${setting.area}`}
                  onClick={() => setChosen(setting.area)}
                >
                  <span className="sy-settings-item-name">{setting.area}</span>
                  {dirtyAreas.has(setting.area) && (
                    <span className="sy-meta sy-settings-item-dirty" aria-hidden="true">
                      unsaved
                    </span>
                  )}
                  {/* The marks are drawn for the eye; this is what a screen reader gets, as the
                      button's description so its NAME stays the visible area. */}
                  <span className="sr-only" id={`marks-${setting.area}`}>
                    {markSummary(setting, own, dirtyAreas.has(setting.area))}
                  </span>
                  {/* Two dots, file and credential: filled when there is one, hollow when not. */}
                  <span className="sy-settings-item-marks" aria-hidden="true">
                    <span
                      className={setting.exists ? "sy-mark sy-mark-on" : "sy-mark"}
                      title={setting.exists ? "file written" : "file never written"}
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

        <div className="sy-settings-detail">
          {config.data.settings.map((setting) => (
            <div key={setting.path} hidden={setting.area !== area}>
              <SettingPanel
                setting={setting}
                onDirty={markDirty}
                secrets={(secrets.data?.secrets ?? []).filter((row) => row.area === setting.area)}
              />
            </div>
          ))}
        </div>
      </div>
    </>
  );
}

/** The file and credential marks as words, for the row's accessible description. */
function markSummary(setting: MachineSetting, own: MachineSecret[], unsaved: boolean): string {
  const missing = own.filter((secret) => secret.present !== true).length;
  const parts = [setting.exists ? "file written" : "file never written"];
  if (own.length > 0) {
    parts.push(missing === 0 ? "credentials set" : `${missing} credential${missing === 1 ? "" : "s"} not set`);
  }
  if (unsaved) parts.push("unsaved changes");
  return parts.join(", ");
}

/**
 * One settings file: what it decides, where it is, what is in it, and a way to
 * change it.
 *
 * The draft is component state keyed by nothing, which is deliberate: a row
 * whose draft survived a refetch would quietly show somebody their own unsaved
 * text as though it were the file. `contents ?? ""` seeds it once and the
 * `saved` flag is what says the two agree.
 */
function SettingPanel({
  setting,
  secrets,
  onDirty,
}: {
  setting: MachineSetting;
  secrets: MachineSecret[];
  onDirty: (area: string, dirty: boolean) => void;
}) {
  const write = useWriteMachineSetting();
  const [draft, setDraft] = useState(setting.contents ?? "");
  const [saved, setSaved] = useState(false);

  const dirty = draft !== (setting.contents ?? "");
  useEffect(() => {
    onDirty(setting.area, dirty);
  }, [onDirty, setting.area, dirty]);

  const save = () => {
    if (!dirty || write.isPending) return;
    write.mutate({ path: setting.path, contents: draft }, { onSuccess: () => setSaved(true) });
  };
  const errorId = `setting-${setting.area}-error`;

  return (
    <Panel
      title={setting.area}
      aside={<StateBadge domain="machine_file" state={setting.exists ? "configured" : "unconfigured"} />}
    >
      <p className="sy-note">{setting.what}</p>

      <dl className="sy-config-facts">
        <div className="sy-fact">
          <dt>file</dt>
          <dd>{setting.resolved}</dd>
        </div>
        <div className="sy-fact">
          <dt>takes effect</dt>
          <dd>{setting.takes_effect}</dd>
        </div>
      </dl>

      <label className="sy-setting-label" htmlFor={`setting-${setting.area}`}>
        {setting.display}
      </label>
      <textarea
        id={`setting-${setting.area}`}
        className="sy-setting-editor"
        spellCheck={false}
        rows={14}
        value={draft}
        aria-invalid={write.error != null ? true : undefined}
        aria-describedby={write.error != null ? errorId : undefined}
        onKeyDown={(event) => {
          if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
            event.preventDefault();
            save();
          }
        }}
        placeholder={
          setting.exists
            ? undefined
            : "this file does not exist yet — writing anything here creates it"
        }
        onChange={(event) => {
          setDraft(event.target.value);
          setSaved(false);
        }}
      />

      <div className="sy-setting-controls">
        <Button
          disabled={!dirty || write.isPending}
          onClick={save}
        >
          {write.isPending ? "saving…" : "Save"}
        </Button>
        {dirty && !write.isPending && (
          <Button
            variant="ghost"
            onClick={() => {
              setDraft(setting.contents ?? "");
              setSaved(false);
            }}
          >
            Revert
          </Button>
        )}
        {dirty && !write.isPending && <span className="sy-meta">unsaved</span>}
        <span className="sy-meta" role="status">
          {saved && !dirty ? <>saved &mdash; {setting.takes_effect}</> : null}
        </span>
      </div>

      {/* The parser's own words. A generic "invalid" would send somebody back to
          a file they cannot see to look for a line nobody named. */}
      {write.error != null && (
        <div id={errorId}>
          {isApiRefusal(write.error) ? (
            <RefusalNote refusal={write.error} />
          ) : (
            <ErrorNote>the núcleo did not answer — nothing was written</ErrorNote>
          )}
        </div>
      )}

      {/* After the file, not before it: the credential is the second half of the same
          decision, and above the file it pushed every editor in the grid out of line. */}
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
          {store.isPending ? "storing…" : "Set"}
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
          <ErrorNote>the núcleo did not answer — nothing was stored</ErrorNote>
        ))}
      {/* The destructive half fails as loudly as the other: a Forget that silently did
          nothing leaves the credential set while the button goes back to normal. */}
      {forget.error != null &&
        (isApiRefusal(forget.error) ? (
          <RefusalNote refusal={forget.error} />
        ) : (
          <ErrorNote>the núcleo did not answer — nothing was forgotten</ErrorNote>
        ))}
    </div>
  );
}
