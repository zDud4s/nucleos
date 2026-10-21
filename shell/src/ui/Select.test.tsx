import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Field } from "./Field";
import { Select } from "./Select";

describe("Select", () => {
  it("is the native select, dressed by the one class", () => {
    render(
      <Select aria-label="Model" value="b" onChange={() => undefined}>
        <option value="a">A</option>
        <option value="b">B</option>
      </Select>,
    );

    const control = screen.getByRole("combobox", { name: "Model" }) as HTMLSelectElement;
    expect(control.tagName).toBe("SELECT");
    expect(control.className).toBe("ui-select");
    expect(control.value).toBe("b");
  });

  it("passes the native change event through", () => {
    const onChange = vi.fn();
    render(
      <Select aria-label="Model" value="a" onChange={(event) => onChange(event.target.value)}>
        <option value="a">A</option>
        <option value="b">B</option>
      </Select>,
    );

    fireEvent.change(screen.getByRole("combobox", { name: "Model" }), { target: { value: "b" } });
    expect(onChange).toHaveBeenCalledWith("b");
  });

  it("names its look with modifiers, not a caller's class", () => {
    render(
      <Select aria-label="Scope" density="sm" block mono defaultValue="x">
        <option value="x">x</option>
      </Select>,
    );

    expect(screen.getByRole("combobox", { name: "Scope" }).className).toBe(
      "ui-select ui-select-sm ui-select-block ui-select-mono",
    );
  });

  it("takes its name and hint from a Field", () => {
    render(
      <Field label="Model" helper="Used for every new run">
        <Select defaultValue="a">
          <option value="a">A</option>
        </Select>
      </Field>,
    );

    const control = screen.getByRole("combobox", { name: "Model" });
    expect(control.getAttribute("aria-describedby")).toBeTruthy();
    expect(screen.getByText("Used for every new run").id).toBe(control.getAttribute("aria-describedby"));
  });
});

/**
 * A page that writes `<select` dresses it again, which is how there came to be a dozen select
 * styles. Only this primitive may render the element. A multi-select is a list, not a `Select`,
 * and is the one shape still allowed (`<select multiple`).
 */
describe("Select authorship", () => {
  const shellSource = join(dirname(fileURLToPath(import.meta.url)), "..");
  const files = readdirSync(shellSource, { recursive: true })
    .filter((e): e is string => typeof e === "string" && e.endsWith(".tsx") && !e.endsWith(".test.tsx"))
    .map((e) => join(shellSource, e));

  it("no page renders a single-choice select of its own", () => {
    const violations: string[] = [];
    for (const file of files) {
      const key = relative(shellSource, file).split(sep).join("/");
      if (key === "ui/Select.tsx") continue;
      const source = readFileSync(file, "utf8");
      for (const hit of source.matchAll(/<select\b([^>]*)>/g)) {
        if (/\bmultiple\b/.test(hit[1] ?? "")) continue;
        violations.push(`${key}:${source.slice(0, hit.index).split("\n").length}`);
      }
    }
    expect(violations, violations.join("\n")).toEqual([]);
  });
});
