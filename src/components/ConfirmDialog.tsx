import { useId, useState, type ReactNode } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";

interface Props {
  /** The control that opens the confirm (rendered via DialogTrigger asChild).
   * Optional when the dialog is driven in controlled mode via `open`. */
  trigger?: ReactNode;
  title: string;
  description?: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  /** Style the confirm button as destructive (red). */
  destructive?: boolean;
  /** Runs on confirm; the dialog closes when it resolves. Errors keep it open. */
  onConfirm: () => void | Promise<void>;
  /** Controlled open state. Omit for the default trigger-driven (uncontrolled) use. */
  open?: boolean;
  /** Notified on open/close in controlled mode (and alongside internal state). */
  onOpenChange?: (open: boolean) => void;
}

/** A lightweight confirm gate for irreversible actions (remove, delete, leave).
 * Built on Dialog since the project has no alert-dialog primitive. */
export function ConfirmDialog({
  trigger,
  title,
  description,
  confirmLabel = "Confirm",
  cancelLabel = "Cancel",
  destructive = false,
  onConfirm,
  open: openProp,
  onOpenChange,
}: Props) {
  const [openState, setOpenState] = useState(false);
  const [busy, setBusy] = useState(false);
  // Rich (element) descriptions contain block elements (div/p/ul), which are
  // invalid inside Radix DialogDescription's <p> root. Render those in a plain
  // block container and wire aria-describedby by hand; string descriptions keep
  // using DialogDescription exactly as before (#692).
  const descriptionId = useId();
  const richDescription = description !== undefined && typeof description !== "string";
  // Controlled when an `open` prop is supplied (e.g. opened from a menu item),
  // otherwise self-managed by the trigger.
  const isControlled = openProp !== undefined;
  const open = isControlled ? openProp : openState;
  const setOpen = (o: boolean) => {
    if (!isControlled) setOpenState(o);
    onOpenChange?.(o);
  };

  async function handleConfirm() {
    setBusy(true);
    try {
      await onConfirm();
      setOpen(false);
    } catch {
      // Keep the dialog open on failure so the user can retry. onConfirm owns
      // surfacing the error (its handlers toast); swallow here so a rejection
      // doesn't escape as an unhandled promise rejection from the onClick.
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      {trigger && <DialogTrigger asChild>{trigger}</DialogTrigger>}
      <DialogContent
        className="sm:max-w-sm"
        onClick={(e) => e.stopPropagation()}
        aria-describedby={richDescription ? descriptionId : undefined}
      >
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {description &&
            (richDescription ? (
              <div
                id={descriptionId}
                className="text-sm text-muted-foreground *:[a]:underline *:[a]:underline-offset-3 *:[a]:hover:text-foreground"
              >
                {description}
              </div>
            ) : (
              <DialogDescription>{description}</DialogDescription>
            ))}
        </DialogHeader>
        <DialogFooter>
          <Button variant="ghost" onClick={() => setOpen(false)} disabled={busy}>
            {cancelLabel}
          </Button>
          <Button
            variant={destructive ? "destructive" : "default"}
            onClick={handleConfirm}
            disabled={busy}
          >
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
