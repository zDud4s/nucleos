import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderWithQuery } from "../test/harness";
import { settingsDouble, type SeedDoc } from "../test/settings-double";
import type { SchemaNode } from "./schema";
import { SettingsForm, takesEffectPhrase } from "./SettingsForm";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

const SCHEMA: SchemaNode = {
  type: "object",
  description: "The mailbox this machine reads.",
  properties: { host: { type: "string" } },
};

function world(seed: Partial<SeedDoc> = {}) {
  const double = settingsDouble([
    { scope: "machine", name: "email", schema: SCHEMA, body: { host: "a.example" }, version: 1, ...seed },
  ]);
  daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => double.handle(path, init));
  return double;
}

function open(onDirty?: (dirty: boolean) => void) {
  return renderWithQuery(
    <SettingsForm scope="machine" name="email" takesEffect="when the daemon restarts" onDirty={onDirty} />,
  );
}

async function hostBox(): Promise<HTMLInputElement> {
  return (await screen.findByLabelText("Host")) as HTMLInputElement;
}

function clickSave() {
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
}

describe("SettingsForm", () => {
  it("draws the document from its schema, with the schema's own description and no heading", async () => {
    world();
    open();

    expect((await hostBox()).value).toBe("a.example");
    expect(screen.getByText("The mailbox this machine reads.")).toBeTruthy();
    expect(screen.getByText("Takes effect when the daemon restarts.")).toBeTruthy();
    // System's panels carry the only heading: its tests look for exactly one named after the area.
    expect(screen.queryByRole("heading")).toBeNull();
  });

  it("sends the edited body with the version it was read at, and says when it starts mattering", async () => {
    const double = world();
    open();

    fireEvent.change(await hostBox(), { target: { value: "b.example" } });
    clickSave();

    expect(await screen.findByText(/^saved and takes effect when the daemon restarts/)).toBeTruthy();
    expect(double.writes).toEqual([
      { scope: "machine", name: "email", body: { host: "b.example" }, version: 1 },
    ]);
    // The editor moved to what the server now holds, so nothing is left to save.
    await waitFor(() => {
      expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(true);
    });
  });

  it("reads and writes a channel document through one encoded path segment", async () => {
    const double = world({ name: "channels/telegram" });
    renderWithQuery(<SettingsForm scope="machine" name="channels/telegram" />);

    fireEvent.change(await hostBox(), { target: { value: "b.example" } });
    clickSave();

    await waitFor(() => {
      expect(double.writes.length).toBe(1);
    });
    expect(double.writes[0].name).toBe("channels/telegram");
    const paths = daemon.apiFetch.mock.calls.map(([path]) => path as string);
    expect(paths.length).toBeGreaterThan(0);
    expect(paths.every((path) => path === "/settings/machine/channels%2Ftelegram")).toBe(true);
  });

  it("keeps keys the schema does not draw, and removes the key of a cleared box", async () => {
    const double = world({ body: { host: "a.example", future_key: 7 } });
    open();

    fireEvent.change(await hostBox(), { target: { value: "" } });
    clickSave();

    await waitFor(() => {
      expect(double.writes.length).toBe(1);
    });
    expect(double.writes[0].body).toEqual({ future_key: 7 });
  });

  it("says when a document was never written, because defaults are not an empty document", async () => {
    world({ version: 0, body: { host: "default.example" } });
    open();

    expect((await hostBox()).value).toBe("default.example");
    expect(screen.getByText("Never written: these are the defaults.")).toBeTruthy();
  });

  it("treats a null body as an empty document", async () => {
    world({ body: null });
    // The double's seed turns a missing body into `{}`, and this test is about the wire's `null`.
    daemon.apiFetch.mockResolvedValue({ body: null, version: 0, schema: SCHEMA });
    open();

    expect((await hostBox()).value).toBe("");
  });

  /** The 422 the handler sends today: `{refusal: "invalid", detail}`, with nothing naming a field. */
  it("shows a 422 that carries only a detail as a note, and keeps the draft", async () => {
    const double = world({ validate: () => ({ message: "unknown field `hots`" }) });
    open();

    fireEvent.change(await hostBox(), { target: { value: "b.example" } });
    clickSave();

    expect(await screen.findByText("unknown field `hots`")).toBeTruthy();
    expect((await hostBox()).getAttribute("aria-invalid")).toBeNull();
    expect((await hostBox()).value).toBe("b.example");
    expect(double.writes).toEqual([]);
  });

  /** `core/src/http/settings.rs` sends no `field` today. This pins what the form does if a refusal names one. */
  it("puts a 422 that names a field on that field and keeps the draft", async () => {
    const double = world({
      validate: () => ({ field: "/host", message: "not a host name" }),
    });
    open();

    fireEvent.change(await hostBox(), { target: { value: "not valid" } });
    clickSave();

    expect(await screen.findByText("not a host name")).toBeTruthy();
    const box = await hostBox();
    expect(box.getAttribute("aria-invalid")).toBe("true");
    expect(box.value).toBe("not valid");
    expect(double.writes).toEqual([]);

    // The refetch a settled write triggers answers the same version, so the draft outlives it.
    await waitFor(() => {
      expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(false);
    });
    expect((await hostBox()).value).toBe("not valid");
  });

  it("shows a 422 naming a field nothing draws as a note", async () => {
    world({ validate: () => ({ field: "/gone", message: "that key is unknown" }) });
    open();

    fireEvent.change(await hostBox(), { target: { value: "b.example" } });
    clickSave();

    expect(await screen.findByText("that key is unknown")).toBeTruthy();
    expect((await hostBox()).getAttribute("aria-invalid")).toBeNull();
    expect((await hostBox()).value).toBe("b.example");
  });

  it("says the emergency stop is why nothing was written", async () => {
    const double = world();
    open();

    fireEvent.change(await hostBox(), { target: { value: "b.example" } });
    double.failNext = { status: 423, body: { refusal: "kill_switch" } };
    clickSave();

    expect(await screen.findByText(/emergency stop is engaged/)).toBeTruthy();
    expect(double.writes).toEqual([]);
    expect((await hostBox()).value).toBe("b.example");
  });

  it("says when the daemon does not consider this app the author", async () => {
    const double = world();
    open();

    fireEvent.change(await hostBox(), { target: { value: "b.example" } });
    double.failNext = { status: 403, body: { refusal: "not_ours" } };
    clickSave();

    expect(await screen.findByText(/does not consider this app the author/)).toBeTruthy();
    expect(double.writes).toEqual([]);
  });

  it("on a 409 shows what is stored now and says the edit was not saved", async () => {
    const double = world();
    open();

    fireEvent.change(await hostBox(), { target: { value: "mine.example" } });
    double.external("machine", "email", { host: "theirs.example" });
    clickSave();

    expect(await screen.findByText(/Someone changed this while you were editing/)).toBeTruthy();
    await waitFor(async () => {
      expect((await hostBox()).value).toBe("theirs.example");
    });
    expect(double.writes).toEqual([]);
  });

  /**
   * The refinement over D6. Nobody clicked anything here: a refetch that brings a new version must not
   * take the text away, and the save that follows is what reports the collision.
   */
  it("does not let a background refetch eat a draft", async () => {
    const double = world();
    const { queryClient } = open();

    fireEvent.change(await hostBox(), { target: { value: "mine.example" } });
    double.external("machine", "email", { host: "theirs.example" });
    await queryClient.invalidateQueries();

    expect((await hostBox()).value).toBe("mine.example");

    await waitFor(() => {
      expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(false);
    });
    clickSave();

    expect(await screen.findByText(/Someone changed this while you were editing/)).toBeTruthy();
    await waitFor(async () => {
      expect((await hostBox()).value).toBe("theirs.example");
    });
  });

  it("reverts to what the server holds, and reports dirtiness both ways", async () => {
    world();
    const onDirty = vi.fn();
    open(onDirty);

    fireEvent.change(await hostBox(), { target: { value: "b.example" } });
    expect(screen.getByText("unsaved changes")).toBeTruthy();
    expect(onDirty).toHaveBeenLastCalledWith(true);

    fireEvent.click(screen.getByRole("button", { name: "Revert" }));
    expect((await hostBox()).value).toBe("a.example");
    expect(screen.queryByText("unsaved changes")).toBeNull();
    expect(onDirty).toHaveBeenLastCalledWith(false);
  });

  it("saves with the keyboard", async () => {
    const double = world();
    open();

    const box = await hostBox();
    fireEvent.change(box, { target: { value: "b.example" } });
    fireEvent.keyDown(box, { key: "s", ctrlKey: true });

    await waitFor(() => {
      expect(double.writes.length).toBe(1);
    });
  });

  it("round-trips through Raw JSON, unknown keys included", async () => {
    const double = world({ body: { host: "a.example", future_key: 7 } });
    open();

    await hostBox();
    fireEvent.click(screen.getByRole("button", { name: "Raw JSON" }));
    const raw = screen.getByLabelText("Raw JSON") as HTMLTextAreaElement;
    expect(JSON.parse(raw.value)).toEqual({ host: "a.example", future_key: 7 });

    fireEvent.change(raw, { target: { value: '{"host":"raw.example","future_key":8}' } });
    fireEvent.click(screen.getByRole("button", { name: "Form" }));
    expect((await hostBox()).value).toBe("raw.example");

    clickSave();
    await waitFor(() => {
      expect(double.writes.length).toBe(1);
    });
    expect(double.writes[0].body).toEqual({ host: "raw.example", future_key: 8 });
  });

  it("refuses to leave Raw JSON, or to save, on text that does not parse", async () => {
    const double = world();
    open();

    await hostBox();
    fireEvent.click(screen.getByRole("button", { name: "Raw JSON" }));
    fireEvent.change(screen.getByLabelText("Raw JSON"), { target: { value: "{broken" } });

    fireEvent.click(screen.getByRole("button", { name: "Form" }));
    expect(screen.getByText("Not valid JSON")).toBeTruthy();
    expect(screen.getByLabelText("Raw JSON")).toBeTruthy();

    clickSave();
    expect(double.writes).toEqual([]);
    expect(screen.getByText("Not valid JSON")).toBeTruthy();
  });

  it("always shows a raw-mode refusal as a note, because no field is drawn to carry it", async () => {
    world({ validate: () => ({ field: "/host", message: "not a host name" }) });
    open();

    await hostBox();
    fireEvent.click(screen.getByRole("button", { name: "Raw JSON" }));
    fireEvent.change(screen.getByLabelText("Raw JSON"), { target: { value: '{"host":"x y"}' } });
    clickSave();

    expect(await screen.findByText("not a host name")).toBeTruthy();
    expect(screen.getByLabelText("Raw JSON")).toBeTruthy();
  });

  it("says a stored document does not read, in the núcleo's words", async () => {
    world({ version: 3, unreadable: "expected a mapping" });
    open();

    expect(await screen.findByText(/does not read: expected a mapping/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  });

  it("says when the daemon did not answer, rather than drawing an empty form", async () => {
    daemon.apiFetch.mockRejectedValue(new TypeError("fetch failed"));
    open();

    expect(await screen.findByText(/is not responding/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  });
});

describe("SettingsForm, history", () => {
  /** Two real versions, so version 1 has a snapshot the double can bring back. */
  function twoVersions() {
    const double = world();
    double.external("machine", "email", { host: "second.example" });
    return double;
  }

  function historyCalls(): unknown[][] {
    return daemon.apiFetch.mock.calls.filter(([path]) => String(path).endsWith("/history"));
  }

  it("does not ask for the history until it is opened", async () => {
    world();
    open();

    await hostBox();
    expect(historyCalls()).toEqual([]);

    fireEvent.click(screen.getByRole("button", { name: "History" }));

    expect(await screen.findByRole("list", { name: "Versions of email" })).toBeTruthy();
    expect(historyCalls().length).toBe(1);
    expect(screen.getByRole("button", { name: "History" }).getAttribute("aria-expanded")).toBe("true");
  });

  it("restores as a forward write: the old body becomes a new version", async () => {
    const double = twoVersions();
    open();

    expect((await hostBox()).value).toBe("second.example");
    fireEvent.click(screen.getByRole("button", { name: "History" }));
    fireEvent.click(await screen.findByRole("button", { name: "Restore version 1" }));

    expect(await screen.findByText(/^restored version 1 and takes effect when the daemon restarts/)).toBeTruthy();
    expect(double.rollbacks).toEqual([{ scope: "machine", name: "email", target: 1, current: 2 }]);
    await waitFor(async () => {
      expect((await hostBox()).value).toBe("a.example");
    });
    // The history only grows, and the panel stays open to show it.
    expect(await screen.findByText("version 3")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Restore version 2" })).toBeTruthy();
  });

  it("will not restore over a draft", async () => {
    const double = twoVersions();
    open();

    fireEvent.change(await hostBox(), { target: { value: "mine.example" } });
    fireEvent.click(screen.getByRole("button", { name: "History" }));

    const restore = (await screen.findByRole("button", { name: "Restore version 1" })) as HTMLButtonElement;
    expect(restore.disabled).toBe(true);
    expect(restore.title).toBe("Save or revert your edit first");

    fireEvent.click(screen.getByRole("button", { name: "Revert" }));
    await waitFor(() => {
      expect(restore.disabled).toBe(false);
    });
    expect(double.rollbacks).toEqual([]);
  });

  it("says the emergency stop is why a restore did nothing", async () => {
    const double = twoVersions();
    open();

    await hostBox();
    fireEvent.click(screen.getByRole("button", { name: "History" }));
    const restore = await screen.findByRole("button", { name: "Restore version 1" });
    double.failNext = { status: 423, body: { refusal: "kill_switch" } };
    fireEvent.click(restore);

    expect(await screen.findByText(/emergency stop is engaged/)).toBeTruthy();
    expect(double.rollbacks).toEqual([]);
    expect((await hostBox()).value).toBe("second.example");
  });

  it("on a 409 shows what is stored now and says nothing was restored", async () => {
    const double = twoVersions();
    open();

    await hostBox();
    fireEvent.click(screen.getByRole("button", { name: "History" }));
    const restore = await screen.findByRole("button", { name: "Restore version 1" });
    double.external("machine", "email", { host: "third.example" });
    fireEvent.click(restore);

    expect(await screen.findByText(/nothing was restored/)).toBeTruthy();
    await waitFor(async () => {
      expect((await hostBox()).value).toBe("third.example");
    });
    expect(double.rollbacks).toEqual([]);
  });

  /** The 422 the handler sends today: `{refusal: "invalid", detail}`, with nothing naming a field. */
  it("shows a refused restore that carries only a detail as a note", async () => {
    const double = twoVersions();
    open();

    await hostBox();
    fireEvent.click(screen.getByRole("button", { name: "History" }));
    const restore = await screen.findByRole("button", { name: "Restore version 1" });
    double.failNext = { status: 422, body: { refusal: "invalid", detail: "no longer a valid host" } };
    fireEvent.click(restore);

    expect(await screen.findByText("no longer a valid host")).toBeTruthy();
    expect((await hostBox()).getAttribute("aria-invalid")).toBeNull();
    expect(double.rollbacks).toEqual([]);
  });

  /**
   * `core/src/http/settings.rs` sends no `field` today, so this cannot happen on the wire yet. It pins what
   * the form does if a refusal names one: the field belongs to the OLD body, which is not what the form is
   * drawing, and putting the sentence on the field would accuse text nobody typed.
   */
  it("shows a refused restore as a note, never on a field", async () => {
    const double = twoVersions();
    open();

    await hostBox();
    fireEvent.click(screen.getByRole("button", { name: "History" }));
    const restore = await screen.findByRole("button", { name: "Restore version 1" });
    double.failNext = {
      status: 422,
      body: { refusal: "invalid", field: "/host", detail: "no longer a valid host" },
    };
    fireEvent.click(restore);

    expect(await screen.findByText("no longer a valid host")).toBeTruthy();
    expect((await hostBox()).getAttribute("aria-invalid")).toBeNull();
  });
});

describe("takesEffectPhrase", () => {
  it("spells the two tokens and renders anything else as the núcleo wrote it", () => {
    expect(takesEffectPhrase("restart")).toBe("when the daemon restarts");
    expect(takesEffectPhrase("live")).toBe("straight away");
    expect(takesEffectPhrase("at the next tick")).toBe("at the next tick");
  });
});
