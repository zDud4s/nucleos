import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import type { CaptureRequest } from "../data/captures";
import { renderWithRouter } from "../test/harness";
import { CapturePanel } from "./CapturePanel";
import { CapturesWaiting } from "./CapturesWaiting";
import { timeLeft } from "./CaptureAnswerForm";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

function request(over: Partial<CaptureRequest> = {}): CaptureRequest {
  return {
    job_id: 7,
    project_id: "alpha",
    causes: [],
    prompt_text: "What went wrong?",
    state: "open",
    deadline: "2026-10-08T12:00:00+00:00",
    seconds_left: 4320,
    note_id: null,
    created_at: "2026-10-08T10:00:00+00:00",
    closed_at: null,
    ...over,
  };
}

function serve(rows: CaptureRequest[]) {
  daemon.apiFetch.mockImplementation((path: string) => {
    if (path === "/capture-requests") return Promise.resolve(rows.filter((r) => r.state === "open"));
    if (path === "/capture-requests?state=all") return Promise.resolve(rows);
    return Promise.resolve({ note_id: 1, released: true });
  });
}

const posts = () => daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("timeLeft", () => {
  it("formats the time left", () => {
    expect(timeLeft(4320)).toBe("1h 12m left");
    expect(timeLeft(300)).toBe("5m left");
    expect(timeLeft(30)).toBe("less than a minute left");
    expect(timeLeft(0)).toBe("expiring");
    expect(timeLeft(-5)).toBe("expiring");
  });
});

describe("CapturesWaiting", () => {
  it("draws nothing when no request is open", async () => {
    serve([request({ state: "dismissed" })]);
    const { container } = await renderWithRouter(<CapturesWaiting onSelect={() => {}} />);
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalled());
    expect(container.querySelector("section")).toBeNull();
    expect(screen.queryByText("Asked of you")).toBeNull();
  });

  it("orders requests by deadline", async () => {
    serve([
      request({ job_id: 1, deadline: "2026-10-08T13:00:00+00:00", prompt_text: "later" }),
      request({ job_id: 2, deadline: "2026-10-08T11:00:00+00:00", prompt_text: "sooner" }),
    ]);
    await renderWithRouter(<CapturesWaiting onSelect={() => {}} />);
    await screen.findByText("sooner");
    const boxes = screen.getAllByRole("textbox").map((box) => box.getAttribute("aria-label"));
    expect(boxes).toEqual(["Answer for job #2", "Answer for job #1"]);
  });

  it("keeps Answer disabled until there is text, then posts it from the shell", async () => {
    serve([request()]);
    await renderWithRouter(<CapturesWaiting onSelect={() => {}} />);
    const button = (await screen.findByRole("button", { name: "Answer" })) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    fireEvent.change(screen.getByRole("textbox", { name: "Answer for job #7" }), {
      target: { value: "the disk was full" },
    });
    expect(button.disabled).toBe(false);
    fireEvent.click(button);
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(posts()[0][0]).toBe("/capture-requests/7/answer");
    expect(JSON.parse(posts()[0][1].body)).toEqual({ text: "the disk was full", origin: "shell" });
  });

  it("dismisses only after a confirmation", async () => {
    serve([request()]);
    await renderWithRouter(<CapturesWaiting onSelect={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Dismiss" }));
    expect(posts()).toEqual([]);
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(screen.getByRole("button", { name: "Dismiss for good" }));
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(posts()[0][0]).toBe("/capture-requests/7/dismiss");
  });

  it("opens the request panel from its heading line", async () => {
    serve([request()]);
    const onSelect = vi.fn();
    await renderWithRouter(<CapturesWaiting onSelect={onSelect} />);
    fireEvent.click(await screen.findByRole("button", { name: /Job #7/ }));
    expect(onSelect).toHaveBeenCalledWith("capture:7");
  });
});

describe("CapturePanel", () => {
  it("is read-only once closed and links to the note", async () => {
    serve([request({ state: "answered", note_id: 12 })]);
    const onSelect = vi.fn();
    await renderWithRouter(<CapturePanel id={7} onSelect={onSelect} />);
    await screen.findByText("answered");
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.queryByRole("button", { name: "Answer" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /Open note #12/ }));
    expect(onSelect).toHaveBeenCalledWith("note:12");
  });

  it("shows the form while the request is open", async () => {
    serve([request()]);
    await renderWithRouter(<CapturePanel id={7} onSelect={() => {}} />);
    expect(await screen.findByRole("textbox", { name: "Answer for job #7" })).toBeDefined();
  });
});
