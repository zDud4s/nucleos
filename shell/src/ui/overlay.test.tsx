import { describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import { useEffect, useState } from "react";
import { Modal } from "./Modal";
import { OverlayProvider, useOverlayOpen, useOverlayRegister, useReportOverlay } from "./overlay";

function Probe() {
  return <span data-testid="probe">{String(useOverlayOpen())}</span>;
}

function Reporter({ open }: { open: boolean }) {
  useReportOverlay(open);
  return null;
}

describe("overlay registry", () => {
  it("is false outside a provider and reporting does not throw", () => {
    render(
      <>
        <Reporter open />
        <Probe />
      </>,
    );
    expect(screen.getByTestId("probe").textContent).toBe("false");
  });

  it("ref-counts several overlays", () => {
    const { rerender } = render(
      <OverlayProvider>
        <Probe />
        <Reporter open />
        <Reporter open />
      </OverlayProvider>,
    );
    expect(screen.getByTestId("probe").textContent).toBe("true");
    rerender(
      <OverlayProvider>
        <Probe />
        <Reporter open={false} />
        <Reporter open />
      </OverlayProvider>,
    );
    expect(screen.getByTestId("probe").textContent).toBe("true");
    rerender(
      <OverlayProvider>
        <Probe />
        <Reporter open={false} />
        <Reporter open={false} />
      </OverlayProvider>,
    );
    expect(screen.getByTestId("probe").textContent).toBe("false");
  });

  it("cleans up on unmount", () => {
    const { rerender } = render(
      <OverlayProvider>
        <Probe />
        <Reporter open />
      </OverlayProvider>,
    );
    expect(screen.getByTestId("probe").textContent).toBe("true");
    rerender(
      <OverlayProvider>
        <Probe />
      </OverlayProvider>,
    );
    expect(screen.getByTestId("probe").textContent).toBe("false");
  });

  it("keeps register stable across count changes", () => {
    const registerCalls = vi.fn();
    function Counted() {
      const register = useOverlayRegister();
      useEffect(() => {
        registerCalls();
      }, [register]);
      useReportOverlay(true);
      return null;
    }
    const { rerender } = render(
      <OverlayProvider>
        <Probe />
        <Counted />
        <Reporter open={false} />
      </OverlayProvider>,
    );
    expect(registerCalls).toHaveBeenCalledTimes(1);
    rerender(
      <OverlayProvider>
        <Probe />
        <Counted />
        <Reporter open />
      </OverlayProvider>,
    );
    rerender(
      <OverlayProvider>
        <Probe />
        <Counted />
        <Reporter open={false} />
      </OverlayProvider>,
    );
    expect(registerCalls).toHaveBeenCalledTimes(1);
  });

  it("follows a Modal open and close", () => {
    let toggle: (open: boolean) => void = () => {};
    function Host() {
      const [open, setOpen] = useState(false);
      toggle = setOpen;
      return <Modal open={open} onOpenChange={setOpen} title="Sure?" />;
    }
    render(
      <OverlayProvider>
        <Probe />
        <Host />
      </OverlayProvider>,
    );
    expect(screen.getByTestId("probe").textContent).toBe("false");
    act(() => toggle(true));
    expect(screen.getByTestId("probe").textContent).toBe("true");
    act(() => toggle(false));
    expect(screen.getByTestId("probe").textContent).toBe("false");
  });
});
