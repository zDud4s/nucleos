import { useState } from "react";
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

  if (config.isPending) return <p className="sy-loading">reading this machine's settings…</p>;
  if (config.error) {
    if (isApiRefusal(config.error)) return <RefusalNote refusal={config.error} />;
    return <ErrorNote>the núcleo did not answer — nothing is known about this machine's settings</ErrorNote>;
  }

  const area = chosen ?? config.data.settings[0]?.area ?? null;

  return (
    <>
      <DistillerModel />
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
                  onClick={() => setChosen(setting.area)}
                >
                  <span className="sy-settings-item-name">{setting.area}</span>
                  {/* Two dots, file and credential: filled when there is one, hollow when not. */}
                  <span className="sy-settings-item-marks">
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
                secrets={(secrets.data?.secrets ?? []).filter((row) => row.area === setting.area)}
              />
            </div>
          ))}
        </div>
      </div>
    </>
  );
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
}: {
  setting: MachineSetting;
  secrets: MachineSecret[];
}) {
  const write = useWriteMachineSetting();
  const [draft, setDraft] = useState(setting.contents ?? "");
  const [saved, setSaved] = useState(false);

  const dirty = draft !== (setting.contents ?? "");

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
        <span className="sy-meta">{setting.display}</span>
      </label>
      <textarea
        id={`setting-${setting.area}`}
        className="sy-setting-editor"
        spellCheck={false}
        rows={14}
        value={draft}
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
          onClick={() => {
            write.mutate(
              { path: setting.path, contents: draft },
              { onSuccess: () => setSaved(true) },
            );
          }}
        >
          {write.isPending ? "saving…" : "Save"}
        </Button>
        {dirty && !write.isPending && <span className="sy-meta">unsaved</span>}
        {saved && !dirty && <span className="sy-meta">saved &mdash; {setting.takes_effect}</span>}
      </div>

      {/* The parser's own words. A generic "invalid" would send somebody back to
          a file they cannot see to look for a line nobody named. */}
      {write.error != null &&
        (isApiRefusal(write.error) ? (
          <RefusalNote refusal={write.error} />
        ) : (
          <ErrorNote>the núcleo did not answer — nothing was written</ErrorNote>
        ))}

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
          }}
        />
        <Button
          disabled={value === "" || store.isPending}
          onClick={() => {
            store.mutate(
              { key: secret.key, value },
              // Cleared on success only. Clearing it on failure would take away
              // what somebody pasted along with the error telling them why.
              { onSuccess: () => { setValue(""); } },
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
              forget.mutate(secret.key);
            }}
          />
        )}
      </div>
      {store.error != null &&
        (isApiRefusal(store.error) ? (
          <RefusalNote refusal={store.error} />
        ) : (
          <ErrorNote>the núcleo did not answer — nothing was stored</ErrorNote>
        ))}
    </div>
  );
}
