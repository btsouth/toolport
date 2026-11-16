import { openGatewayLog } from "@/lib/api";
import { toastError } from "@/lib/toast";
import type { ReactNode } from "react";
import { FileText, RotateCw, TriangleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

const BAR = {
  danger: "shadow-[inset_3px_0_0_var(--destructive)]",
  warning: "shadow-[inset_3px_0_0_var(--warning)]",
  success: "shadow-[inset_3px_0_0_var(--success)]",
  // Info needs no action, so it has no status bar.
  info: "",
} as const;

const TILE = {
  danger: "bg-destructive/12 text-destructive",
  warning: "bg-warning/12 text-warning",
  success: "bg-success/12 text-success",
  info: "bg-muted text-muted-foreground",
} as const;

/**
 * The card from spec §10: surface, 12px radius, 1px border, a 3px status bar, a 32px tinted
 * tile, a 14/600 title and 13px detail. Actions sit beside the title while the title keeps
 * 240px; on a card narrower than 560px they move under the detail, left-aligned.
 */
export function StatusCard({
  variant = "info",
  icon,
  title,
  detail,
  actions,
  children,
  role,
  className,
  testId,
}: {
  variant?: keyof typeof BAR;
  icon: ReactNode;
  title: ReactNode;
  detail?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
  role?: string;
  className?: string;
  testId?: string;
}) {
  return (
    <div
      role={role}
      data-testid={testId}
      data-slot="status-card"
      data-variant={variant}
      className={cn(
        "@container flex gap-3 rounded-xl border bg-card p-4 text-left",
        BAR[variant],
        className,
      )}
    >
      <div
        aria-hidden="true"
        className={cn(
          "grid size-8 shrink-0 place-items-center rounded-md [&_svg]:size-4",
          TILE[variant],
        )}
      >
        {icon}
      </div>
      <div className="grid min-w-0 flex-1 grid-cols-[minmax(0,1fr)_auto] gap-x-2 @max-[560px]:grid-cols-1">
        <p
          data-slot="status-card-title"
          className="col-start-1 row-start-1 text-sm font-semibold break-words"
        >
          {title}
        </p>
        {actions && (
          <div className="col-start-2 row-start-1 flex flex-wrap items-start gap-2 @max-[560px]:order-last @max-[560px]:col-start-1 @max-[560px]:row-start-auto @max-[560px]:mt-3">
            {actions}
          </div>
        )}
        {detail && (
          <div
            data-slot="status-card-detail"
            className="col-span-full mt-1 text-meta break-words text-muted-foreground"
          >
            {detail}
          </div>
        )}
        {children && <div className="col-span-full mt-2">{children}</div>}
      </div>
    </div>
  );
}

/**
 * The one error state (spec §11): a danger card in place of the content, with a title, the
 * cause in one line, Retry (secondary) and an optional View log (tertiary).
 */
export function ErrorCallout({
  title,
  cause,
  onRetry,
  retryLabel,
  retryAriaLabel,
  retrying = false,
  onViewLog = () => void openGatewayLog().catch((e: unknown) => toastError(String(e))),
  actions,
  children,
  className,
}: {
  title: ReactNode;
  cause?: ReactNode;
  onRetry?: () => void;
  retryLabel?: ReactNode;
  /** Accessible name for Retry when several retries share a page. */
  retryAriaLabel?: string;
  retrying?: boolean;
  onViewLog?: () => void;
  /** Extra buttons, placed after View log and Retry. */
  actions?: ReactNode;
  children?: ReactNode;
  className?: string;
}) {
  const { t } = useI18n();
  const buttons =
    onRetry || onViewLog || actions ? (
      <>
        {onViewLog && (
          <Button type="button" variant="ghost" size="sm" onClick={onViewLog}>
            <FileText /> {t("View log")}
          </Button>
        )}
        {onRetry && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => onRetry()}
            disabled={retrying}
            aria-label={retryAriaLabel}
          >
            <RotateCw className={retrying ? "animate-spin" : undefined} />
            {retryLabel ?? t("Retry")}
          </Button>
        )}
        {actions}
      </>
    ) : undefined;
  return (
    <StatusCard
      role="alert"
      variant="danger"
      icon={<TriangleAlert />}
      title={title}
      detail={cause}
      actions={buttons}
      className={className}
    >
      {children}
    </StatusCard>
  );
}
