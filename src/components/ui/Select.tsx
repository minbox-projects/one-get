import { forwardRef, useId } from "react";
import type { SelectHTMLAttributes } from "react";

import { cn } from "../../lib/cn";

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

export interface SelectProps extends SelectHTMLAttributes<HTMLSelectElement> {
  label: string;
  /** Secondary accessible label kept when the visible language differs. */
  labelAlias?: string;
  options: SelectOption[];
  error?: string;
}

export const Select = forwardRef<HTMLSelectElement, SelectProps>(
  (
    {
      label,
      labelAlias,
      options,
      error,
      id,
      className,
      "aria-describedby": ariaDescribedBy,
      ...props
    },
    ref,
  ) => {
    const generatedId = useId();
    const selectId = id ?? generatedId;
    const errorId = `${selectId}-error`;
    const describedBy = [ariaDescribedBy, error ? errorId : undefined]
      .filter((value): value is string => Boolean(value))
      .join(" ");

    return (
      <div className="og-field">
        <label className="og-field__label" htmlFor={selectId}>
          {label}
          {labelAlias ? (
            <span className="og-visually-hidden"> {labelAlias}</span>
          ) : null}
        </label>
        <select
          {...props}
          ref={ref}
          id={selectId}
          className={cn("og-select", error ? "og-select--invalid" : undefined, className)}
          aria-invalid={error ? true : undefined}
          aria-describedby={describedBy || undefined}
        >
          {options.map((option) => (
            <option
              key={option.value}
              value={option.value}
              disabled={option.disabled}
            >
              {option.label}
            </option>
          ))}
        </select>
        {error ? (
          <p className="og-field__error" id={errorId} role="alert">
            {error}
          </p>
        ) : null}
      </div>
    );
  },
);

Select.displayName = "Select";
