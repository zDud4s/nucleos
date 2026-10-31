/**
 * The small slice of JSON Schema 2020-12 that schemars emits for the núcleo's settings structs, and the
 * pure helpers the form is built from. No React and no imports on purpose: the awkward part of a
 * schema-driven form is resolving `$ref`s, spotting `Option<T>` and doing JSON-pointer arithmetic, and
 * none of that needs a DOM to be tested.
 *
 * Anything this file cannot classify is `raw`: the form draws a JSON textarea for that node instead of
 * failing. A new schema shape therefore costs a worse control, never a blank screen.
 */

export interface SchemaNode {
  $ref?: string;
  $defs?: Record<string, SchemaNode>;
  type?: string | string[];
  title?: string;
  description?: string;
  default?: unknown;
  properties?: Record<string, SchemaNode>;
  required?: string[];
  additionalProperties?: boolean | SchemaNode;
  items?: SchemaNode;
  prefixItems?: SchemaNode[];
  enum?: unknown[];
  const?: unknown;
  oneOf?: SchemaNode[];
  anyOf?: SchemaNode[];
  allOf?: SchemaNode[];
  minimum?: number;
  maximum?: number;
  format?: string;
  /** Which special control draws this field: `model-picker`, `cron`, `command` or `secret-ref`. */
  "x-widget"?: string;
}

export type NodeKind =
  | "object"
  | "map"
  | "array"
  | "string"
  | "integer"
  | "number"
  | "boolean"
  | "enum"
  | "raw";

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Own-property test; `"constructor" in {}` is true, and a schema key can be anything. */
export function has(object: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(object, key);
}

const MAX_DEPTH = 16;
const DEFS_PREFIX = "#/$defs/";

/**
 * Follows a `$ref` into the root's `$defs` and merges the node's own keys over the target, so a
 * `description` written beside the `$ref` wins over the one on the definition. A single-element
 * `allOf` is unwrapped the same way. A cycle ends at {@link MAX_DEPTH} with an empty node.
 */
export function resolve(node: SchemaNode, root: SchemaNode, depth = 0): SchemaNode {
  if (depth > MAX_DEPTH) return {};
  if (node.$ref !== undefined) {
    const siblings: SchemaNode = { ...node };
    delete siblings.$ref;
    if (node.$ref.startsWith(DEFS_PREFIX)) {
      const name = node.$ref.slice(DEFS_PREFIX.length);
      const target = root.$defs !== undefined && has(root.$defs, name) ? root.$defs[name] : undefined;
      if (target !== undefined) return { ...resolve(target, root, depth + 1), ...siblings };
    }
    return siblings;
  }
  if (node.allOf !== undefined && node.allOf.length === 1) {
    const siblings: SchemaNode = { ...node };
    delete siblings.allOf;
    return { ...resolve(node.allOf[0], root, depth + 1), ...siblings };
  }
  return node;
}

/** The string choices of an enum, from `enum: [...]` or from `oneOf`/`anyOf` of string `const`s. */
export function enumOptions(node: SchemaNode): string[] | null {
  if (node.enum !== undefined) {
    const strings = node.enum.filter((value): value is string => typeof value === "string");
    return strings.length > 0 ? strings : null;
  }
  const variants = node.oneOf ?? node.anyOf;
  if (
    variants !== undefined &&
    variants.length > 0 &&
    variants.every((variant) => typeof variant.const === "string")
  ) {
    return variants.map((variant) => variant.const as string);
  }
  return null;
}

/**
 * Resolves a node and strips `Option<T>`: `type: [X, "null"]`, or an `anyOf`/`oneOf` with exactly one
 * non-null variant. The description and `x-widget` written on the optional wrapper are carried onto the
 * inner node. A nullable field is one the form may leave unset.
 */
export function unwrapNullable(
  node: SchemaNode,
  root: SchemaNode,
): { node: SchemaNode; nullable: boolean } {
  const base = resolve(node, root);
  if (Array.isArray(base.type)) {
    const rest = base.type.filter((type) => type !== "null");
    const nullable = rest.length !== base.type.length;
    if (rest.length === 1) return { node: { ...base, type: rest[0] }, nullable };
    return { node: base, nullable };
  }
  const variants = base.anyOf ?? base.oneOf;
  if (variants !== undefined && enumOptions(base) === null) {
    const resolved = variants.map((variant) => resolve(variant, root));
    const nonNull = resolved.filter((variant) => variant.type !== "null");
    if (resolved.length > 1 && nonNull.length === 1) {
      const inner = nonNull[0];
      const merged: SchemaNode = { ...inner };
      const description = base.description ?? inner.description;
      if (description !== undefined) merged.description = description;
      const widget = base["x-widget"] ?? inner["x-widget"];
      if (widget !== undefined) merged["x-widget"] = widget;
      return { node: merged, nullable: true };
    }
  }
  return { node: base, nullable: false };
}

/** What the form draws for an already-resolved, already-unwrapped node. */
export function nodeKind(node: SchemaNode): NodeKind {
  if (enumOptions(node) !== null) return "enum";
  const type = node.type;
  if (typeof type === "string") {
    switch (type) {
      case "object":
        if (node.properties !== undefined) return "object";
        if (typeof node.additionalProperties === "object") return "map";
        return "raw";
      case "array":
        return node.items !== undefined && node.prefixItems === undefined ? "array" : "raw";
      case "string":
      case "integer":
      case "number":
      case "boolean":
        return type;
      default:
        return "raw";
    }
  }
  if (type === undefined && node.properties !== undefined) return "object";
  return "raw";
}

/* --------------------------------------------------------------- pointers -- */

/** RFC 6901: `~` is `~0` and `/` is `~1`, in that order. */
export function pointerJoin(base: string, key: string | number): string {
  const segment = String(key).replace(/~/g, "~0").replace(/\//g, "~1");
  return `${base}/${segment}`;
}

export function pointerSegments(pointer: string): string[] {
  if (pointer === "") return [];
  return pointer
    .slice(1)
    .split("/")
    .map((segment) => segment.replace(/~1/g, "/").replace(/~0/g, "~"));
}

/**
 * Whether the form draws a control at `pointer`, so a 422 can be shown on the field it names. Walks the
 * schema and the body together: a declared property counts even when the body lacks it, a map key or an
 * array index only when the body has it. The empty pointer, a pointer without a leading `/` and a pointer
 * into a `raw` node's inside are all "no".
 */
export function hasField(root: SchemaNode, body: unknown, pointer: string): boolean {
  if (!pointer.startsWith("/")) return false;
  let node = unwrapNullable(root, root).node;
  let value: unknown = body;
  for (const segment of pointerSegments(pointer)) {
    const kind = nodeKind(node);
    if (kind === "object") {
      const properties = node.properties ?? {};
      if (!has(properties, segment)) return false;
      node = unwrapNullable(properties[segment], root).node;
      value = isRecord(value) ? value[segment] : undefined;
    } else if (kind === "map") {
      if (!isRecord(value) || !has(value, segment)) return false;
      node = unwrapNullable(node.additionalProperties as SchemaNode, root).node;
      value = value[segment];
    } else if (kind === "array") {
      const index = Number(segment);
      if (!Array.isArray(value) || !Number.isInteger(index) || index < 0 || index >= value.length) {
        return false;
      }
      node = unwrapNullable(node.items as SchemaNode, root).node;
      value = value[index];
    } else {
      return false;
    }
  }
  return true;
}

/* ---------------------------------------------------------------- values -- */

/** The smallest value the schema accepts: required fields only, `[]`, `{}`, `""`, `false`, the first choice. */
export function defaultFor(node: SchemaNode, root: SchemaNode, depth = 0): unknown {
  if (depth > 8) return null;
  const { node: inner } = unwrapNullable(node, root);
  if (inner.default !== undefined) return inner.default;
  switch (nodeKind(inner)) {
    case "object": {
      const out: Record<string, unknown> = {};
      const properties = inner.properties ?? {};
      for (const key of inner.required ?? []) {
        if (has(properties, key)) out[key] = defaultFor(properties[key], root, depth + 1);
      }
      return out;
    }
    case "map":
      return {};
    case "array":
      return [];
    case "string":
      return "";
    case "integer":
    case "number":
      return inner.minimum ?? 0;
    case "boolean":
      return false;
    case "enum":
      return (enumOptions(inner) ?? [""])[0];
    case "raw":
      return null;
  }
}

/** `gate_command` becomes "Gate command". */
export function fieldLabel(key: string): string {
  const spaced = key.replace(/_/g, " ").trim();
  if (spaced === "") return key;
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}
