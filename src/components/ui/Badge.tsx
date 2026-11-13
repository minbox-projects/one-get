import type { HTMLAttributes } from "react";

import { cn } from "../../lib/cn";

export type BadgeTone = "neutral" | "info" | "success" | "warning" | "danger";

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  tone?: BadgeTone;
}

export function Badge({ tone = "neutral", className, children, ...props }: BadgeProps) {
  return (
    <span
      {...props}
      className={cn("og-badge", `og-badge--${tone}`, className)}
    >
      {children}
    </span>
  );
}
