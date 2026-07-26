import { useEffect, useRef, useState, type ReactNode } from "react";
import { Button, type ButtonProps } from "./Button";

interface ConfirmButtonProps extends Omit<ButtonProps, "onClick"> {
  /** Rótulo mostrado no estado armado — a pergunta, ex.: "Discard worktree?" */
  confirmLabel: string;
  onConfirm: () => void;
  children: ReactNode;
}

const DISARM_AFTER_MS = 4000;

/**
 * Confirmação inline em dois cliques para ações irreversíveis.
 * O primeiro clique arma o botão (a pergunta substitui o rótulo);
 * o segundo confirma. Desarma sozinho se o segundo clique não vier.
 */
export function ConfirmButton({
  confirmLabel,
  onConfirm,
  children,
  className,
  ...rest
}: ConfirmButtonProps) {
  const [armed, setArmed] = useState(false);
  const timer = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    },
    [],
  );

  function handleClick() {
    if (!armed) {
      setArmed(true);
      timer.current = window.setTimeout(() => setArmed(false), DISARM_AFTER_MS);
      return;
    }
    if (timer.current !== null) window.clearTimeout(timer.current);
    setArmed(false);
    onConfirm();
  }

  const classes = [armed ? "is-armed" : null, className].filter(Boolean);
  return (
    <Button
      {...rest}
      className={classes.length > 0 ? classes.join(" ") : undefined}
      onClick={handleClick}
    >
      {armed ? confirmLabel : children}
    </Button>
  );
}
