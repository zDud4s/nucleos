import { useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  useMachineConfig,
  useWriteMachineSetting,
  type MachineSetting,
} from "../data/machine-config";
import { Badge, Button, ErrorNote, Panel, RefusalNote } from "../ui";

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

  if (config.isPending) return <p className="sy-loading">reading this machine's settings…</p>;
  if (config.error) {
    if (isApiRefusal(config.error)) return <RefusalNote refusal={config.error} />;
    return <ErrorNote>the núcleo did not answer — nothing is known about this machine's settings</ErrorNote>;
  }

  return (
    <>
      <Panel title="This machine">
        <p className="sy-note">
          These files belong to the daemon rather than to any project, and they are read from the
          directory it was launched in. Every one of them is validated before a byte is written: a
          file that would not parse is refused with the parser's own words, and what is on disk is
          left alone.
        </p>
        <dl className="sy-config-facts">
          <div className="sy-fact">
            <dt>working directory</dt>
            <dd>{config.data.root}</dd>
          </div>
        </dl>
      </Panel>

      {config.data.settings.map((setting) => (
        <SettingPanel key={setting.path} setting={setting} />
      ))}
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
function SettingPanel({ setting }: { setting: MachineSetting }) {
  const write = useWriteMachineSetting();
  const [draft, setDraft] = useState(setting.contents ?? "");
  const [saved, setSaved] = useState(false);

  const dirty = draft !== (setting.contents ?? "");

  return (
    <Panel
      title={setting.area}
      aside={
        setting.exists ? (
          <Badge tone="info">configured</Badge>
        ) : (
          <Badge tone="off">never configured</Badge>
        )
      }
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
        <span className="sy-meta">{setting.path}</span>
      </label>
      <textarea
        id={`setting-${setting.area}`}
        className="sy-setting-editor"
        spellCheck={false}
        rows={8}
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
    </Panel>
  );
}
