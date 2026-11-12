import { forwardRef } from "react";
import type { ButtonHTMLAttributes } from "react";

import { cn } from "../../lib/cn";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  loading?: boolean;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(
  (
    {
      variant = "primary",
      loading = false,
      disabled,
      className,
      children,
      type = "button",
      ...props
    },
    ref,
  ) => (
    <button
      {...props}
      ref={ref}
      type={type}
      className={cn("og-button", `og-button--${variant}`, className)}
      disabled={disabled || loading}
      aria-busy={loading ? "true" : undefined}
    >
      {loading ? <span className="og-button__spinner" aria-hidden="true" /> : null}
      <span className="og-button__label">{children}</span>
    </button>
  ),
);

Button.displayName = "Button";
