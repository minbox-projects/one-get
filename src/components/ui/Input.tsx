import { forwardRef, useId } from "react";
import type { InputHTMLAttributes } from "react";

import { cn } from "../../lib/cn";

export interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  label: string;
  error?: string;
}

export const Input = forwardRef<HTMLInputElement, InputProps>(
  (
    {
      label,
      error,
      id,
      className,
      "aria-describedby": ariaDescribedBy,
      ...props
    },
    ref,
  ) => {
    const generatedId = useId();
    const inputId = id ?? generatedId;
    const errorId = `${inputId}-error`;
    const describedBy = [ariaDescribedBy, error ? errorId : undefined]
      .filter((value): value is string => Boolean(value))
      .join(" ");

    return (
      <div className="og-field">
        <label className="og-field__label" htmlFor={inputId}>
          {label}
        </label>
        <input
          {...props}
          ref={ref}
          id={inputId}
          className={cn("og-input", error ? "og-input--invalid" : undefined, className)}
          aria-invalid={error ? true : undefined}
          aria-describedby={describedBy || undefined}
        />
        {error ? (
          <p className="og-field__error" id={errorId} role="alert">
            {error}
          </p>
        ) : null}
      </div>
    );
  },
);

Input.displayName = "Input";
