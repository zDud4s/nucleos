import { fireEvent, screen, within } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { renderWithQuery } from "../test/harness";
import { FieldNode } from "./FieldNode";
import type { SchemaNode } from "./schema";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const SCHEMA: SchemaNode = {
  type: "object",
  properties: {
    hotkey: { type: "string", description: "Push to talk." },
    limit: { type: ["integer", "null"], minimum: 1 },
    enabled: { type: "boolean" },
    mode: { $ref: "#/$defs/Mode" },
    tags: { type: "array", items: { type: "string" } },
    named: { type: "object", additionalProperties: { type: "string" } },
    gate: { type: "string", "x-widget": "command" },
    loose: {},
  },
  required: ["hotkey"],
  $defs: {
    Mode: {
      oneOf: [
        { type: "string", const: "once" },
        { type: "string", const: "always" },
      ],
    },
  },
};

function Harness({
  schema = SCHEMA,
  initial = { hotkey: "F9" },
  errors = {},
}: {
  schema?: SchemaNode;
  initial?: unknown;
  errors?: Record<string, string>;
}) {
  const [value, setValue] = useState<unknown>(initial);
  return (
    <div>
      <FieldNode node={schema} root={schema} value={value} onChange={setValue} path="" label="Settings" errors={errors} bare />
      <output data-testid="json">{JSON.stringify(value)}</output>
    </div>
  );
}

const current = (): unknown => JSON.parse(screen.getByTestId("json").textContent ?? "null");

describe("FieldNode", () => {
  it("draws a labelled control per property, with the description beside it", () => {
    renderWithQuery(<Harness />);
    expect((screen.getByLabelText("Hotkey") as HTMLInputElement).value).toBe("F9");
    expect(screen.getByText("Push to talk.")).toBeDefined();
    expect(screen.getByLabelText("Enabled")).toBeDefined();
    expect(screen.getByLabelText("Mode")).toBeDefined();
  });

  it("edits a string and keeps keys the schema does not declare", () => {
    renderWithQuery(<Harness initial={{ hotkey: "F9", extra: { kept: true } }} />);
    fireEvent.change(screen.getByLabelText("Hotkey"), { target: { value: "F10" } });
    expect(current()).toEqual({ hotkey: "F10", extra: { kept: true } });
  });

  it("removes the key when an optional string is cleared, instead of sending an empty one", () => {
    renderWithQuery(<Harness initial={{ hotkey: "F9", gate: "make test" }} />);
    fireEvent.change(screen.getByLabelText("Gate"), { target: { value: "" } });
    expect(current()).toEqual({ hotkey: "F9" });
  });

  it("keeps a required string as an empty string", () => {
    renderWithQuery(<Harness />);
    fireEvent.change(screen.getByLabelText("Hotkey"), { target: { value: "" } });
    expect(current()).toEqual({ hotkey: "" });
  });

  it("reads a number, and removes the key when the box is emptied", () => {
    renderWithQuery(<Harness />);
    const box = screen.getByLabelText("Limit");
    fireEvent.change(box, { target: { value: "5" } });
    expect(current()).toEqual({ hotkey: "F9", limit: 5 });
    fireEvent.change(box, { target: { value: "" } });
    expect(current()).toEqual({ hotkey: "F9" });
  });

  it("toggles a boolean", () => {
    renderWithQuery(<Harness />);
    fireEvent.click(screen.getByLabelText("Enabled"));
    expect(current()).toEqual({ hotkey: "F9", enabled: true });
  });

  it("offers an enum's choices and an empty one while it is optional", () => {
    renderWithQuery(<Harness />);
    const select = screen.getByLabelText("Mode");
    expect(within(select).getAllByRole("option").map((o) => o.textContent)).toEqual(["", "once", "always"]);
    fireEvent.change(select, { target: { value: "always" } });
    expect(current()).toEqual({ hotkey: "F9", mode: "always" });
    fireEvent.change(select, { target: { value: "" } });
    expect(current()).toEqual({ hotkey: "F9" });
  });

  it("adds, edits and removes list items", () => {
    renderWithQuery(<Harness initial={{ hotkey: "F9", tags: ["x"] }} />);
    fireEvent.change(screen.getByLabelText("Tags 1"), { target: { value: "y" } });
    fireEvent.click(screen.getByRole("button", { name: "Add Tags" }));
    expect(current()).toEqual({ hotkey: "F9", tags: ["y", ""] });
    fireEvent.click(screen.getByRole("button", { name: "Remove Tags 1" }));
    expect(current()).toEqual({ hotkey: "F9", tags: [""] });
  });

  it("adds and removes map entries", () => {
    renderWithQuery(<Harness initial={{ hotkey: "F9", named: { k: "v" } }} />);
    expect((screen.getByLabelText("k") as HTMLInputElement).value).toBe("v");
    fireEvent.change(screen.getByLabelText("New key for Named"), { target: { value: "z" } });
    fireEvent.click(screen.getByRole("button", { name: "Add entry to Named" }));
    expect(current()).toEqual({ hotkey: "F9", named: { k: "v", z: "" } });
    fireEvent.click(screen.getByRole("button", { name: "Remove k" }));
    expect(current()).toEqual({ hotkey: "F9", named: { z: "" } });
  });

  it("does not add a map entry under a key that is already there", () => {
    renderWithQuery(<Harness initial={{ hotkey: "F9", named: { k: "v" } }} />);
    fireEvent.change(screen.getByLabelText("New key for Named"), { target: { value: "k" } });
    fireEvent.click(screen.getByRole("button", { name: "Add entry to Named" }));
    expect(current()).toEqual({ hotkey: "F9", named: { k: "v" } });
  });

  it("shows an error on the field its pointer names, and marks the control invalid", () => {
    renderWithQuery(<Harness errors={{ "/hotkey": "expected a chord" }} />);
    expect(screen.getByText("expected a chord")).toBeDefined();
    expect(screen.getByLabelText("Hotkey").getAttribute("aria-invalid")).toBe("true");
  });

  it("uses the widget a schema asks for", () => {
    renderWithQuery(<Harness />);
    expect(screen.getByLabelText("Gate").getAttribute("spellcheck")).toBe("false");
  });

  it("falls back to a JSON box for a node it cannot draw, and holds back invalid JSON", () => {
    renderWithQuery(<Harness />);
    const box = screen.getByLabelText("Loose (JSON)");
    fireEvent.change(box, { target: { value: '{"a":1}' } });
    expect(current()).toEqual({ hotkey: "F9", loose: { a: 1 } });
    fireEvent.change(box, { target: { value: "{" } });
    expect(screen.getByText("Not valid JSON")).toBeDefined();
    expect(current()).toEqual({ hotkey: "F9", loose: { a: 1 } });
  });

  it("stops descending into a schema that refers to itself", () => {
    const cyclic: SchemaNode = {
      $ref: "#/$defs/Node",
      $defs: { Node: { type: "object", properties: { child: { $ref: "#/$defs/Node" } } } },
    };
    renderWithQuery(<Harness schema={cyclic} initial={{}} />);
    expect(screen.getAllByLabelText(/\(JSON\)/).length).toBeGreaterThan(0);
  });
});
