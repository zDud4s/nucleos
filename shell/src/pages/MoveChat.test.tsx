import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { abortChatTransfer, beginChatTransfer, finishChatTransfer, importChatTransfer, packageChatTransfer, previewChatTransfer } from "../data/chats";
import { ApiRefusal, apiFetch } from "../data/client";
import { keys } from "../data/keys";
import { pairedMachine, saveMachines } from "../data/machineStore";
import { requestNativeStepUp } from "../remote/nativeTransport";
import { MoveChatDialog, TransferNotes, abortStartedMove, finalizeChatMove, finishWithRetry, stepUpForMove, unreachableOriginNotice } from "./MoveChat";

vi.mock("../remote/nativeTransport", () => ({ requestNativeStepUp: vi.fn() }));

vi.mock("../data/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../data/client")>();
  return { ...actual, apiFetch: vi.fn() };
});

vi.mock("../data/chats", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../data/chats")>();
  return {
    ...actual,
    abortChatTransfer: vi.fn(),
    beginChatTransfer: vi.fn(),
    finishChatTransfer: vi.fn(),
    importChatTransfer: vi.fn(),
    packageChatTransfer: vi.fn(),
    previewChatTransfer: vi.fn(),
    useChatProject: vi.fn(() => ({ data: { cwd: null } })),
  };
});

vi.mock("../data/machines", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../data/machines")>();
  return {
    ...actual,
    useMachines: vi.fn(() => [
      { id: "here", name: "This device", reachable: true },
      { id: "tower", name: "Tower", reachable: true },
    ]),
  };
});

function renderMoveDialog(onOpenChange = vi.fn()) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <MoveChatDialog chatId="chat-1" open onOpenChange={onOpenChange} />
    </QueryClientProvider>,
  );
  return onOpenChange;
}

function renderMoveDialogWithClient(onOpenChange = vi.fn()) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <MoveChatDialog chatId="chat-1" open onOpenChange={onOpenChange} />
    </QueryClientProvider>,
  );
  return { client, onOpenChange };
}

function prepareMove() {
  saveMachines([{ id: "tower", origin: "https://tower.example", name: "Tower" }]);
  vi.mocked(requestNativeStepUp).mockResolvedValue({ kind: "verified" });
  vi.mocked(apiFetch).mockResolvedValue({ name: "Tower" } as never);
  vi.mocked(previewChatTransfer).mockResolvedValue({
    will_stop: [],
    left_behind: [],
    running: false,
    target_ok: true,
    target_problem: null,
  });
  vi.mocked(beginChatTransfer).mockResolvedValue({} as never);
  vi.mocked(packageChatTransfer).mockResolvedValue({ transfer_id: "transfer-1" } as never);
}

describe("moving a chat", () => {
  beforeEach(() => {
    // Mock implementations set by one test (a never-settling import, a rejecting import) must not leak into the next.
    for (const mocked of [abortChatTransfer, beginChatTransfer, finishChatTransfer, importChatTransfer, packageChatTransfer, previewChatTransfer, apiFetch, requestNativeStepUp]) {
      vi.mocked(mocked).mockReset();
    }
    localStorage.clear();
  });

  it("the_dialog_lists_what_stops_and_what_stays_before_confirming", () => {
    render(
      <TransferNotes
        preview={{
          will_stop: ["A question is waiting for your answer."],
          left_behind: ["local.log"],
          running: true,
          target_ok: true,
          target_problem: null,
        }}
      />,
    );

    expect(screen.getByRole("heading", { name: "What stops" })).toBeTruthy();
    expect(screen.getByText("A question is waiting for your answer.")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "What stays behind" })).toBeTruthy();
    expect(screen.getByText("local.log")).toBeTruthy();
  });

  it("an_unreachable_origin_explains_why_it_cannot_move", () => {
    expect(unreachableOriginNotice("tower", false)).toBe("This chat's machine is unreachable, so it cannot be moved.");
    expect(unreachableOriginNotice("here", false)).toBeNull();
  });

  it("aborts_the_origin_when_a_failure_happens_after_begin", async () => {
    const abort = vi.fn().mockResolvedValue(undefined);
    await expect(
      finalizeChatMove({
        begin: vi.fn().mockResolvedValue(undefined),
        packageChat: vi.fn().mockRejectedValue(new Error("package failed")),
        importChat: vi.fn(),
        finish: vi.fn(),
        abort,
      }),
    ).rejects.toThrow("package failed");
    expect(abort).toHaveBeenCalledOnce();
  });

  it("a begin error still aborts the origin", async () => {
    const abort = vi.fn().mockResolvedValue(undefined);

    await expect(finalizeChatMove({
      begin: vi.fn().mockRejectedValue(new Error("begin timed out")),
      packageChat: vi.fn(),
      importChat: vi.fn(),
      finish: vi.fn(),
      abort,
    })).rejects.toThrow("begin timed out");

    expect(abort).toHaveBeenCalledOnce();
  });

  it("counts_an_already_imported_package_as_success", async () => {
    const finish = vi.fn().mockResolvedValue(undefined);
    const imported = await finalizeChatMove({
      begin: vi.fn().mockResolvedValue(undefined),
      packageChat: vi.fn().mockResolvedValue({ transfer_id: "transfer-1" }),
      importChat: vi.fn().mockResolvedValue({ already: true }),
      finish,
      abort: vi.fn(),
    });

    expect(imported).toEqual({ already: true });
    expect(finish).toHaveBeenCalledOnce();
  });

  it("a finish failure after import retries finish and never calls abort", async () => {
    const finish = vi.fn().mockRejectedValueOnce(new Error("network drop")).mockResolvedValue(undefined);
    const abort = vi.fn();

    await finalizeChatMove({
      begin: vi.fn().mockResolvedValue(undefined),
      packageChat: vi.fn().mockResolvedValue({ transfer_id: "transfer-1" }),
      importChat: vi.fn().mockResolvedValue({ already: false }),
      finish,
      abort,
    });

    expect(finish).toHaveBeenCalledTimes(2);
    expect(abort).not.toHaveBeenCalled();
  });

  it("an unknown import response retries the same transfer and never calls abort", async () => {
    const importChat = vi.fn()
      .mockRejectedValueOnce(new Error("connection dropped"))
      .mockResolvedValue({ already: true });
    const abort = vi.fn();

    await finalizeChatMove({
      begin: vi.fn().mockResolvedValue(undefined),
      packageChat: vi.fn().mockResolvedValue({ transfer_id: "transfer-1" }),
      importChat,
      finish: vi.fn().mockResolvedValue(undefined),
      abort,
    });

    expect(importChat).toHaveBeenCalledTimes(2);
    expect(abort).not.toHaveBeenCalled();
  });

  it("an unknown import followed by a refusal does not abort the origin", async () => {
    saveMachines([{ id: "tower", origin: "https://tower.example", name: "Tower" }]);
    vi.mocked(requestNativeStepUp).mockResolvedValue({ kind: "verified" });
    vi.mocked(apiFetch).mockResolvedValue({ name: "Tower" } as never);
    vi.mocked(previewChatTransfer).mockResolvedValue({
      will_stop: [],
      left_behind: [],
      running: false,
      target_ok: true,
      target_problem: null,
    });
    vi.mocked(beginChatTransfer).mockResolvedValue({} as never);
    vi.mocked(packageChatTransfer).mockResolvedValue({ transfer_id: "transfer-1" } as never);
    vi.mocked(importChatTransfer)
      .mockRejectedValueOnce(new Error("connection dropped"))
      .mockRejectedValueOnce(new Error("connection dropped"))
      .mockRejectedValueOnce(new Error("connection dropped"))
      .mockRejectedValue(new ApiRefusal(409, "already_here", "the destination refused the retry"));
    const onOpenChange = renderMoveDialog();

    await waitFor(() => expect((screen.getByLabelText("Move chat to") as HTMLSelectElement).value).toBe("tower"));
    fireEvent.click(screen.getByRole("button", { name: "Review move" }));
    await screen.findByRole("button", { name: "Move chat" });
    fireEvent.click(screen.getByRole("button", { name: "Move chat" }));
    await screen.findByRole("button", { name: "Retry" });
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));

    await waitFor(() => expect(importChatTransfer).toHaveBeenCalledTimes(6));
    expect(abortChatTransfer).not.toHaveBeenCalled();
    expect(onOpenChange).not.toHaveBeenCalled();
  });

  it("an origin changed after expiry close-out stops retrying", async () => {
    const finish = vi.fn().mockRejectedValue(
      new ApiRefusal(409, "origin_changed_after_expiry", "the origin took a new turn"),
    );

    await expect(finishWithRetry(finish)).rejects.toBeInstanceOf(ApiRefusal);

    expect(finish).toHaveBeenCalledOnce();
  });

  it("shows the destination name when close-out finds both copies live", async () => {
    prepareMove();
    vi.mocked(importChatTransfer).mockResolvedValue({} as never);
    vi.mocked(finishChatTransfer).mockRejectedValue(
      new ApiRefusal(409, "origin_changed_after_expiry", "the origin took a new turn"),
    );
    renderMoveDialog();

    await waitFor(() => expect((screen.getByLabelText("Move chat to") as HTMLSelectElement).value).toBe("tower"));
    fireEvent.click(screen.getByRole("button", { name: "Review move" }));
    fireEvent.click(await screen.findByRole("button", { name: "Move chat" }));

    expect(await screen.findByText(/now runs on Tower;.*both copies are live/i)).toBeTruthy();
  });

  it("a remote machine asks for step-up and this device does not", async () => {
    localStorage.setItem("nucleos-machines", JSON.stringify([{ id: "tower", origin: "https://tower.example", name: "Tower" }]));
    vi.mocked(requestNativeStepUp).mockResolvedValue({ kind: "verified" });

    await stepUpForMove("here");
    expect(requestNativeStepUp).not.toHaveBeenCalled();
    expect(pairedMachine("tower")).toBeDefined();
    await stepUpForMove("tower");

    expect(requestNativeStepUp).toHaveBeenCalledOnce();
  });

  it("the dialog cancel before begin does nothing", async () => {
    const abort = vi.fn().mockResolvedValue(undefined);

    await expect(abortStartedMove(null, abort)).resolves.toBe(false);

    expect(abort).not.toHaveBeenCalled();
  });

  it("Escape during transferring does not abort the move", async () => {
    saveMachines([{ id: "tower", origin: "https://tower.example", name: "Tower" }]);
    vi.mocked(requestNativeStepUp).mockResolvedValue({ kind: "verified" });
    vi.mocked(apiFetch).mockResolvedValue({ name: "Tower" } as never);
    vi.mocked(previewChatTransfer).mockResolvedValue({
      will_stop: [],
      left_behind: [],
      running: false,
      target_ok: true,
      target_problem: null,
    });
    vi.mocked(beginChatTransfer).mockResolvedValue({} as never);
    vi.mocked(packageChatTransfer).mockResolvedValue({ transfer_id: "transfer-1" } as never);
    vi.mocked(importChatTransfer).mockImplementation(() => new Promise(() => {}));
    const onOpenChange = renderMoveDialog();

    await waitFor(() => expect((screen.getByLabelText("Move chat to") as HTMLSelectElement).value).toBe("tower"));
    fireEvent.click(screen.getByRole("button", { name: "Review move" }));
    await screen.findByRole("button", { name: "Move chat" });
    fireEvent.click(screen.getByRole("button", { name: "Move chat" }));
    await waitFor(() => expect(beginChatTransfer).toHaveBeenCalledOnce());
    await waitFor(() => expect(importChatTransfer).toHaveBeenCalledOnce());

    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });

    await waitFor(() => expect(screen.getByText(/still being transferred/i)).toBeTruthy());
    expect(abortChatTransfer).not.toHaveBeenCalled();
    expect(onOpenChange).not.toHaveBeenCalled();
  });

  it("the import retry can close without aborting the origin", async () => {
    saveMachines([{ id: "tower", origin: "https://tower.example", name: "Tower" }]);
    vi.mocked(requestNativeStepUp).mockResolvedValue({ kind: "verified" });
    vi.mocked(apiFetch).mockResolvedValue({ name: "Tower" } as never);
    vi.mocked(previewChatTransfer).mockResolvedValue({
      will_stop: [],
      left_behind: [],
      running: false,
      target_ok: true,
      target_problem: null,
    });
    vi.mocked(beginChatTransfer).mockResolvedValue({} as never);
    vi.mocked(packageChatTransfer).mockResolvedValue({ transfer_id: "transfer-1" } as never);
    vi.mocked(importChatTransfer).mockRejectedValue(new Error("connection dropped"));
    const onOpenChange = renderMoveDialog();

    await waitFor(() => expect((screen.getByLabelText("Move chat to") as HTMLSelectElement).value).toBe("tower"));
    fireEvent.click(screen.getByRole("button", { name: "Review move" }));
    await screen.findByRole("button", { name: "Move chat" });
    fireEvent.click(screen.getByRole("button", { name: "Move chat" }));
    // The modal header also has an icon button labelled "Close", so wait on the import-retry notice instead.
    await screen.findByText(/may have completed at the destination/i, {}, { timeout: 4000 });

    const footerClose = screen.getAllByRole("button", { name: "Close" }).find((button) => button.textContent === "Close");
    expect(footerClose).toBeDefined();
    fireEvent.click(footerClose!);

    expect(abortChatTransfer).not.toHaveBeenCalled();
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("closing an uncertain import clears its review and refreshes the chat list", async () => {
    prepareMove();
    vi.mocked(importChatTransfer).mockRejectedValue(new Error("connection dropped"));
    const { client, onOpenChange } = renderMoveDialogWithClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");

    await waitFor(() => expect((screen.getByLabelText("Move chat to") as HTMLSelectElement).value).toBe("tower"));
    fireEvent.click(screen.getByRole("button", { name: "Review move" }));
    fireEvent.click(await screen.findByRole("button", { name: "Move chat" }));
    await screen.findByText(/may have completed at the destination/i, {}, { timeout: 4000 });
    const footerClose = screen.getAllByRole("button", { name: "Close" }).find((button) => button.textContent === "Close");
    fireEvent.click(footerClose!);

    expect(invalidate).toHaveBeenCalledWith({ queryKey: keys.chats.all });
    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(screen.getByRole("button", { name: "Review move" })).toBeTruthy();
  });

  it("Escape during an import retry closes like the footer action", async () => {
    prepareMove();
    vi.mocked(importChatTransfer).mockRejectedValue(new Error("connection dropped"));
    const onOpenChange = renderMoveDialog();

    await waitFor(() => expect((screen.getByLabelText("Move chat to") as HTMLSelectElement).value).toBe("tower"));
    fireEvent.click(screen.getByRole("button", { name: "Review move" }));
    fireEvent.click(await screen.findByRole("button", { name: "Move chat" }));
    await screen.findByText(/may have completed at the destination/i, {}, { timeout: 4000 });

    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });

    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
    expect(abortChatTransfer).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Review move" })).toBeTruthy();
  });
});
