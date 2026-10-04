import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";

const roster = vi.hoisted(() => ({
  data: undefined as Array<{ project_id: string; project_root: string | null }> | undefined,
}));
const home = vi.hoisted(() => ({ root: undefined as string | null | undefined }));
vi.mock("../data/system", () => ({
  useHome: () => ({
    data: home.root === undefined ? undefined : { root: home.root },
    isPending: home.root === undefined,
  }),
  useProjects: () => ({
    data: roster.data,
    isPending: roster.data === undefined,
    isError: false,
  }),
}));

import { ProjectPicker } from "./ProjectPicker";

describe("ProjectPicker", () => {
  function draw(onChoose = vi.fn()) {
    render(
      <ProjectPicker
        open
        onOpenChange={() => {}}
        onChoose={onChoose}
        pending={false}
        holding
      />,
    );
    return onChoose;
  }

  it("offers the roster's rooted projects and Root, and nothing to type", () => {
    home.root = "C:/Projects/nucleos";
    roster.data = [
      { project_id: "nucleos", project_root: "C:/Projects/nucleos" },
      { project_id: "site", project_root: "C:/Projects/site" },
      { project_id: "unpromoted", project_root: null },
    ];
    draw();

    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getAllByRole("radio")).toHaveLength(3);
    // Root is the folder the daemon runs in, shown as it said it.
    expect(within(dialog).getAllByText("C:/Projects/nucleos")).toHaveLength(2);
    expect(within(dialog).queryByText("unpromoted")).toBeNull();
    expect(within(dialog).queryByRole("textbox")).toBeNull();
    expect(within(dialog).getByText(/your message is held/i)).toBeTruthy();
  });

  it("confirms nothing until a row is chosen, then hands back its folder", () => {
    home.root = "D:/NucleOS";
    roster.data = [{ project_id: "nucleos", project_root: "C:/Projects/nucleos" }];
    const onChoose = draw();

    const use = screen.getByRole("button", { name: "Use this" }) as HTMLButtonElement;
    expect(use.disabled).toBe(true);
    fireEvent.click(screen.getByRole("radio", { name: /root/i }));
    fireEvent.click(use);

    expect(onChoose).toHaveBeenCalledWith("D:/NucleOS");
  });

  it("offers Root disabled while the daemon has not said where it lives", () => {
    home.root = undefined;
    roster.data = [{ project_id: "a", project_root: "C:/a" }];
    draw();

    expect((screen.getByRole("radio", { name: /root/i }) as HTMLInputElement).disabled).toBe(true);
    expect(screen.getByText(/reading where NucleOS lives/i)).toBeTruthy();
  });

  it("offers Root disabled when the daemon could not read its own folder", () => {
    home.root = null;
    roster.data = [{ project_id: "a", project_root: "C:/a" }];
    draw();

    expect((screen.getByRole("radio", { name: /root/i }) as HTMLInputElement).disabled).toBe(true);
  });
});
