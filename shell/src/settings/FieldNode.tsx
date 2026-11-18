import { useState, type ReactElement, type ReactNode } from "react";
import { ChannelCardBox, ChannelCards } from "../channels/ChannelDetail";
import { Button, ErrorNote, Field, Select, Switch, Tip, type FieldProps } from "../ui";
import {
  defaultFor,
  enumOptions,
  fieldLabel,
  has,
  isRecord,
  nodeKind,
  pointerJoin,
  unwrapNullable,
  type SchemaNode,
} from "./schema";
import { widgetFor } from "./widgets";
import "./rows.css";

/**
 * Draws one schema node and recurses into its children.
 *
 * Controlled and save-agnostic: `value` in, `onChange` out, where `undefined` means "remove this key" (an
 * emptied optional box is never sent as `null` or `""`). Keys the schema does not declare are carried
 * through untouched, so a newer núcleo's extra field survives an edit made by an older shell.
 *
 * `errors` maps a JSON pointer to the sentence the núcleo gave for it; a node shows the one at its own
 * pointer beside its control. Nothing here validates: the núcleo is the only judge.
 */
export interface FieldNodeProps {
  node: SchemaNode;
  root: SchemaNode;
  value: unknown;
  onChange: (value: unknown) => void;
  /** This node's JSON pointer; `""` for the document. */
  path: string;
  label: string;
  errors: Record<string, string>;
  required?: boolean;
  /** The document root: its properties are drawn with no fieldset around them. */
  bare?: boolean;
  depth?: number;
  /** JSON pointers of fields not drawn. Their values stay in the body, so a save writes them back unchanged. */
  hide?: readonly string[];
  /**
   * `"stack"` (default): each field is a label over a full-width input. `"rows"`: each field is one compact
   * row, label on the left and control on the right, and the document root is drawn as a grid of cards.
   */
  layout?: FieldLayout;
  /** Rows layout only: how the root's fields split into cards (see `SettingsGroup`). */
  groups?: readonly SettingsGroup[];
}

export type FieldLayout = "stack" | "rows";

/**
 * One card of a rows-layout document. `fields` are top-level property names, in the order drawn. A property
 * no group names joins the LAST card, so an edit never loses a field it did not know to place; names the
 * schema does not have are skipped, and a card with nothing in it is not drawn. `extra` is drawn as a last
 * row of the card (the e-mail password, which is not part of the document).
 *
 * Without any group, a root of more than four fields is split into two untitled halves, otherwise one card.
 */
export interface SettingsGroup {
  title: string;
  fields: readonly string[];
  extra?: ReactNode;
}

interface CardPlan {
  title: string | undefined;
  nodes: ReactNode[];
  extra: ReactNode;
}

function planCards(keys: readonly string[], nodes: Map<string, ReactNode>, groups: readonly SettingsGroup[] | undefined): CardPlan[] {
  if (groups === undefined || groups.length === 0) {
    const split = keys.length > 4 ? Math.ceil(keys.length / 2) : keys.length;
    const halves = split < keys.length ? [keys.slice(0, split), keys.slice(split)] : [keys];
    return halves.map((half) => ({ title: undefined, nodes: half.map((key) => nodes.get(key)), extra: null }));
  }
  const placed = new Set<string>();
  const plans: CardPlan[] = groups.map((group) => {
    const mine = group.fields.filter((key) => nodes.has(key) && !placed.has(key));
    mine.forEach((key) => placed.add(key));
    return { title: group.title, nodes: mine.map((key) => nodes.get(key)), extra: group.extra ?? null };
  });
  const rest = keys.filter((key) => !placed.has(key));
  if (rest.length > 0) plans[plans.length - 1].nodes.push(...rest.map((key) => nodes.get(key)));
  return plans.filter((plan) => plan.nodes.length > 0 || plan.extra !== null);
}

/** A schema that refers to itself would otherwise draw forever; past this depth a node is a JSON box. */
const MAX_FIELD_DEPTH = 8;

function withKey(record: Record<string, unknown>, key: string, next: unknown): Record<string, unknown> {
  const copy: Record<string, unknown> = { ...record };
  if (next === undefined) delete copy[key];
  else copy[key] = next;
  return copy;
}

function RawNode(props: {
  label: string;
  helper: ReactNode;
  value: unknown;
  invalid: boolean | undefined;
  onChange: (value: unknown) => void;
}) {
  const [text, setText] = useState(() =>
    props.value === undefined ? "" : JSON.stringify(props.value, null, 2),
  );
  const [bad, setBad] = useState(false);
  return (
    <>
      <Field label={`${props.label} (JSON)`} helper={props.helper}>
        <textarea
          className="sy-field-input font-mono"
          rows={4}
          spellCheck={false}
          value={text}
          aria-invalid={bad || props.invalid === true ? true : undefined}
          onChange={(event) => {
            const next = event.target.value;
            setText(next);
            if (next.trim() === "") {
              setBad(false);
              props.onChange(undefined);
              return;
            }
            try {
              const parsed: unknown = JSON.parse(next);
              setBad(false);
              props.onChange(parsed);
            } catch {
              setBad(true);
            }
          }}
        />
      </Field>
      {bad ? <span className="sy-meta">Not valid JSON</span> : null}
    </>
  );
}

function MapAdder(props: { label: string; exists: (key: string) => boolean; onAdd: (key: string) => void }) {
  const [key, setKey] = useState("");
  return (
    <div className="flex items-end gap-2">
      <Field label={`New key for ${props.label}`}>
        <input
          type="text"
          className="sy-field-input"
          value={key}
          onChange={(event) => { setKey(event.target.value); }}
        />
      </Field>
      <Button
        variant="quiet"
        aria-label={`Add entry to ${props.label}`}
        disabled={key.trim() === "" || props.exists(key.trim())}
        onClick={() => {
          props.onAdd(key.trim());
          setKey("");
        }}
      >
        Add
      </Button>
    </div>
  );
}

/** A schema description is help, so it opens on demand; `persist` keeps its text in the document. */
function aboutTip(description: string | undefined): ReactElement | undefined {
  return description === undefined ? undefined : <Tip tip={description} name="About" persist />;
}

export function FieldNode(props: FieldNodeProps): ReactElement {
  const { node, root, value, onChange, path, label, errors, required = false, bare = false, depth = 0, hide } = props;
  const { layout = "stack", groups } = props;
  const rows = layout === "rows";
  const { node: inner, nullable } = unwrapNullable(node, root);
  const optional = nullable || !required;
  const kind = depth > MAX_FIELD_DEPTH ? "raw" : nodeKind(inner);
  const message = errors[path];
  const invalid = message !== undefined ? true : undefined;
  const failure = message !== undefined ? <ErrorNote>{message}</ErrorNote> : null;

  const leaf = (control: FieldProps["children"]): ReactElement => (
    <div data-field={path} className={rows ? "set-row" : "flex flex-col gap-1"}>
      <Field label={label} helper={aboutTip(inner.description)}>
        {control}
      </Field>
      {failure}
    </div>
  );

  const group = (children: ReactNode): ReactElement => (
    <div data-field={path}>
      <fieldset className="flex flex-col gap-3 rounded-lg border border-border-strong p-3">
        <legend className="px-1 text-sm">
          {label}
          {aboutTip(inner.description)}
        </legend>
        {children}
      </fieldset>
      {failure}
    </div>
  );

  const child = (
    schema: SchemaNode,
    key: string | number,
    childValue: unknown,
    childLabel: string,
    setChild: (next: unknown) => void,
    childRequired: boolean,
  ): ReactElement => (
    <FieldNode
      key={String(key)}
      node={schema}
      root={root}
      value={childValue}
      onChange={setChild}
      path={pointerJoin(path, key)}
      label={childLabel}
      errors={errors}
      required={childRequired}
      depth={depth + 1}
      hide={hide}
      layout={layout}
    />
  );

  switch (kind) {
    case "object": {
      const properties = inner.properties ?? {};
      const record = isRecord(value) ? value : {};
      const requiredKeys = inner.required ?? [];
      const drawn = Object.keys(properties).filter((key) => !(hide?.includes(pointerJoin(path, key)) ?? false));
      const fields = drawn.map((key) =>
        child(
          properties[key],
          key,
          record[key],
          fieldLabel(key),
          (next) => { onChange(withKey(record, key, next)); },
          requiredKeys.includes(key),
        ),
      );
      if (bare && rows) {
        const byKey = new Map<string, ReactNode>(drawn.map((key, at) => [key, fields[at]]));
        const plans = planCards(drawn, byKey, groups);
        // The `extra` of a group is drawn even when the schema has no field for it.
        return (
          <ChannelCards className="set-cards">
            {plans.map((plan, at) => (
              <ChannelCardBox key={plan.title ?? at} title={plan.title}>
                <div className="set-rows">
                  {plan.nodes}
                  {plan.extra}
                </div>
              </ChannelCardBox>
            ))}
          </ChannelCards>
        );
      }
      if (bare) {
        return (
          <div data-field={path} className="flex flex-col gap-3">
            {fields}
          </div>
        );
      }
      return group(fields);
    }

    case "map": {
      const record = isRecord(value) ? value : {};
      const valueSchema = inner.additionalProperties as SchemaNode;
      return group(
        <>
          {Object.keys(record).map((key) => (
            <div key={key} className="flex items-end gap-2">
              <div className="grow">
                {child(valueSchema, key, record[key], key, (next) => { onChange(withKey(record, key, next)); }, true)}
              </div>
              <Button
                variant="quiet"
                aria-label={`Remove ${key}`}
                onClick={() => { onChange(withKey(record, key, undefined)); }}
              >
                Remove
              </Button>
            </div>
          ))}
          <MapAdder
            label={label}
            exists={(key) => has(record, key)}
            onAdd={(key) => { onChange(withKey(record, key, defaultFor(valueSchema, root))); }}
          />
        </>,
      );
    }

    case "array": {
      const items = Array.isArray(value) ? (value as unknown[]) : [];
      const itemSchema = inner.items as SchemaNode;
      return group(
        <>
          {items.map((item, index) => (
            <div key={index} className="flex items-end gap-2">
              <div className="grow">
                {child(
                  itemSchema,
                  index,
                  item,
                  `${label} ${index + 1}`,
                  (next) => { onChange(items.map((old, at) => (at === index ? next : old))); },
                  true,
                )}
              </div>
              <Button
                variant="quiet"
                aria-label={`Remove ${label} ${index + 1}`}
                onClick={() => { onChange(items.filter((_, at) => at !== index)); }}
              >
                Remove
              </Button>
            </div>
          ))}
          <div>
            <Button
              variant="quiet"
              aria-label={`Add ${label}`}
              onClick={() => { onChange([...items, defaultFor(itemSchema, root)]); }}
            >
              Add
            </Button>
          </div>
        </>,
      );
    }

    case "string": {
      const text = typeof value === "string" ? value : "";
      const emit = (next: string) => { onChange(next === "" && optional ? undefined : next); };
      const widget = widgetFor(inner);
      if (widget !== null) {
        const Control = widget.Control;
        return leaf(<Control value={text} onChange={emit} nullable={optional} aria-invalid={invalid} />);
      }
      return leaf(
        <input
          type="text"
          className="sy-field-input"
          value={text}
          aria-invalid={invalid}
          onChange={(event) => { emit(event.target.value); }}
        />,
      );
    }

    case "integer":
    case "number": {
      return leaf(
        <input
          type="number"
          className="sy-field-input"
          min={inner.minimum}
          max={inner.maximum}
          value={typeof value === "number" ? String(value) : ""}
          aria-invalid={invalid}
          onChange={(event) => {
            const text = event.target.value.trim();
            if (text === "") {
              onChange(undefined);
              return;
            }
            const parsed = Number(text);
            if (Number.isNaN(parsed)) return;
            onChange(kind === "integer" ? Math.trunc(parsed) : parsed);
          }}
        />,
      );
    }

    case "boolean": {
      return (
        <div data-field={path} className={rows ? "set-row set-row-switch" : "flex flex-col gap-1"}>
          <div className="flex items-center gap-2">
            <Switch
              label={label}
              checked={value === undefined || value === null ? inner.default === true : value === true}
              onChange={(next) => { onChange(next); }}
            />
            {aboutTip(inner.description)}
          </div>
          {failure}
        </div>
      );
    }

    case "enum": {
      const options = enumOptions(inner) ?? [];
      const text = typeof value === "string" ? value : "";
      return leaf(
        <Select
          value={text}
          aria-invalid={invalid}
          onChange={(event) => {
            const next = event.target.value;
            onChange(next === "" && optional ? undefined : next);
          }}
        >
          {optional || text === "" ? <option value=""></option> : null}
          {text !== "" && !options.includes(text) ? <option value={text}>{text}</option> : null}
          {options.map((option) => (
            <option key={option} value={option}>
              {option}
            </option>
          ))}
        </Select>,
      );
    }

    case "raw":
      return (
        <div data-field={path} className="flex flex-col gap-1">
          <RawNode label={label} helper={aboutTip(inner.description)} value={value} invalid={invalid} onChange={onChange} />
          {failure}
        </div>
      );
  }
}
