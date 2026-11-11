import * as DialogPrimitive from "@radix-ui/react-dialog";
import type { ReactNode } from "react";

import { cn } from "../../lib/cn";

export interface DialogProps {
  trigger: ReactNode;
  title: string;
  description?: string;
  children?: ReactNode;
  className?: string;
}

export function Dialog({
  trigger,
  title,
  description,
  children,
  className,
}: DialogProps) {
  return (
    <DialogPrimitive.Root>
      <DialogPrimitive.Trigger asChild>{trigger}</DialogPrimitive.Trigger>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="og-dialog__overlay" />
        <DialogPrimitive.Content className={cn("og-dialog__content", className)}>
          <DialogPrimitive.Close asChild>
            <button type="button" className="og-dialog__close" aria-label="Close">
              <span aria-hidden="true">&times;</span>
            </button>
          </DialogPrimitive.Close>
          <DialogPrimitive.Title className="og-dialog__title">
            {title}
          </DialogPrimitive.Title>
          {description ? (
            <DialogPrimitive.Description className="og-dialog__description">
              {description}
            </DialogPrimitive.Description>
          ) : null}
          {children ? <div className="og-dialog__body">{children}</div> : null}
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

Dialog.displayName = "Dialog";
