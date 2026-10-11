import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

const VARIANTS = {
  danger: "border-destructive bg-destructive/8",
  warning: "border-warning bg-warning/8",
  success: "border-success bg-success/8",
  info: "border-info bg-info/8",
} as const;

interface Props {
  variant?: keyof typeof VARIANTS;
  className?: string;
  children: ReactNode;
  /** ARIA role, e.g. "status" or "alert" for banners that should be announced. */
  role?: string;
}

/** A tinted message banner with a semantic left edge. One source of truth for the success / warning / info /
 * danger notice boxes that views used to hand-roll with inconsistent padding, radius,
 * and opacity. Colors come from the semantic accent tokens. */
export function Callout({ variant = "info", className, children, role }: Props) {
  return (
    <div
      role={role}
      className={cn("toolport-alert px-3.5 py-2.5", VARIANTS[variant], className)}
    >
      {children}
    </div>
  );
}
