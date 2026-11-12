import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { OpenPrompt, PromptAnswer } from "../data/liveRecords";
import { LivePrompt } from "./LivePrompt";

function show(prompt: OpenPrompt) {
  const onAnswer = vi.fn<(a: PromptAnswer) => void>();
  const view = render(<LivePrompt prompt={prompt} onAnswer={onAnswer} />);
  return { onAnswer, ...view };
}

function click(name: string) {
  fireEvent.click(screen.getByRole("button", { name }));
}

describe("LivePrompt", () => {
  it("answers a confirm dialog with accept true or false", () => {
    const prompt: OpenPrompt = {
      id: "p1",
      kind: "dialog",
      dialogType: "confirm",
      message: "Delete it?",
      defaultPrompt: "",
    };
    const first = show(prompt);
    expect(screen.getByText("Delete it?")).toBeDefined();
    click("OK");
    expect(first.onAnswer).toHaveBeenCalledWith({ accept: true, text: "" });
    first.unmount();

    const second = show(prompt);
    click("Cancel");
    expect(second.onAnswer).toHaveBeenCalledWith({ accept: false, text: "" });
  });

  it("answers a prompt dialog with the typed text", () => {
    const { onAnswer } = show({
      id: "p2",
      kind: "dialog",
      dialogType: "prompt",
      message: "Your name?",
      defaultPrompt: "Ada",
    });
    const input = screen.getByRole("textbox") as HTMLInputElement;
    expect(input.value).toBe("Ada");
    fireEvent.change(input, { target: { value: "Grace" } });
    click("OK");
    expect(onAnswer).toHaveBeenCalledWith({ accept: true, text: "Grace" });
  });

  it("answers a select with the chosen value", () => {
    const { onAnswer } = show({
      id: "p3",
      kind: "select",
      multiple: false,
      options: [
        { value: "a", label: "Alpha", selected: true },
        { value: "b", label: "Beta", selected: false },
      ],
    });
    expect((screen.getByLabelText("Alpha") as HTMLInputElement).checked).toBe(true);
    fireEvent.click(screen.getByLabelText("Beta"));
    click("Choose");
    expect(onAnswer).toHaveBeenCalledWith({ value: "b" });
  });

  it("sends a chosen file as base64 and refuses one above 10 MiB", async () => {
    const { onAnswer, container } = show({ id: "p4", kind: "file", multiple: false, accept: "" });
    const input = container.ownerDocument.querySelector('input[type="file"]') as HTMLInputElement;
    expect(input).not.toBeNull();

    const small = new File(["hello"], "hi.txt", { type: "text/plain" });
    fireEvent.change(input, { target: { files: [small] } });
    click("Send");
    await waitFor(() =>
      expect(onAnswer).toHaveBeenCalledWith({
        files: [{ name: "hi.txt", mime: "text/plain", data_b64: "aGVsbG8=" }],
      }),
    );

    onAnswer.mockClear();
    const big = new File(["x"], "big.bin", { type: "application/octet-stream" });
    Object.defineProperty(big, "size", { value: 10 * 1024 * 1024 + 1 });
    fireEvent.change(input, { target: { files: [big] } });
    click("Send");
    await waitFor(() => expect(screen.getByText("big.bin is larger than 10 MiB")).toBeDefined());
    expect(onAnswer).not.toHaveBeenCalled();
  });

  it("refuses a total above 10 MiB across files with a visible sentence", async () => {
    const { onAnswer, container } = show({ id: "p6", kind: "file", multiple: true, accept: "" });
    const input = container.ownerDocument.querySelector('input[type="file"]') as HTMLInputElement;
    const a = new File(["x"], "a.bin", { type: "application/octet-stream" });
    const b = new File(["x"], "b.bin", { type: "application/octet-stream" });
    Object.defineProperty(a, "size", { value: 6 * 1024 * 1024 });
    Object.defineProperty(b, "size", { value: 6 * 1024 * 1024 });
    fireEvent.change(input, { target: { files: [a, b] } });
    click("Send");
    await waitFor(() => expect(screen.getByText(/together/)).toBeDefined());
    expect(onAnswer).not.toHaveBeenCalled();
  });

  it("disables every submit while an answer is in flight", () => {
    const onAnswer = vi.fn<(a: PromptAnswer) => void>();
    render(
      <LivePrompt
        prompt={{ id: "p7", kind: "dialog", dialogType: "confirm", message: "Sure?", defaultPrompt: "" }}
        onAnswer={onAnswer}
        pending
      />,
    );
    expect((screen.getByRole("button", { name: "OK" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "OK" }));
    expect(onAnswer).not.toHaveBeenCalled();
  });

  it("sends auth credentials or cancel", () => {
    const prompt: OpenPrompt = { id: "p5", kind: "auth", origin: "https://example.test", realm: "Staging" };
    const first = show(prompt);
    expect(screen.getByText(/https:\/\/example\.test/)).toBeDefined();
    expect(screen.getByText(/Staging/)).toBeDefined();
    fireEvent.change(screen.getByLabelText("Username"), { target: { value: "ada" } });
    const password = screen.getByLabelText("Password") as HTMLInputElement;
    expect(password.type).toBe("password");
    fireEvent.change(password, { target: { value: "s3cret" } });
    click("Sign in");
    expect(first.onAnswer).toHaveBeenCalledWith({ username: "ada", password: "s3cret" });
    first.unmount();

    const second = show(prompt);
    click("Cancel");
    expect(second.onAnswer).toHaveBeenCalledWith({ cancel: true });
  });
});
