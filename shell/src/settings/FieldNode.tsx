import { useState, type ReactElement, type ReactNode } from "react";
import { Button, ErrorNote, Field, Select, type FieldProps } from "../ui";
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
  helper: string | undefined;
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

export function FieldNode(props: FieldNodeProps): ReactElement {
  const { node, root, value, onChange, path, label, errors, required = false, bare = false, depth = 0 } = props;
  const { node: inner, nullable } = unwrapNullable(node, root);
  const optional = nullable || !required;
  const kind = depth > MAX_FIELD_DEPTH ? "raw" : nodeKind(inner);
  const message = errors[path];
  const invalid = message !== undefined ? true : undefined;
  const failure = message !== undefined ? <ErrorNote>{message}</ErrorNote> : null;

  const leaf = (control: FieldProps["children"]): ReactElement => (
    <div data-field={path} className="flex flex-col gap-1">
      <Field label={label} helper={inner.description}>
        {control}
      </Field>
      {failure}
    </div>
  );

  const group = (children: ReactNode): ReactElement => (
    <div data-field={path}>
      <fieldset className="flex flex-col gap-3 rounded-lg border border-border-strong p-3">
        <legend className="px-1 text-sm">{label}</legend>
        {inner.description !== undefined ? <p className="sy-note">{inner.description}</p> : null}
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
    />
  );

  switch (kind) {
    case "object": {
      const properties = inner.properties ?? {};
      const record = isRecord(value) ? value : {};
      const requiredKeys = inner.required ?? [];
      const fields = Object.keys(properties).map((key) =>
        child(
          properties[key],
          key,
          record[key],
          fieldLabel(key),
          (next) => { onChange(withKey(record, key, next)); },
          requiredKeys.includes(key),
        ),
      );
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
        <div data-field={path} className="flex flex-col gap-1">
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={value === true}
              aria-invalid={invalid}
              onChange={(event) => { onChange(event.target.checked); }}
            />
            <span>{label}</span>
          </label>
          {inner.description !== undefined ? <p className="sy-note">{inner.description}</p> : null}
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
          <RawNode label={label} helper={inner.description} value={value} invalid={invalid} onChange={onChange} />
          {failure}
        </div>
      );
  }
}
