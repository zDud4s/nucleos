import type { ReactNode } from "react";
import { useModelGroups } from "../data/chats";
import { useMachineSecrets, type MachineSecret } from "../data/secrets";
import { enumOptions, type SchemaNode } from "./schema";

/**
 * The special controls a schema can ask for with `x-widget`. Each one draws a STRING field; the value it
 * hands back is the string that goes into the document, so the núcleo's own validation stays the only
 * judge of what is acceptable. A widget that helps (a cron hint) says so beside the box and never blocks it.
 */
export interface WidgetProps {
  id?: string;
  "aria-describedby"?: string;
  "aria-invalid"?: boolean;
  value: string;
  onChange: (value: string) => void;
  /** The field is optional: an empty choice is offered. */
  nullable: boolean;
}

export interface Widget {
  accepts: (node: SchemaNode) => boolean;
  Control: (props: WidgetProps) => ReactNode;
}

/** The props that belong on the DOM control; `nullable` is the widget's own business. */
function domProps(props: WidgetProps) {
  const dom: Pick<WidgetProps, "id" | "aria-describedby" | "aria-invalid"> = {};
  if (props.id !== undefined) dom.id = props.id;
  if (props["aria-describedby"] !== undefined) dom["aria-describedby"] = props["aria-describedby"];
  if (props["aria-invalid"] !== undefined) dom["aria-invalid"] = props["aria-invalid"];
  return dom;
}

/* ------------------------------------------------------------- the pickers -- */

function ModelPicker(props: WidgetProps) {
  const { value, onChange, nullable } = props;
  const groups = useModelGroups();
  const listed = new Set<string>();
  for (const group of groups.data?.groups ?? []) {
    for (const model of group.models) listed.add(model.id);
  }
  return (
    <select
      className="sy-field-input"
      value={value}
      onChange={(event) => { onChange(event.target.value); }}
      {...domProps(props)}
    >
      {nullable || value === "" ? <option value="">{nullable ? "(none)" : "Choose a model"}</option> : null}
      {value !== "" && !listed.has(value) ? <option value={value}>{value}</option> : null}
      {(groups.data?.groups ?? []).map((group) => (
        <optgroup key={`${group.provider}/${group.family}`} label={group.label}>
          {group.models.map((model) => (
            <option key={model.id} value={model.id}>
              {model.label}
            </option>
          ))}
        </optgroup>
      ))}
    </select>
  );
}

/**
 * What the keyring said about a name. `present: null` means the store did not answer, and it must never
 * read as "not stored": that sentence is what sends somebody to paste a credential they already pasted.
 */
function keyringNote(known: MachineSecret | undefined): string {
  if (known === undefined || known.present === false) return "Nothing is stored under this name yet.";
  return known.present === true ? "stored in the keyring" : "Could not check the keyring for this name.";
}

function SecretRef(props: WidgetProps) {
  const { value, onChange, nullable } = props;
  const secrets = useMachineSecrets();
  const rows = secrets.data?.secrets ?? [];
  const known = rows.find((row) => row.key === value);
  return (
    <>
      <select
        className="sy-field-input"
        value={value}
        onChange={(event) => { onChange(event.target.value); }}
        {...domProps(props)}
      >
        {nullable || value === "" ? <option value="">{nullable ? "(none)" : "Choose a credential"}</option> : null}
        {value !== "" && known === undefined ? <option value={value}>{value}</option> : null}
        {rows.map((row) => (
          <option key={row.key} value={row.key}>
            {row.key}
          </option>
        ))}
      </select>
      {value === "" || secrets.data === undefined ? null : <span className="sy-meta">{keyringNote(known)}</span>}
    </>
  );
}

/* ------------------------------------------------------------- text boxes -- */

const CRON_ITEM = "(\\*|[0-9A-Za-z]+(-[0-9A-Za-z]+)?)(/[0-9]+)?";
const CRON_FIELD = new RegExp(`^${CRON_ITEM}(,${CRON_ITEM})*$`);

/**
 * A readable complaint about a cron expression, or null. It checks the SHAPE (five fields, each a list of
 * values, ranges and steps) and nothing about what a value means: the núcleo's scheduler is the judge.
 */
export function cronProblem(text: string): string | null {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  const fields = trimmed.split(/\s+/);
  if (fields.length !== 5) {
    return `Expected five fields (minute hour day-of-month month weekday), found ${fields.length}.`;
  }
  const bad = fields.find((field) => !CRON_FIELD.test(field));
  return bad === undefined ? null : `"${bad}" is not a valid cron field.`;
}

function CronBox(props: WidgetProps) {
  const problem = cronProblem(props.value);
  const dom = domProps(props);
  if (problem !== null) dom["aria-invalid"] = true;
  return (
    <>
      <input
        type="text"
        className="sy-field-input"
        value={props.value}
        spellCheck={false}
        autoComplete="off"
        onChange={(event) => { props.onChange(event.target.value); }}
        {...dom}
      />
      {problem === null ? null : <span className="sy-meta">{problem}</span>}
    </>
  );
}

function CommandBox(props: WidgetProps) {
  return (
    <input
      type="text"
      className="sy-field-input font-mono"
      value={props.value}
      spellCheck={false}
      autoComplete="off"
      onChange={(event) => { props.onChange(event.target.value); }}
      {...domProps(props)}
    />
  );
}

/* --------------------------------------------------------------- registry -- */

const isString = (node: SchemaNode) => node.type === "string" && enumOptions(node) === null;

const WIDGETS = new Map<string, Widget>([
  ["model-picker", { accepts: isString, Control: ModelPicker }],
  ["cron", { accepts: isString, Control: CronBox }],
  ["command", { accepts: isString, Control: CommandBox }],
  ["secret-ref", { accepts: isString, Control: SecretRef }],
]);

export function widgetNamed(name: string): Widget | undefined {
  return WIDGETS.get(name);
}

/** The widget a (resolved, unwrapped) node asks for, if it names one that fits; otherwise null. */
export function widgetFor(node: SchemaNode): Widget | null {
  const name = node["x-widget"];
  if (name === undefined) return null;
  const widget = WIDGETS.get(name);
  return widget !== undefined && widget.accepts(node) ? widget : null;
}
