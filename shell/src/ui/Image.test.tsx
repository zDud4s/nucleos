import { describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Image } from "./Image";

describe("Image", () => {
  it("shows the placeholder (hidden from assistive tech) until the image loads", () => {
    const { container } = render(<Image src="/a.png" alt="Ada" placeholder="data:image/gif;base64,R0lG" />);
    const root = container.querySelector(".ui-image") as HTMLElement;
    expect(root.getAttribute("data-state")).toBe("loading");
    expect(container.querySelector(".ui-image-placeholder")?.getAttribute("aria-hidden")).toBe("true");
    fireEvent.load(screen.getByAltText("Ada"));
    expect(root.getAttribute("data-state")).toBe("loaded");
  });

  it("reserves the box through aspect ratio and size", () => {
    const { container } = render(<Image src="/a.png" alt="" aspectRatio="16 / 9" width={40} height={40} />);
    const root = container.querySelector(".ui-image") as HTMLElement;
    expect(root.style.aspectRatio).toBe("16 / 9");
    expect(root.style.width).toBe("40px");
  });

  it("keeps the placeholder and drops the broken image on error", () => {
    const { container } = render(<Image src="/missing.png" alt="x" placeholder="/tiny.png" />);
    fireEvent.error(screen.getByAltText("x"));
    expect(screen.queryByAltText("x")).toBeNull();
    expect(container.querySelector(".ui-image-placeholder")).not.toBeNull();
    expect((container.querySelector(".ui-image") as HTMLElement).getAttribute("data-state")).toBe("failed");
  });

  it("draws a circle for avatars and lazy-loads by default", () => {
    const { container } = render(<Image src="/a.png" alt="" shape="circle" />);
    expect(container.querySelector(".ui-image-circle")).not.toBeNull();
    expect(container.querySelector("img.ui-image-full")?.getAttribute("loading")).toBe("lazy");
  });
});
