import { cn } from "../../lib/cn";

export interface ProgressBarProps {
  value: number | null;
  max?: number;
  label: string;
  indeterminate?: boolean;
  className?: string;
}

export function ProgressBar({
  value,
  max = 100,
  label,
  indeterminate = false,
  className,
}: ProgressBarProps) {
  const known =
    !indeterminate &&
    typeof value === "number" &&
    Number.isFinite(value) &&
    max > 0;
  const ratio = known ? Math.min(Math.max(value / max, 0), 1) : 0;

  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={max}
      aria-valuenow={known ? value : undefined}
      className={cn(
        "og-progress",
        known ? undefined : "og-progress--indeterminate",
        className,
      )}
    >
      <span
        className="og-progress__fill"
        style={known ? { width: `${ratio * 100}%` } : undefined}
        aria-hidden="true"
      />
    </div>
  );
}
