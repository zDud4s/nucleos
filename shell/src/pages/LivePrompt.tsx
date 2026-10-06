import { useState, type ReactNode } from "react";
import { Button, Field, Modal } from "../ui";
import type { OpenPrompt, PromptAnswer } from "../data/liveRecords";

const MAX_FILE_BYTES = 10 * 1024 * 1024;
const MAX_NAME_CHARS = 128;

export interface LivePromptProps {
  prompt: OpenPrompt;
  onAnswer: (answer: PromptAnswer) => void;
  pending?: boolean;
}

/** Reads a file's bytes; falls back to FileReader where `Blob.arrayBuffer` does not exist. */
function readBytes(file: File): Promise<Uint8Array> {
  if (typeof file.arrayBuffer === "function") {
    return file.arrayBuffer().then((buf) => new Uint8Array(buf));
  }
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(new Uint8Array(reader.result as ArrayBuffer));
    reader.onerror = () => reject(reader.error);
    reader.readAsArrayBuffer(file);
  });
}

/** Base64 in chunks, so a large file never spreads into one huge argument list. */
function toBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

function dialogLabels(type: "alert" | "confirm" | "prompt" | "beforeunload"): { yes: string; no: string | null } {
  switch (type) {
    case "alert":
      return { yes: "OK", no: null };
    case "beforeunload":
      return { yes: "Leave", no: "Stay" };
    default:
      return { yes: "OK", no: "Cancel" };
  }
}

/** One open page prompt, shown as a modal; reports a contract-shaped answer. */
export function LivePrompt({ prompt, onAnswer, pending = false }: LivePromptProps) {
  const [text, setText] = useState(prompt.kind === "dialog" ? prompt.defaultPrompt : "");
  const [chosen, setChosen] = useState(
    prompt.kind === "select" ? (prompt.options.find((o) => o.selected)?.value ?? prompt.options[0]?.value ?? "") : "",
  );
  const [files, setFiles] = useState<File[]>([]);
  const [fileError, setFileError] = useState<string | null>(null);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");

  const cancelAnswer = (): PromptAnswer => {
    switch (prompt.kind) {
      case "dialog":
        return { accept: false, text: "" };
      case "select":
        return { value: chosen };
      default:
        return { cancel: true };
    }
  };

  const sendFiles = async () => {
    if (files.length === 0) {
      setFileError("Choose a file first");
      return;
    }
    for (const file of files) {
      if (file.size > MAX_FILE_BYTES) {
        setFileError(`${file.name} is larger than 10 MiB`);
        return;
      }
      if (file.name.length > MAX_NAME_CHARS) {
        setFileError(`${file.name.slice(0, 32)}... has a name longer than ${MAX_NAME_CHARS} characters`);
        return;
      }
    }
    // The core caps the whole answer body at 14 MiB of base64; 10 MiB raw leaves the margin.
    if (files.reduce((sum, file) => sum + file.size, 0) > MAX_FILE_BYTES) {
      setFileError("These files are too large together (10 MiB at most)");
      return;
    }
    setFileError(null);
    try {
      const out = [];
      for (const file of files) {
        out.push({ name: file.name, mime: file.type, data_b64: toBase64(await readBytes(file)) });
      }
      onAnswer({ files: out });
    } catch {
      setFileError("The file could not be read");
    }
  };

  let title = "";
  let description: string | undefined;
  let body: ReactNode = null;
  let footer: ReactNode = null;

  if (prompt.kind === "dialog") {
    const labels = dialogLabels(prompt.dialogType);
    title = prompt.dialogType === "beforeunload" ? "Leave this page?" : "The page says";
    description = prompt.message;
    if (prompt.dialogType === "prompt") {
      body = (
        <Field label="Answer">
          <input type="text" value={text} onChange={(e) => setText(e.target.value)} disabled={pending} />
        </Field>
      );
    }
    footer = (
      <>
        {labels.no !== null && (
          <Button disabled={pending} onClick={() => onAnswer({ accept: false, text: "" })}>
            {labels.no}
          </Button>
        )}
        <Button
          variant="approve"
          disabled={pending}
          onClick={() => onAnswer({ accept: true, text: prompt.dialogType === "prompt" ? text : "" })}
        >
          {labels.yes}
        </Button>
      </>
    );
  } else if (prompt.kind === "select") {
    title = "Choose an option";
    body = (
      <div role="radiogroup" aria-label="Options">
        {prompt.options.map((option, index) => {
          const id = `live-prompt-option-${index}`;
          return (
            <div key={option.value}>
              <input
                id={id}
                type="radio"
                name="live-prompt-select"
                checked={chosen === option.value}
                disabled={pending}
                onChange={() => setChosen(option.value)}
              />{" "}
              <label htmlFor={id}>{option.label}</label>
            </div>
          );
        })}
      </div>
    );
    footer = (
      <Button variant="approve" disabled={pending} onClick={() => onAnswer({ value: chosen })}>
        Choose
      </Button>
    );
  } else if (prompt.kind === "file") {
    title = "Choose a file";
    body = (
      <>
        <Field label="File">
          <input
            type="file"
            multiple={prompt.multiple}
            accept={prompt.accept}
            disabled={pending}
            onChange={(e) => {
              setFiles(Array.from(e.target.files ?? []));
              setFileError(null);
            }}
          />
        </Field>
        {fileError !== null && <p role="alert">{fileError}</p>}
      </>
    );
    footer = (
      <>
        <Button disabled={pending} onClick={() => onAnswer({ cancel: true })}>
          Cancel
        </Button>
        <Button variant="approve" disabled={pending} onClick={() => void sendFiles()}>
          Send
        </Button>
      </>
    );
  } else {
    title = "Sign in";
    description = `${prompt.origin} asks for a username and password (${prompt.realm})`;
    body = (
      <>
        <Field label="Username">
          <input type="text" value={username} onChange={(e) => setUsername(e.target.value)} disabled={pending} />
        </Field>
        <Field label="Password">
          <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} disabled={pending} />
        </Field>
      </>
    );
    footer = (
      <>
        <Button disabled={pending} onClick={() => onAnswer({ cancel: true })}>
          Cancel
        </Button>
        <Button variant="approve" disabled={pending} onClick={() => onAnswer({ username, password })}>
          Sign in
        </Button>
      </>
    );
  }

  return (
    <Modal
      open
      onOpenChange={(open) => {
        if (!open && !pending) onAnswer(cancelAnswer());
      }}
      title={title}
      description={description}
      footer={footer}
    >
      {body}
    </Modal>
  );
}
