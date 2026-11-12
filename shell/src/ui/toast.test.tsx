import { describe, expect, it } from "vitest";
import { act, screen } from "@testing-library/react";
import { toastManager } from "./toast";

describe("toastManager", () => {
  it("adds a toast, announces errors as alerts, and closes it", () => {
    let id = "";
    act(() => {
      id = toastManager.add({ title: "Broke", description: "Try again", type: "error" });
    });
    expect(screen.getByRole("alert").textContent).toContain("Broke");
    act(() => {
      toastManager.add({ title: "Fine", type: "success" });
    });
    expect(screen.getByRole("status").textContent).toContain("Fine");
    act(() => toastManager.close(id));
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("deduplicates by key", () => {
    act(() => {
      toastManager.add({ title: "Same", type: "error", dedupeKey: "k" });
      toastManager.add({ title: "Same", type: "error", dedupeKey: "k" });
    });
    expect(screen.getAllByRole("alert")).toHaveLength(1);
  });

  it("promise: loading becomes success", async () => {
    let resolve!: (v: string) => void;
    const p = new Promise<string>((r) => (resolve = r));
    act(() => {
      void toastManager.promise(p, {
        loading: { title: "Loading..." },
        success: (d) => ({ title: `Done ${d}` }),
        error: () => ({ title: "Failed" }),
      });
    });
    expect(screen.getByRole("status").textContent).toContain("Loading...");
    await act(async () => resolve("ok"));
    expect(screen.getByRole("status").textContent).toContain("Done ok");
  });

  it("promise: loading becomes an error alert", async () => {
    const p = Promise.reject(new Error("no"));
    await act(async () => {
      await toastManager
        .promise(p, {
          loading: { title: "Loading..." },
          success: { title: "Done" },
          error: () => ({ title: "Something went wrong", description: "Please try again." }),
        })
        .catch(() => {});
    });
    expect(screen.getByRole("alert").textContent).toContain("Something went wrong");
  });
});
