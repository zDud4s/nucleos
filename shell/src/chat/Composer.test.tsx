import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Composer from "./Composer";
import type { ApiResult } from "../api";

function refuse(status: number): () => Promise<ApiResult<number>> {
  return async () => ({ ok: false, fault: "failed", status });
}

async function type(text: string) {
  fireEvent.change(screen.getByRole("textbox"), { target: { value: text } });
}

async function send() {
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: /send/i }));
  });
}

describe("Composer", () => {
  it.each([
    [409, /still working on the previous message/],
    [503, /this machine has none configured/],
    [500, /did not take the message/],
  ])("explains a %i instead of just failing", async (status, phrase) => {
    render(<Composer busy={false} onSend={refuse(status)} />);
    await type("olá");

    await send();

    expect(screen.getByText(phrase)).toBeTruthy();
  });

  it("keeps what you wrote when the daemon refuses it", async () => {
    // Clearing the box on a refusal loses the message and gives you nothing to retry with.
    render(<Composer busy={false} onSend={refuse(409)} />);
    await type("a long thing nobody wants to retype");

    await send();

    expect(screen.getByRole("textbox")).toHaveProperty(
      "value",
      "a long thing nobody wants to retype",
    );
  });

  it("clears the box once the message is taken", async () => {
    const onSend = vi.fn(async (): Promise<ApiResult<number>> => ({ ok: true, value: 7 }));
    render(<Composer busy={false} onSend={onSend} />);
    await type("olá");

    await send();

    expect(onSend).toHaveBeenCalledWith("olá");
    expect(screen.getByRole("textbox")).toHaveProperty("value", "");
  });

  it("will not send while this conversation is mid-turn", async () => {
    // One turn per chat is the daemon's rule, not a nicety: a second message would be refused 409.
    const onSend = vi.fn(async (): Promise<ApiResult<number>> => ({ ok: true, value: 7 }));
    render(<Composer busy onSend={onSend} />);

    expect(screen.getByRole("textbox")).toHaveProperty("disabled", true);
    expect(screen.getByRole("button")).toHaveProperty("disabled", true);
  });

  it("sends the trimmed text and refuses to send blank", async () => {
    const onSend = vi.fn(async (): Promise<ApiResult<number>> => ({ ok: true, value: 7 }));
    render(<Composer busy={false} onSend={onSend} />);

    await type("   ");
    expect(screen.getByRole("button")).toHaveProperty("disabled", true);

    await type("  olá  ");
    await send();
    expect(onSend).toHaveBeenCalledWith("olá");
  });
});
