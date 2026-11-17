import type { ReactNode } from "react";
import { Inbox } from "lucide-react";
import { cn } from "@/lib/utils";

/**
 * The one empty state (spec §11): a row group holding a 32px secondary icon, a 14/600 title,
 * one 13px line and the action as a secondary button. `framed={false}` drops the row group for
 * a state that already sits inside one (a dialog list, a settings group).
 */
export function EmptyState({
  icon = <Inbox />,
  title,
  description,
  action,
  framed = true,
  className,
}: {
  icon?: ReactNode;
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
  framed?: boolean;
  className?: string;
}) {
  return (
    <div
      role="status"
      data-slot="empty-state"
      className={cn(
        "flex flex-col items-center justify-center gap-3 px-6 py-10 text-center",
        framed && "rounded-xl border bg-card",
        className,
      )}
    >
      {icon && (
        <div aria-hidden="true" className="text-muted-foreground [&_svg]:size-8">
          {icon}
        </div>
      )}
      <div className="space-y-1">
        <p className="text-sm font-semibold">{title}</p>
        {description && (
          <p className="mx-auto max-w-lg text-meta text-pretty text-muted-foreground">
            {description}
          </p>
        )}
      </div>
      {action && (
        <div className="mt-1 flex flex-wrap items-center justify-center gap-2">
          {action}
        </div>
      )}
    </div>
  );
}
