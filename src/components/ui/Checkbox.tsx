import { forwardRef, useId } from "react";
import type { InputHTMLAttributes } from "react";

import { cn } from "../../lib/cn";

export interface CheckboxProps
  extends Omit<InputHTMLAttributes<HTMLInputElement>, "type"> {
  label: string;
  error?: string;
}

export const Checkbox = forwardRef<HTMLInputElement, CheckboxProps>(
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
    const checkboxId = id ?? generatedId;
    const errorId = `${checkboxId}-error`;
    const describedBy = [ariaDescribedBy, error ? errorId : undefined]
      .filter((value): value is string => Boolean(value))
      .join(" ");

    return (
      <div className="og-checkbox-field">
        <input
          {...props}
          ref={ref}
          id={checkboxId}
          type="checkbox"
          className={cn("og-checkbox", className)}
          aria-invalid={error ? true : undefined}
          aria-describedby={describedBy || undefined}
        />
        <label className="og-checkbox__label" htmlFor={checkboxId}>
          {label}
        </label>
        {error ? (
          <p className="og-field__error" id={errorId} role="alert">
            {error}
          </p>
        ) : null}
      </div>
    );
  },
);

Checkbox.displayName = "Checkbox";
