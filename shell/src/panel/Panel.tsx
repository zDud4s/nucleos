import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import type { KeyboardEvent } from "react";

import { ComposerBox } from "../chats/ComposerBox";
import { MessageList } from "../chats/MessageList";
import {
  clearKeep,
  collapse,
  giveBack,
  initialState,
  keep,
  pendingSay,
  reduce,
  retrying,
  say,
  takeWheel,
  type Incoming,
  type Outgoing,
  type PanelState,
} from "./protocol";

type Action = Incoming | { kind: "local"; apply: (s: PanelState) => PanelState };

function step(state: PanelState, action: Action): PanelState {
  return action.kind === "local" ? action.apply(state) : reduce(state, action);
}

export interface PanelProps {
  send: (message: Outgoing) => void;
  subscribe: (fn: (message: Incoming) => void) => () => void;
}

export function Panel({ send, subscribe }: PanelProps) {
  const [state, dispatch] = useReducer(step, undefined, initialState);
  const [text, setText] = useState("");
  const [writable, setWritable] = useState(false);
  const boxRef = useRef<HTMLTextAreaElement>(null);
  const counter = useRef(0);

  useEffect(() => subscribe((m) => dispatch(m)), [subscribe]);

  const local = useCallback(
    (apply: (s: PanelState) => PanelState) => dispatch({ kind: "local", apply }),
    [],
  );

  const submit = () => {
    const body = text.trim();
    if (!body) return;
    counter.current += 1;
    const id = `c${Date.now().toString(36)}-${counter.current}`;
    local((s) => pendingSay(s, id, body, new Date().toISOString()));
    send(say(id, body));
    setText("");
  };

  const retry = (id: string, body: string) => {
    local((s) => retrying(s, id));
    send(say(id, body));
  };

  const driving = state.mode === "agent";

  const handBack = () => {
    send(giveBack(text.trim()));
    setText("");
  };

  if (state.collapsed) {
    return (
      <button
        type="button"
        className="panel-tab"
        aria-label="Open the panel"
        onClick={() => send(collapse(false))}
      >
        {driving ? "Agent" : "You"}
      </button>
    );
  }

  const messages = state.messages.map((m) => ({
    key: m.key,
    role: m.role,
    text: m.text,
    failed: m.failed,
    onRetry: m.id ? () => retry(m.id as string, m.text) : undefined,
  }));

  // Returns false always: Escape only blurs, and the box's own Enter handling must still run.
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>): boolean => {
    if (event.key === "Escape") event.currentTarget.blur();
    return false;
  };

  return (
    <div className="panel-root">
      {driving ? (
        <div className="panel-pellicle" onClick={() => send(takeWheel())}>
          The agent is driving — Take the wheel
        </div>
      ) : null}
      <aside className="panel-strip" aria-label="Agent panel">
        <header className="panel-top">
          <span className="panel-state">{driving ? "The agent is driving" : "You are driving"}</span>
          <span className="panel-host">{state.host}</span>
          <button type="button" aria-label="Collapse" onClick={() => send(collapse(true))}>
            &rsaquo;
          </button>
        </header>
        <div className="panel-messages">
          <MessageList messages={messages} />
        </div>
        {state.askWheel !== null ? (
          <div className="panel-band" role="alert">
            <p>{state.askWheel}</p>
            <button type="button" onClick={() => send(takeWheel())}>
              Take the wheel
            </button>
          </div>
        ) : null}
        {state.askKeep !== null ? (
          <div className="panel-keep">
            <p>May the agent use these sites?</p>
            <ul>
              {state.askKeep.map((h) => (
                <li key={h}>{h}</li>
              ))}
            </ul>
            <label>
              <input
                type="checkbox"
                checked={writable}
                onChange={(e) => setWritable(e.target.checked)}
              />
              Also submit forms
            </label>
            <div className="panel-keep-actions">
              <button
                type="button"
                onClick={() => {
                  send(keep(true, writable));
                  local(clearKeep);
                }}
              >
                Yes
              </button>
              <button
                type="button"
                onClick={() => {
                  send(keep(false, false));
                  local(clearKeep);
                }}
              >
                No
              </button>
            </div>
          </div>
        ) : null}
        <ComposerBox
          text={text}
          onText={setText}
          onSubmit={submit}
          onKeyDown={onKeyDown}
          boxRef={boxRef}
          sendDisabled={text.trim() === ""}
          placeholder={driving ? "Tell the agent something" : "Leave a note for the agent"}
          actions={
            driving ? (
              <button type="button" className="panel-main" onClick={() => send(takeWheel())}>
                Take the wheel
              </button>
            ) : (
              <button type="button" className="panel-main" onClick={handBack}>
                Hand back to the agent
              </button>
            )
          }
        />
      </aside>
    </div>
  );
}
