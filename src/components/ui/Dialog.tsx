import * as DialogPrimitive from "@radix-ui/react-dialog";
import { useEffect, useRef } from "react";
import type { KeyboardEvent, ReactNode } from "react";

import { cn } from "../../lib/cn";

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

export interface DialogProps {
  trigger?: ReactNode;
  title: string;
  description?: string;
  children?: ReactNode;
  className?: string;
  closeLabel?: string;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}

export function Dialog({
  trigger,
  title,
  description,
  children,
  className,
  closeLabel = "Close",
  open,
  onOpenChange,
}: DialogProps) {
  const contentRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) {
      return;
    }
    const content = contentRef.current;
    if (content === null) {
      return;
    }
    const first = content.querySelector<HTMLElement>(FOCUSABLE);
    (first ?? content).focus();
  }, [open]);

  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Tab") {
      return;
    }
    const content = contentRef.current;
    if (content === null) {
      return;
    }
    const focusable = Array.from(
      content.querySelectorAll<HTMLElement>(FOCUSABLE),
    );
    if (focusable.length === 0) {
      event.preventDefault();
      content.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement;
    if (event.shiftKey) {
      if (active === first || !content.contains(active)) {
        event.preventDefault();
        last.focus();
      }
    } else if (active === last || !content.contains(active)) {
      event.preventDefault();
      first.focus();
    }
  };

  return (
    <DialogPrimitive.Root
      open={open}
      onOpenChange={onOpenChange}
      modal={false}
    >
      {trigger ? (
        <DialogPrimitive.Trigger asChild>{trigger}</DialogPrimitive.Trigger>
      ) : null}
      <DialogPrimitive.Portal>
        <div className="og-dialog__overlay" aria-hidden="true" />
        <DialogPrimitive.Content
          ref={contentRef}
          tabIndex={-1}
          onKeyDown={handleKeyDown}
          className={cn("og-dialog__content", className)}
        >
          <DialogPrimitive.Close asChild>
            <button type="button" className="og-dialog__close" aria-label={closeLabel}>
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
