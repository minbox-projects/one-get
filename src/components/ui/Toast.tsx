import type { ReactNode } from "react";

import { cn } from "../../lib/cn";

export type ToastTone = "info" | "success" | "warning" | "error";

export interface ToastProps {
  message: ReactNode;
  tone?: ToastTone;
  onDismiss?: () => void;
  dismissLabel?: string;
  className?: string;
}

export function Toast({
  message,
  tone = "info",
  onDismiss,
  dismissLabel = "Dismiss",
  className,
}: ToastProps) {
  const role = tone === "error" ? "alert" : "status";

  return (
    <div role={role} className={cn("og-toast", `og-toast--${tone}`, className)}>
      <span className="og-toast__message">{message}</span>
      {onDismiss ? (
        <button
          type="button"
          className="og-toast__dismiss"
          aria-label={dismissLabel}
          onClick={onDismiss}
        >
          <span aria-hidden="true">&times;</span>
        </button>
      ) : null}
    </div>
  );
}
