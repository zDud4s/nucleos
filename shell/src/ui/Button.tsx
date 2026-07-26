import type { ButtonHTMLAttributes } from "react";

export type ButtonVariant = "approve" | "ghost" | "danger" | "danger-solid" | "link";
export type ButtonSize = "md" | "sm";
/** Semântica de hover para botões ghost: "go" afirma (verde), "stop" nega (vermelho). */
export type ButtonIntent = "go" | "stop";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  intent?: ButtonIntent;
}

export function Button({
  variant = "ghost",
  size = "md",
  intent,
  className,
  type = "button",
  ...rest
}: ButtonProps) {
  const classes = ["btn", `btn--${variant}`, `btn--${size}`];
  if (intent !== undefined) classes.push(`intent-${intent}`);
  if (className !== undefined) classes.push(className);
  return <button type={type} className={classes.join(" ")} {...rest} />;
}
