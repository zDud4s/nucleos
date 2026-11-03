import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { TransferNotes, finalizeChatMove, unreachableOriginNotice } from "./MoveChat";

describe("moving a chat", () => {
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
});
