import { useState } from "react";
import { classifyWriteFailure, useSettingDoc, useSettingsIndex, useWriteSetting } from "../data/settings";
import { Button, ConflictNote, Segmented, Table, Td, Th } from "../ui";

const NAME = "autopilot";

/** The targets a gate can run locally, in display order. Mirrors `core/src/config.rs`; the núcleo validates. */
export const GATE_TARGETS = ["core", "sidecars", "shell", "tauri", "hooks", "broker"] as const;

type GatePlace = "local" | "ci";
type Choices = Partial<Record<string, GatePlace>>;

const PLACES = [
  { value: "local", label: "Local" },
  { value: "ci", label: "CI only" },
] as const;

/**
 * Where each gate target runs, edited with controls and never as JSON.
 *
 * Saving is a `PUT` of the whole `autopilot` document at the version that was read, with
 * `gate_targets` replaced. The núcleo is the judge of what is valid and answers 422 with a sentence,
 * shown as it came.
 */
export function GateTargets({ projectId }: { projectId: string }) {
  const scope = `project:${projectId}`;
  const index = useSettingsIndex(scope);

  if (index.isPending) return <p className="text-sm text-text-faint">Reading this document…</p>;

  const listed = index.data?.some((row) => row.scope === scope && row.name === NAME);
  if (index.data !== undefined && listed !== true) {
    return <p className="text-sm text-text-muted">This project has no {NAME} document to edit.</p>;
  }
  return <Editor scope={scope} />;
}

type Notice = { kind: "stale" } | { kind: "failed"; text: string };

function storedTargets(body: unknown): Choices {
  if (typeof body !== "object" || body === null) return {};
  const raw = (body as Record<string, unknown>).gate_targets;
  if (typeof raw !== "object" || raw === null) return {};
  const out: Choices = {};
  for (const [key, value] of Object.entries(raw)) {
    if (value === "local" || value === "ci") out[key] = value;
  }
  return out;
}

function Editor({ scope }: { scope: string }) {
  const doc = useSettingDoc(scope, NAME);
  const write = useWriteSetting(scope, NAME);
  // Only what the person picked in this session; the stored mapping shows through underneath.
  const [picked, setPicked] = useState<Choices>({});
  const [notice, setNotice] = useState<Notice | null>(null);

  if (doc.isPending) return <p className="text-sm text-text-faint">Reading this document…</p>;
  if (doc.data === undefined) {
    return <p className="text-sm text-text-muted">The autopilot rules could not be read. Reopening the page asks again.</p>;
  }

  const stored = storedTargets(doc.data.body);
  const shown: Choices = { ...stored, ...picked };
  const dirty = Object.keys(picked).length > 0;
  const base = typeof doc.data.body === "object" && doc.data.body !== null ? doc.data.body : {};
  const version = doc.data.version;

  function save() {
    setNotice(null);
    write.mutate(
      { body: { ...base, gate_targets: shown }, version },
      {
        onSuccess: () => setPicked({}),
        onError: (error) => {
          const failure = classifyWriteFailure(error);
          setNotice(failure.kind === "stale" ? { kind: "stale" } : { kind: "failed", text: sentenceOf(failure) });
        },
      },
    );
  }

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm text-text-muted">
        Where each gate target runs. Until you choose, nothing publishes automatically.
      </p>

      {notice?.kind === "stale" ? (
        <ConflictNote>Someone else changed the autopilot rules. Showing the latest version — try again.</ConflictNote>
      ) : null}

      <Table label="Gate targets">
        <thead>
          <tr>
            <Th>Target</Th>
            <Th>Runs</Th>
            <Th>State</Th>
          </tr>
        </thead>
        <tbody>
          {GATE_TARGETS.map((target) => {
            const place = shown[target];
            return (
              <tr key={target}>
                <Td>{target}</Td>
                <Td>
                  <Segmented<string>
                    label={target}
                    value={place ?? ""}
                    options={PLACES}
                    onChange={(next) => setPicked({ ...picked, [target]: next as GatePlace })}
                  />
                </Td>
                <Td muted>{place === undefined ? "Not set" : ""}</Td>
              </tr>
            );
          })}
          <tr>
            <Td>security</Td>
            <Td muted>CI only (cannot run locally)</Td>
            <Td />
          </tr>
        </tbody>
      </Table>

      {notice?.kind === "failed" ? (
        <p role="alert" className="ui-note ui-note-error">
          {notice.text}
        </p>
      ) : null}

      <div>
        <Button variant="ghost" disabled={!dirty || write.isPending} onClick={save}>
          Save targets
        </Button>
      </div>
    </div>
  );
}

/** One sentence for each way a write can fail; the núcleo's own words win where it has them. */
function sentenceOf(failure: ReturnType<typeof classifyWriteFailure>): string {
  switch (failure.kind) {
    case "invalid":
    case "refused":
      return failure.message;
    case "kill_switch":
      return "The emergency stop is engaged, so nothing was saved.";
    case "not_ours":
      return "The daemon does not let this window edit this document, so nothing was saved.";
    case "unreachable":
      return "The daemon gave no reply, so nothing was saved.";
    case "stale":
      return "Someone else changed the autopilot rules.";
  }
}
