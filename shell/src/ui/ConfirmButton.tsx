import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { Button, type ButtonProps } from "./Button";

interface ConfirmButtonProps extends Omit<ButtonProps, "onClick"> {
  /** Rótulo mostrado no estado armado — a pergunta, ex.: "Discard worktree?" */
  confirmLabel: string;
  onConfirm: () => void;
  /**
   * Chamado quando o botão arma e desarma. Um pai cuja lista se reordena
   * sozinha usa isto para ficar quieto enquanto uma decisão está aberta.
   */
  onArmedChange?: (armed: boolean) => void;
  children: ReactNode;
}

const DISARM_AFTER_MS = 4000;
/**
 * Two clicks are only a confirmation if a person could have read the question
 * between them. A double-click crosses both states in ~50ms and a held Enter
 * repeats at ~30/s, so without a floor the second click is the same accident
 * as the first — and the action behind it discards a worktree or disengages
 * the kill switch.
 */
const ARM_DWELL_MS = 300;

/**
 * Confirmação inline em dois cliques para ações irreversíveis.
 * O primeiro clique arma o botão (a pergunta substitui o rótulo);
 * o segundo confirma. Desarma sozinho se o segundo clique não vier.
 */
export function ConfirmButton({
  confirmLabel,
  onConfirm,
  onArmedChange,
  children,
  className,
  onKeyDown,
  ...rest
}: ConfirmButtonProps) {
  const [armed, setArmed] = useState(false);
  const timer = useRef<number | null>(null);
  const armedAt = useRef(0);
  // Mirrors `armed` for the unmount path, which cannot read state: a caller
  // that freezes a list while this button is armed would freeze it forever if
  // the button disappeared without ever saying it had disarmed.
  const armedRef = useRef(false);
  const notifyRef = useRef(onArmedChange);
  notifyRef.current = onArmedChange;

  useEffect(
    () => () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
      if (armedRef.current) notifyRef.current?.(false);
    },
    [],
  );

  function disarm() {
    setArmed(false);
    armedRef.current = false;
    onArmedChange?.(false);
  }

  function handleClick() {
    if (!armed) {
      setArmed(true);
      armedRef.current = true;
      onArmedChange?.(true);
      armedAt.current = Date.now();
      timer.current = window.setTimeout(disarm, DISARM_AFTER_MS);
      return;
    }
    // Inside the dwell the click is discarded, NOT treated as a disarm: an
    // accidental double-click must not also consume the deliberate second
    // click the user is about to make.
    if (Date.now() - armedAt.current < ARM_DWELL_MS) return;
    if (timer.current !== null) window.clearTimeout(timer.current);
    disarm();
    onConfirm();
  }

  function handleKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    onKeyDown?.(event);
    // A held Enter on a focused button repeats keydown, and the browser
    // synthesises a click for each repeat — enough to arm and confirm from a
    // single sustained press. Cancelling the repeat stops those clicks from
    // ever being generated; the dwell alone cannot outlast a held key.
    if (event.repeat && !event.defaultPrevented) event.preventDefault();
  }

  const classes = [armed ? "is-armed" : null, className].filter(Boolean);
  return (
    <Button
      {...rest}
      className={classes.length > 0 ? classes.join(" ") : undefined}
      onClick={handleClick}
      onKeyDown={handleKeyDown}
    >
      {armed ? confirmLabel : children}
    </Button>
  );
}
