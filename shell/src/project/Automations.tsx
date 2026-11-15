import { useState } from "react";
import { classifyWriteFailure, useSettingDoc, useSettingsIndex, useWriteSetting } from "../data/settings";
import { Button, ConfirmButton, ConflictNote, Switch } from "../ui";
import { RuleForm } from "./RuleForm";
import { blankDraft, draftOf, readRules, ruleOf, summarize, type Draft, type Rule } from "./automations-model";

const NAME = "automations";

/**
 * The project's automations, edited with controls and never as JSON.
 *
 * Every change (save a rule, switch one on or off, delete one) is a `PUT` of the whole
 * `{rules: [...]}` document at the version that was read. The núcleo stays the authority on what is
 * valid and answers 422 with a sentence, which is shown as it came.
 */
export function Automations({ projectId }: { projectId: string }) {
  const scope = `project:${projectId}`;
  const index = useSettingsIndex(scope);

  if (index.isPending) return <p className="text-sm text-text-faint">Reading this document…</p>;

  const listed = index.data?.some((row) => row.scope === scope && row.name === NAME);
  if (index.data !== undefined && listed !== true) {
    return <p className="text-sm text-text-muted">This project has no {NAME} document to edit.</p>;
  }
  return <Editor scope={scope} />;
}

interface Form {
  /** The index of the rule being edited, or null for a new one. */
  at: number | null;
  draft: Draft;
}

type Notice = { kind: "stale" } | { kind: "failed"; where: "form" | "list"; text: string };

function Editor({ scope }: { scope: string }) {
  const doc = useSettingDoc(scope, NAME);
  const write = useWriteSetting(scope, NAME);
  const [form, setForm] = useState<Form | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);

  if (doc.isPending) return <p className="text-sm text-text-faint">Reading this document…</p>;
  if (doc.data === undefined) {
    return <p className="text-sm text-text-muted">The automations could not be read. Reopening the page asks again.</p>;
  }

  const rules = readRules(doc.data.body);
  const version = doc.data.version;

  function put(next: Rule[], where: "form" | "list", onDone?: () => void) {
    setNotice(null);
    write.mutate(
      { body: { rules: next }, version },
      {
        onSuccess: () => onDone?.(),
        onError: (error) => {
          const failure = classifyWriteFailure(error);
          if (failure.kind === "stale") {
            // The query is already invalidated and reloading; the open form is dropped with the old list.
            setForm(null);
            setNotice({ kind: "stale" });
          } else {
            setNotice({ kind: "failed", where, text: sentenceOf(failure) });
          }
        },
      },
    );
  }

  function save() {
    if (form === null) return;
    const rule = ruleOf(form.draft, form.at === null ? false : rules[form.at].enabled);
    const next = form.at === null ? [...rules, rule] : rules.map((old, i) => (i === form.at ? rule : old));
    put(next, "form", () => setForm(null));
  }

  const failureAt = (where: "form" | "list") =>
    notice?.kind === "failed" && notice.where === where ? (
      <p role="alert" className="ui-note ui-note-error">
        {notice.text}
      </p>
    ) : null;

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm text-text-muted">A new rule starts switched off.</p>

      {notice?.kind === "stale" ? (
        <ConflictNote>Someone else changed the automations. Showing the latest version — try again.</ConflictNote>
      ) : null}

      {rules.length === 0 && form === null ? (
        <p className="text-sm text-text-muted">
          No automations yet. Nothing here acts on its own until you add a rule and switch it on.
        </p>
      ) : (
        <ul className="flex flex-col gap-2">
          {rules.map((rule, at) => (
            <li key={`${at}-${rule.name}`} className="flex flex-wrap items-center gap-x-4 gap-y-1">
              <span className="font-medium">{rule.name}</span>
              <Switch
                label={`Enable ${rule.name}`}
                labelHidden
                checked={rule.enabled}
                disabled={write.isPending}
                onChange={(enabled) =>
                  put(rules.map((old, i) => (i === at ? { ...old, enabled } : old)), "list")
                }
              />
              <span className="text-sm text-text-muted">{summarize(rule)}</span>
              <span className="flex items-center gap-2">
                <Button
                  variant="quiet"
                  disabled={write.isPending}
                  onClick={() => {
                    setNotice(null);
                    setForm({ at, draft: draftOf(rule) });
                  }}
                >
                  {`Edit ${rule.name}`}
                </Button>
                <ConfirmButton
                  variant="quiet"
                  label={`Delete ${rule.name}`}
                  confirmLabel={`Delete ${rule.name}? This cannot be undone.`}
                  disabled={write.isPending}
                  onConfirm={() =>
                    put(
                      rules.filter((_, i) => i !== at),
                      "list",
                      () => setForm(null),
                    )
                  }
                />
              </span>
            </li>
          ))}
        </ul>
      )}

      {form === null ? (
        <div>
          <Button
            variant="ghost"
            onClick={() => {
              setNotice(null);
              setForm({ at: null, draft: blankDraft() });
            }}
          >
            Add automation
          </Button>
          {failureAt("list")}
        </div>
      ) : (
        <>
          {failureAt("list")}
          <RuleForm
            draft={form.draft}
            onChange={(draft) => setForm({ ...form, draft })}
            onSave={save}
            onCancel={() => {
              setNotice(null);
              setForm(null);
            }}
            busy={write.isPending}
            failure={failureAt("form")}
          />
        </>
      )}
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
      return "Someone else changed the automations.";
  }
}
