import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";

const roster = vi.hoisted(() => ({
  data: undefined as Array<{ project_id: string; project_root: string | null }> | undefined,
}));
vi.mock("../data/system", () => ({
  useProjects: () => ({
    data: roster.data,
    isPending: roster.data === undefined,
    isError: false,
  }),
}));

import { ProjectPicker, projectsRoot } from "./ProjectPicker";

describe("projectsRoot", () => {
  it("is the folder the projects sit in side by side", () => {
    expect(projectsRoot(["C:/Projects/nucleos", "C:/Projects/site"])).toBe("C:/Projects");
  });

  it("reads Windows separators and trailing slashes, and ignores case", () => {
    expect(projectsRoot(["C:\\Projects\\nucleos\\", "c:/projects/site"])).toBe("C:/Projects");
  });

  it("steps out of a single project to the folder that holds it", () => {
    expect(projectsRoot(["C:/Projects/nucleos"])).toBe("C:/Projects");
  });

  it("steps out of a project that another one is nested in", () => {
    expect(projectsRoot(["/home/me/work/app", "/home/me/work/app/sub"])).toBe("/home/me/work");
  });

  // A whole drive is not what Root was asked to mean.
  it("is nothing when the only thing shared is a filesystem root", () => {
    expect(projectsRoot(["C:/nucleos", "C:/site"])).toBeNull();
    expect(projectsRoot(["/srv", "/opt"])).toBeNull();
    expect(projectsRoot(["C:/Projects/a", "D:/Projects/b"])).toBeNull();
    expect(projectsRoot([])).toBeNull();
  });
});

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
    roster.data = [
      { project_id: "nucleos", project_root: "C:/Projects/nucleos" },
      { project_id: "site", project_root: "C:/Projects/site" },
      { project_id: "unpromoted", project_root: null },
    ];
    draw();

    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getAllByRole("radio")).toHaveLength(3);
    expect(within(dialog).getByText("C:/Projects")).toBeTruthy();
    expect(within(dialog).queryByText("unpromoted")).toBeNull();
    expect(within(dialog).queryByRole("textbox")).toBeNull();
    expect(within(dialog).getByText(/your message is held/i)).toBeTruthy();
  });

  it("confirms nothing until a row is chosen, then hands back its folder", () => {
    roster.data = [{ project_id: "nucleos", project_root: "C:/Projects/nucleos" }];
    const onChoose = draw();

    const use = screen.getByRole("button", { name: "Use this" }) as HTMLButtonElement;
    expect(use.disabled).toBe(true);
    fireEvent.click(screen.getByRole("radio", { name: /root/i }));
    fireEvent.click(use);

    expect(onChoose).toHaveBeenCalledWith("C:/Projects");
  });

  it("offers Root disabled when no folder holds the projects", () => {
    roster.data = [
      { project_id: "a", project_root: "C:/a" },
      { project_id: "b", project_root: "D:/b" },
    ];
    draw();

    expect((screen.getByRole("radio", { name: /root/i }) as HTMLInputElement).disabled).toBe(true);
  });
});
