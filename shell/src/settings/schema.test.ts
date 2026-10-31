import { describe, expect, it } from "vitest";
import {
  defaultFor,
  enumOptions,
  fieldLabel,
  hasField,
  nodeKind,
  pointerJoin,
  pointerSegments,
  resolve,
  unwrapNullable,
  type SchemaNode,
} from "./schema";

const ROOT: SchemaNode = {
  type: "object",
  properties: {
    hotkey: { type: "string", description: "Push to talk." },
    mode: { $ref: "#/$defs/Mode", description: "How it listens." },
    limit: { type: ["integer", "null"], minimum: 1 },
    model: { anyOf: [{ type: "string" }, { type: "null" }], "x-widget": "model-picker" },
    tags: { type: "array", items: { type: "string" } },
    named: { type: "object", additionalProperties: { type: "string" } },
    loose: {},
  },
  required: ["hotkey"],
  $defs: {
    Mode: {
      description: "a mode",
      oneOf: [
        { type: "string", const: "once" },
        { type: "string", const: "always" },
      ],
    },
  },
};

const prop = (name: string): SchemaNode => ROOT.properties?.[name] ?? {};

describe("resolve", () => {
  it("follows a $ref and lets the sibling description win", () => {
    const node = resolve(prop("mode"), ROOT);
    expect(node.description).toBe("How it listens.");
    expect(enumOptions(node)).toEqual(["once", "always"]);
  });

  it("returns a node without a $ref untouched", () => {
    const node = prop("hotkey");
    expect(resolve(node, ROOT)).toBe(node);
  });

  it("gives up on a cycle instead of looping", () => {
    const cyclic: SchemaNode = { $defs: { A: { $ref: "#/$defs/A" } } };
    expect(() => resolve({ $ref: "#/$defs/A" }, cyclic)).not.toThrow();
  });

  it("unwraps a single-element allOf", () => {
    const node = resolve({ allOf: [{ $ref: "#/$defs/Mode" }], description: "d" }, ROOT);
    expect(node.description).toBe("d");
    expect(enumOptions(node)).toEqual(["once", "always"]);
  });
});

describe("unwrapNullable", () => {
  it("reads type: [X, null]", () => {
    const { node, nullable } = unwrapNullable(prop("limit"), ROOT);
    expect(nullable).toBe(true);
    expect(node.type).toBe("integer");
    expect(node.minimum).toBe(1);
  });

  it("reads anyOf [X, null] and carries x-widget", () => {
    const { node, nullable } = unwrapNullable(prop("model"), ROOT);
    expect(nullable).toBe(true);
    expect(node.type).toBe("string");
    expect(node["x-widget"]).toBe("model-picker");
  });

  it("leaves a plain node alone", () => {
    expect(unwrapNullable(prop("hotkey"), ROOT).nullable).toBe(false);
  });

  it("does not take a string enum for a nullable", () => {
    const { nullable } = unwrapNullable(prop("mode"), ROOT);
    expect(nullable).toBe(false);
  });
});

describe("nodeKind", () => {
  it("classifies what schemars emits", () => {
    expect(nodeKind(ROOT)).toBe("object");
    expect(nodeKind(prop("named"))).toBe("map");
    expect(nodeKind(prop("tags"))).toBe("array");
    expect(nodeKind(resolve(prop("mode"), ROOT))).toBe("enum");
    expect(nodeKind(unwrapNullable(prop("limit"), ROOT).node)).toBe("integer");
    expect(nodeKind({ type: "string" })).toBe("string");
    expect(nodeKind({ type: "number" })).toBe("number");
    expect(nodeKind({ type: "boolean" })).toBe("boolean");
  });

  it("falls back to raw for what it cannot draw", () => {
    expect(nodeKind(prop("loose"))).toBe("raw");
    expect(nodeKind({ type: ["string", "integer"] })).toBe("raw");
    expect(nodeKind({ type: "array", prefixItems: [{ type: "string" }] })).toBe("raw");
    expect(nodeKind({ type: "object" })).toBe("raw");
  });
});

describe("pointers", () => {
  it("escapes ~ and / when joining", () => {
    expect(pointerJoin("", "a/b")).toBe("/a~1b");
    expect(pointerJoin("/x", "~")).toBe("/x/~0");
    expect(pointerJoin("/tags", 2)).toBe("/tags/2");
  });

  it("splits and unescapes", () => {
    expect(pointerSegments("/a~1b/0")).toEqual(["a/b", "0"]);
    expect(pointerSegments("")).toEqual([]);
  });
});

describe("hasField", () => {
  const body = { tags: ["a"], named: { k: "v" } };

  it("finds a declared property even when the body lacks it", () => {
    expect(hasField(ROOT, body, "/hotkey")).toBe(true);
  });

  it("walks arrays and maps by what the body holds", () => {
    expect(hasField(ROOT, body, "/tags/0")).toBe(true);
    expect(hasField(ROOT, body, "/tags/1")).toBe(false);
    expect(hasField(ROOT, body, "/named/k")).toBe(true);
    expect(hasField(ROOT, body, "/named/zzz")).toBe(false);
  });

  it("refuses what the form does not draw", () => {
    expect(hasField(ROOT, body, "/nope")).toBe(false);
    expect(hasField(ROOT, body, "")).toBe(false);
    expect(hasField(ROOT, body, "hotkey")).toBe(false);
    expect(hasField(ROOT, body, "/constructor")).toBe(false);
  });
});

describe("defaultFor", () => {
  it("builds the smallest value the schema accepts", () => {
    expect(defaultFor(ROOT, ROOT)).toEqual({ hotkey: "" });
    expect(defaultFor(prop("mode"), ROOT)).toBe("once");
    expect(defaultFor(prop("limit"), ROOT)).toBe(1);
    expect(defaultFor(prop("tags"), ROOT)).toEqual([]);
    expect(defaultFor(prop("named"), ROOT)).toEqual({});
    expect(defaultFor({ type: "boolean" }, ROOT)).toBe(false);
    expect(defaultFor({ type: "integer", default: 7 }, ROOT)).toBe(7);
    expect(defaultFor(prop("loose"), ROOT)).toBeNull();
  });
});

describe("fieldLabel", () => {
  it("turns a key into a label", () => {
    expect(fieldLabel("gate_command")).toBe("Gate command");
    expect(fieldLabel("hotkey")).toBe("Hotkey");
  });
});
