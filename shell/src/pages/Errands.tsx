import { useState } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  useCloseErrand,
  useCreateErrandRule,
  useDeleteErrandRule,
  useErrandFile,
  useErrandFiles,
  useErrandNotebook,
  useErrandRules,
  useErrands,
  usePatchErrand,
  useWriteErrandFile,
  type Errand,
  type ErrandRule,
  type ErrandStatus,
} from "../data/errands";
import {
  Button,
  ConfirmButton,
  Count,
  ErrorNote,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  StaleNote,
  StateBadge,
  Teach,
  Well,
} from "../ui";
import "./errands.css";

/**
 * Errands — one component serving `/errands` and `/errands/$errandId`, the
 * `Council` pattern: a list that is always on screen, with the detail added
 * below it once something is selected rather than replacing it.
 *
 * **There is no create control anywhere on this page.** `POST /errands` takes
 * a `chat_key` — a Telegram topic id composed by the sidecar — and nothing on
 * this side of the wire can invent one. An errand is opened from Telegram and
 * appears here, never the other way round.
 *
 * **There is no `GET /errands/{id}`.** The detail is a row out of the same
 * list this page polls at `POLL.queue`; a pause, a brain change, a set
 * investigation or a close all invalidate the list, and the refetch that
 * follows is what the detail panel sees change.
 */
export function Errands() {
  const params = useParams({ strict: false }) as { errandId?: string };
  const errandId = params.errandId ?? null;

  const errands = useErrands();
  const rows = errands.data ?? [];
  const answered = errands.data !== undefined;
  const stale = errands.isError && answered;
  const selected = errandId === null ? undefined : rows.find((row) => String(row.id) === errandId);

  return (
    <>
      <PageHeader title="Errands" headline={headlineFor(rows, answered)} />

      {stale && <StaleNote dataUpdatedAt={errands.dataUpdatedAt} />}
      {errands.isError && !answered && <ListError error={errands.error} />}

      <ErrandList rows={rows} answered={answered} selected={errandId} />

      {errandId === null && (
        <Teach title="Errands are opened from Telegram">
          <p>
            An errand starts on a Telegram topic — <code>chat_key</code> is the topic&apos;s own id,
            composed by the Telegram sidecar, and nothing on this side of the wire can invent one. That
            is why this page offers no button to start one: open a topic in Telegram, and its errand
            appears here on its own.
          </p>
          {rows.length > 0 && (
            <p>Pick one from the list above to see its notebook, its files and its rules.</p>
          )}
        </Teach>
      )}

      {errandId !== null && selected !== undefined && <ErrandDetail key={errandId} errand={selected} />}
      {errandId !== null && selected === undefined && answered && (
        <Panel title="Errand">
          <Quiet says="there is no errand with that id." />
        </Panel>
      )}
      {errandId !== null && selected === undefined && !answered && !errands.isError && (
        <Panel title="Errand">
          <p className="errands-loading">reading the errands…</p>
        </Panel>
      )}
    </>
  );
}

/** One derived sentence about the whole list. */
function headlineFor(rows: Errand[], answered: boolean): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0) return "no errand has been opened from Telegram";
  const noun = rows.length === 1 ? "errand" : "errands";
  const answering = rows.filter((row) => row.status === "active").length;
  return answering === 0 ? `${rows.length} ${noun}, none answering` : `${rows.length} ${noun}, ${answering} answering`;
}

function ListError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the errands</ErrorNote>;
}

/**
 * The daemon's own sentence, when it really sent one.
 *
 * `client.ts` falls back to `statusText` for a refusal with an empty body, so
 * a bare status arrives carrying only the status word — four words is the
 * floor between that and a sentence the daemon wrote on purpose, such as the
 * rule-creation refusal naming the word that made a cron unreadable.
 */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

/* ------------------------------------------------------------------- list -- */

function ErrandList({
  rows,
  answered,
  selected,
}: {
  rows: Errand[];
  answered: boolean;
  selected: string | null;
}) {
  if (answered && rows.length === 0) return null;

  return (
    <Panel title="Errands" aside={<Count n={answered ? rows.length : undefined} />}>
      {!answered && <p className="errands-loading">reading the errands…</p>}
      {rows.length > 0 && (
        <Rows label="Errands">
          {rows.map((row) => (
            <ErrandRow key={row.id} errand={row} active={String(row.id) === selected} />
          ))}
        </Rows>
      )}
    </Panel>
  );
}

/**
 * What an errand's investigation reads as.
 *
 * `done_when: null` is not an empty field — it means this errand answers when
 * spoken to and nothing else. `windows_left: 0` is not an empty field either
 * — it means no turns of its own initiative are left. Neither must read as a
 * gap where a fact should be.
 */
function investigationText(errand: Errand): string {
  if (errand.done_when === null) return "answers when spoken to and nothing else";
  const windows =
    errand.windows_left === 0
      ? "no turns of its own initiative left"
      : `${errand.windows_left} turn${errand.windows_left === 1 ? "" : "s"} of its own initiative left`;
  return `${errand.done_when} — ${windows}`;
}

function ErrandRow({ errand, active }: { errand: Errand; active: boolean }) {
  return (
    // The row you are on is `current` — the shared 2px rule on the leading edge,
    // in a neutral — and nothing else. `aria-current` on the link says the same
    // thing to a screen reader, and both stay.
    <Row current={active}>
      <Link
        className="errands-row-link"
        to={`/errands/${errand.id}`}
        aria-current={active ? "page" : undefined}
      >
        <span className="errands-row-name">{errand.name}</span>
        <StateBadge domain="errand" state={errand.status} />
        <span className="errands-row-brain">{errand.brain}</span>
        <span className="errands-row-folder">{errand.folder}</span>
        <span
          className={
            errand.done_when === null
              ? "errands-row-investigation errands-row-investigation-none"
              : "errands-row-investigation"
          }
        >
          {investigationText(errand)}
        </span>
      </Link>
    </Row>
  );
}

/* ----------------------------------------------------------------- detail -- */

function ErrandDetail({ errand }: { errand: Errand }) {
  const close = useCloseErrand();

  return (
    <>
      <Panel
        title="This errand"
        aside={
          errand.status !== "done" ? (
            <ConfirmButton
              label="Close"
              confirmLabel="Close this errand — nothing is deleted"
              variant="quiet"
              intent="stop"
              disabled={close.isPending}
              onConfirm={() => close.mutate(errand.id)}
            />
          ) : undefined
        }
      >
        <div className="errands-facts">
          <StateBadge domain="errand" state={errand.status} />
          <span className="errands-chat-key">topic {errand.chat_key}</span>
          <span className="errands-folder">{errand.folder}</span>
        </div>

        <div className="errands-controls">
          <PauseResumeControl errand={errand} />
          <BrainControl errand={errand} />
        </div>

        <Link className="errands-feed-link" to={`/feed?errand=${errand.id}`}>
          See this errand in the Feed
        </Link>

        {close.isError && <CloseRefusal error={close.error} />}
        {/* A close that already happened elsewhere is not an error — the row
            above already shows `closed`, and this note only explains why the
            button is gone. */}
        {errand.status === "done" && (
          <p className="errands-note" role="status">
            This errand is closed — the asking stopped, and its record and folder are kept.
          </p>
        )}
      </Panel>

      <InvestigationPanel errand={errand} />
      <NotebookPanel errandId={errand.id} />
      <FilesPanel errandId={errand.id} />
      <RulesPanel errandId={errand.id} />
    </>
  );
}

function CloseRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — this errand was not closed</ErrorNote>;
  return <RefusalNote refusal={error} sentences={{ not_found: "this errand is already gone" }} />;
}

function PatchRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — nothing was changed</ErrorNote>;
  return <RefusalNote refusal={error} sentences={{ not_found: "this errand is gone" }} />;
}

/* --------------------------------------------------------- pause / resume -- */

function PauseResumeControl({ errand }: { errand: Errand }) {
  const patch = usePatchErrand();
  if (errand.status === "done") return null;
  const next: ErrandStatus = errand.status === "active" ? "paused" : "active";

  return (
    <div className="errands-pause">
      <Button
        variant={errand.status === "active" ? "ghost" : "approve"}
        disabled={patch.isPending}
        onClick={() => patch.mutate({ id: errand.id, status: next })}
      >
        {errand.status === "active" ? "Pause" : "Resume"}
      </Button>
      {patch.isError && <PatchRefusal error={patch.error} />}
    </div>
  );
}

/* ---------------------------------------------------------------- brain -- */

function BrainControl({ errand }: { errand: Errand }) {
  const patch = usePatchErrand();

  return (
    <div className="errands-brain">
      <Button
        variant={errand.brain === "cloud" ? "approve" : "ghost"}
        aria-pressed={errand.brain === "cloud"}
        disabled={patch.isPending}
        onClick={() => patch.mutate({ id: errand.id, brain: "cloud" })}
      >
        Cloud
      </Button>
      <Button
        variant={errand.brain === "local" ? "approve" : "ghost"}
        aria-pressed={errand.brain === "local"}
        disabled={patch.isPending}
        onClick={() => patch.mutate({ id: errand.id, brain: "local" })}
      >
        Local
      </Button>
      {patch.isError && <PatchRefusal error={patch.error} />}
    </div>
  );
}

/* --------------------------------------------------------- investigation -- */

function InvestigationPanel({ errand }: { errand: Errand }) {
  const [criterion, setCriterion] = useState(errand.done_when ?? "");
  const [windows, setWindows] = useState(String(errand.windows_left));
  const patch = usePatchErrand();
  const parsedWindows = Number.parseInt(windows.trim(), 10);
  const validWindows = Number.isSafeInteger(parsedWindows) && parsedWindows >= 0;
  const validCriterion = criterion.trim() !== "";

  return (
    <Panel title="Investigation">
      <p className="errands-note">
        {errand.done_when === null ? (
          <>
            This errand <strong>answers when spoken to and nothing else</strong> — no criterion is set.
          </>
        ) : (
          <>
            Finishes when: <strong>{errand.done_when}</strong>
          </>
        )}
      </p>
      <p className="errands-note">
        {errand.windows_left === 0
          ? "It may take no turns of its own initiative right now."
          : `It may take ${errand.windows_left} more turn${errand.windows_left === 1 ? "" : "s"} of its own initiative.`}
      </p>

      <form
        className="errands-investigation-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!validCriterion || !validWindows || patch.isPending) return;
          patch.mutate({ id: errand.id, done_when: criterion.trim(), windows: parsedWindows });
        }}
      >
        <label className="errands-field">
          <span>Finishes when</span>
          <textarea
            rows={2}
            aria-label="Finishes when"
            value={criterion}
            onChange={(event) => setCriterion(event.target.value)}
          />
        </label>
        <label className="errands-field errands-field-narrow">
          <span>Turns of its own initiative</span>
          <input
            type="number"
            min={0}
            aria-label="Turns of its own initiative"
            value={windows}
            onChange={(event) => setWindows(event.target.value)}
          />
        </label>
        <Button type="submit" intent="go" disabled={!validCriterion || !validWindows || patch.isPending}>
          Set investigation
        </Button>
      </form>
      {patch.isError && <PatchRefusal error={patch.error} />}
    </Panel>
  );
}

/* ------------------------------------------------------------- notebook -- */

function NotebookPanel({ errandId }: { errandId: number }) {
  const notebook = useErrandNotebook(errandId);

  return (
    <Panel title="Notebook">
      <p className="errands-note">
        caderno.md — this errand&apos;s memory across turns, and somebody else&apos;s writing. Shown
        as plain text, never as markup.
      </p>
      {notebook.isError && <NotebookError error={notebook.error} />}
      {!notebook.isError && notebook.data === undefined && (
        <p className="errands-loading">reading the notebook…</p>
      )}
      {notebook.data !== undefined && notebook.data === "" && (
        <Quiet says="nothing has been written to the notebook yet." />
      )}
      {/* A well and not a box: the notebook is a file the errand wrote, and the
          rung below the panel is what the system calls a recess cut into a
          surface. `as="pre"` keeps the literal text literal — `base.css` gives
          a `pre` `white-space: pre-wrap`, so it wraps rather than scrolling
          sideways, and it never becomes markup. */}
      {notebook.data !== undefined && notebook.data !== "" && (
        <Well as="pre">{notebook.data}</Well>
      )}
    </Panel>
  );
}

function NotebookError({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — nothing is known about the notebook</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        unavailable: "this install has no files root configured",
        internal: "the núcleo could not read the notebook from disk",
      }}
    />
  );
}

/* ----------------------------------------------------------------- files -- */

function FilesPanel({ errandId }: { errandId: number }) {
  const [openPath, setOpenPath] = useState<string | null>(null);
  const files = useErrandFiles(errandId);
  const names = [...(files.data ?? [])].sort((a, b) => a.localeCompare(b));

  return (
    <>
      <Panel title="Files" aside={<Count n={files.data?.length} />}>
        {files.isError && <FilesError error={files.error} />}
        {!files.isError && files.data === undefined && <p className="errands-loading">reading the folder…</p>}
        {files.data !== undefined && names.length === 0 && (
          <Quiet says="this errand's folder is empty." />
        )}
        {names.length > 0 && (
          <ul className="errands-files" aria-label="Files">
            {names.map((name) => (
              <li key={name}>
                <Button variant="link" onClick={() => setOpenPath(name)}>
                  {name}
                </Button>
              </li>
            ))}
          </ul>
        )}
      </Panel>

      {openPath !== null && (
        <FileEditor errandId={errandId} path={openPath} onClose={() => setOpenPath(null)} />
      )}
    </>
  );
}

function FilesError({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — nothing is known about this errand&apos;s files</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        bad_request: "that path was refused",
        not_found: "there is nothing there",
        unavailable: "this install has no files root configured",
        internal: "the núcleo could not read that from disk",
      }}
    />
  );
}

/**
 * One file, as text, with an editor that PUTs it back.
 *
 * Never rendered as markup, and there is no `dangerouslySetInnerHTML` on
 * this page: this is another party's folder, written into by an agent that
 * has been reading the open web.
 */
function FileEditor({
  errandId,
  path,
  onClose,
}: {
  errandId: number;
  path: string;
  onClose: () => void;
}) {
  const file = useErrandFile(errandId, path);
  const write = useWriteErrandFile();
  const [draft, setDraft] = useState<string | null>(null);
  const shown = draft ?? file.data ?? "";
  const dirty = draft !== null && draft !== file.data;

  return (
    <Panel
      title="File"
      aside={
        <Button variant="ghost" onClick={onClose}>
          Close
        </Button>
      }
    >
      <p className="errands-file-path">{path}</p>
      {file.isError && <FilesError error={file.error} />}
      {!file.isError && file.data === undefined && <p className="errands-loading">reading the file…</p>}
      {file.data !== undefined && (
        <>
          <textarea
            className="errands-file-editor"
            aria-label={`Contents of ${path}`}
            rows={12}
            value={shown}
            onChange={(event) => setDraft(event.target.value)}
          />
          <div className="errands-file-actions">
            <Button
              variant="approve"
              disabled={!dirty || write.isPending}
              onClick={() =>
                write.mutate({ id: errandId, path, contents: shown }, { onSuccess: () => setDraft(null) })
              }
            >
              Save
            </Button>
            {dirty && (
              <Button variant="ghost" disabled={write.isPending} onClick={() => setDraft(null)}>
                Revert
              </Button>
            )}
          </div>
          {write.isError && <WriteRefusal error={write.error} />}
        </>
      )}
    </Panel>
  );
}

function WriteRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — this file was not saved</ErrorNote>;
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        bad_request: "that path was refused",
        not_found: "that path is not inside this errand's folder",
        unavailable: "this install has no files root configured",
        internal: "the núcleo could not write that file",
      }}
    />
  );
}

/* ----------------------------------------------------------------- rules -- */

function RulesPanel({ errandId }: { errandId: number }) {
  const rules = useErrandRules(errandId);
  const rows = rules.data ?? [];

  return (
    <Panel title="Rules" aside={<Count n={rules.data?.length} />}>
      <p className="errands-note">
        Standing instructions this errand fires on its own. There is no edit control — the núcleo has
        no route to change a rule once armed, only to arm one and to disarm one.
      </p>
      {rules.isError && <RulesError error={rules.error} />}
      {!rules.isError && rules.data === undefined && <p className="errands-loading">reading the rules…</p>}
      {rules.data !== undefined && rows.length === 0 && <Quiet says="no rule is armed on this errand." />}
      {rows.length > 0 && (
        <Rows label="Rules" className="errands-rules">
          {rows.map((rule) => (
            <RuleRow key={rule.id} errandId={errandId} rule={rule} />
          ))}
        </Rows>
      )}

      <CreateRuleForm errandId={errandId} />
    </Panel>
  );
}

function RulesError({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — nothing is known about this errand&apos;s rules</ErrorNote>;
  }
  return <RefusalNote refusal={error} sentences={{ not_found: "this errand is gone" }} />;
}

function RuleRow({ errandId, rule }: { errandId: number; rule: ErrandRule }) {
  const del = useDeleteErrandRule();

  return (
    <Row className="errands-rule">
      <div className="errands-rule-head">
        <span className="errands-rule-name">{rule.name}</span>
        <code className="errands-rule-cron">{rule.cron}</code>
        <span className="errands-rule-tz">{rule.timezone ?? "UTC"}</span>
        <ConfirmButton
          label="Delete"
          confirmLabel="Delete this rule"
          variant="danger"
          intent="stop"
          disabled={del.isPending}
          onConfirm={() => del.mutate({ id: errandId, ruleId: rule.id })}
        />
      </div>
      <p className="errands-rule-prompt">{rule.prompt}</p>
      <dl className="errands-rule-facts">
        <div className="errands-fact">
          <dt>last fired</dt>
          <dd>
            <RelativeTime at={rule.last_fired_at} />
          </dd>
        </div>
        <div className="errands-fact">
          <dt>today</dt>
          <dd>{rule.fires_today}</dd>
        </div>
      </dl>
      {del.isError && <DeleteRuleRefusal error={del.error} />}
    </Row>
  );
}

function DeleteRuleRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — this rule was not deleted</ErrorNote>;
  return <RefusalNote refusal={error} sentences={{ not_found: "this rule is already gone" }} />;
}

/**
 * Arms a new rule. There is no roster of examples — cron and prompt are free
 * text, and the daemon's own `400` is what explains a bad one, verbatim.
 */
function CreateRuleForm({ errandId }: { errandId: number }) {
  const [name, setName] = useState("");
  const [cron, setCron] = useState("");
  const [prompt, setPrompt] = useState("");
  const [timezone, setTimezone] = useState("");
  const create = useCreateErrandRule();
  const valid = name.trim() !== "" && cron.trim() !== "" && prompt.trim() !== "";

  return (
    <form
      className="errands-rule-form"
      onSubmit={(event) => {
        event.preventDefault();
        if (!valid || create.isPending) return;
        create.mutate(
          {
            id: errandId,
            name: name.trim(),
            cron: cron.trim(),
            prompt: prompt.trim(),
            timezone: timezone.trim() === "" ? undefined : timezone.trim(),
          },
          {
            onSuccess: () => {
              setName("");
              setCron("");
              setPrompt("");
              setTimezone("");
            },
          },
        );
      }}
    >
      <label className="errands-field">
        <span>Name</span>
        <input aria-label="Rule name" value={name} onChange={(event) => setName(event.target.value)} />
      </label>
      <label className="errands-field">
        <span>Cron</span>
        <input aria-label="Cron" value={cron} onChange={(event) => setCron(event.target.value)} />
      </label>
      <label className="errands-field">
        <span>Timezone (optional — UTC if blank)</span>
        <input aria-label="Timezone" value={timezone} onChange={(event) => setTimezone(event.target.value)} />
      </label>
      <label className="errands-field errands-field-wide">
        <span>Prompt</span>
        <textarea rows={2} aria-label="Prompt" value={prompt} onChange={(event) => setPrompt(event.target.value)} />
      </label>
      <Button type="submit" intent="go" disabled={!valid || create.isPending}>
        Arm rule
      </Button>
      {create.isError && <CreateRuleRefusal error={create.error} />}
    </form>
  );
}

/**
 * Why a rule was refused.
 *
 * `400` carries `{"error": "<reason>"}` — free text naming the word that was
 * wrong — and `daemonProse` is what lets that sentence through rather than
 * the generic "the núcleo would not accept that request" floor. `409`'s body
 * is `{}`, no sentence at all, so the page supplies its own instead of
 * showing the daemon's empty JSON back.
 */
function CreateRuleRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — no rule was armed</ErrorNote>;
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        conflict: "this errand already has a rule with that name",
        not_found: "this errand is gone",
        ...daemonProse(error),
      }}
    />
  );
}
