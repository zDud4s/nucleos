import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ClassesView } from "./ClassesView";
import { createAppQueryClient } from "../app/queryClient";
import type { FileItems } from "../data/project-map";

beforeEach(() => daemon.apiFetch.mockReset());

const item = (id: string, kind: string, over: object = {}) => ({
  id,
  name: id,
  container: null,
  kind,
  exported: true,
  documented: false,
  line: 1,
  ...over,
});

const ANSWER: FileItems = {
  path: "core/src/a.rs",
  reader: "rust",
  items: [
    item("Job", "shape"),
    item("State", "shape"),
    item("Run", "contract"),
    item("helper", "function"),
  ] as FileItems["items"],
  references: [{ from: "Job", to: "State" }],
  missed: [],
  members: [
    { owner: "Job", name: "id", kind: "field", ty: "u32", exported: true, line: 2 },
    { owner: "Job", name: "state", kind: "field", ty: "State", exported: false, line: 3 },
    { owner: "State", name: "Queued", kind: "variant", ty: null, exported: false, line: 6 },
    { owner: "State", name: "Done", kind: "variant", ty: "(u8)", exported: false, line: 7 },
  ],
  uses: [{ path: "std::fmt", line: 1 }],
};

const MODULES = ["core/src/a.rs", "core/src/b.rs", "shell/src/x.tsx"];

function show(answer: FileItems | Error, modules = MODULES) {
  daemon.apiFetch.mockImplementation(async () => {
    if (answer instanceof Error) throw answer;
    return answer;
  });
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <ClassesView projectId="alpha" modules={modules} />
    </QueryClientProvider>,
  );
}

describe("the Classes view", () => {
  it("offers only the Rust files and reads the first one", async () => {
    show(ANSWER);
    const picker = (await screen.findByLabelText("File")) as HTMLSelectElement;
    expect(Array.from(picker.options).map((o) => o.value)).toEqual([
      "core/src/a.rs",
      "core/src/b.rs",
    ]);
    expect(daemon.apiFetch.mock.calls[0][0]).toContain("path=core%2Fsrc%2Fa.rs");
  });

  it("draws one box per shape with its fields and variants", async () => {
    show(ANSWER);
    const job = await screen.findByRole("region", { name: "Job" });
    expect(within(job).getByText("id")).toBeTruthy();
    expect(within(job).getByText("u32")).toBeTruthy();
    const state = screen.getByRole("region", { name: "State" });
    expect(within(state).getByText("Queued")).toBeTruthy();
    expect(within(state).getByText("(u8)")).toBeTruthy();
    expect(screen.getByRole("region", { name: "Run" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: "helper" })).toBeNull();
  });

  it("lists the relations between boxes and the use paths", async () => {
    show(ANSWER);
    expect(await screen.findByText("Job → State")).toBeTruthy();
    expect(screen.getByText("std::fmt")).toBeTruthy();
  });

  it("says what the reader missed", async () => {
    show({ ...ANSWER, missed: ["macro-generated structs"] });
    expect(await screen.findByText(/macro-generated structs/)).toBeTruthy();
  });

  it("says so when a file declares no shape", async () => {
    show({ ...ANSWER, items: [], members: [], references: [] });
    expect(await screen.findByText(/declares no struct, enum or trait/)).toBeTruthy();
  });

  it("says so when the project has no Rust file", () => {
    show(ANSWER, ["shell/src/x.tsx"]);
    expect(screen.getByText(/no Rust file/i)).toBeTruthy();
  });

  it("reads another file when picked", async () => {
    show(ANSWER);
    const picker = await screen.findByLabelText("File");
    fireEvent.change(picker, { target: { value: "core/src/b.rs" } });
    await screen.findByRole("region", { name: "Job" });
    const calls = daemon.apiFetch.mock.calls;
    const last = calls[calls.length - 1][0] as string;
    expect(last).toContain("path=core%2Fsrc%2Fb.rs");
  });

  it("reports a read that failed", async () => {
    show(new Error("boom"));
    expect(await screen.findByText(/could not read/i)).toBeTruthy();
  });
});
