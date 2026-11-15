import { toast } from "sonner";
import { Badge } from "@/components/ui/badge";
import { hostThreadChip } from "@/lib/hostAttribution";
import { translate } from "@/lib/i18n";
import { toastError } from "@/lib/toast";
import type { HostAttribution } from "@/lib/types";

/** "Thread {id}" with the full id in its tooltip, for rows a T3 Code thread made. Click,
 * Enter or Space copies the full id. It sits inside clickable rows, so its own clicks and
 * keys stop there. */
export function HostThreadChip({ row }: { row: HostAttribution }) {
  const chip = hostThreadChip(row);
  if (!chip) return null;
  const copy = () => {
    navigator.clipboard.writeText(chip.id).then(
      () => toast.success(translate("Copied thread id")),
      () => toastError(translate("Couldn't copy to clipboard")),
    );
  };
  return (
    <Badge
      asChild
      variant="off"
      className="cursor-pointer font-normal outline-none hover:bg-muted/70 focus-visible:border-transparent focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
    >
      <button
        type="button"
        title={chip.title}
        aria-label={chip.label}
        data-host-thread-chip=""
        onClick={(event) => {
          event.stopPropagation();
          copy();
        }}
        onKeyDown={(event) => event.stopPropagation()}
      >
        {chip.text}
      </button>
    </Badge>
  );
}
