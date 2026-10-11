import { lazy, Suspense } from "react";
import type { SecretsDialogProps } from "@/components/SecretsDialog";

// Credentials are opened from a server row, never at startup, so the dialog
// loads in its own chunk. Its trigger shows in the meantime.
const SecretsDialog = lazy(() =>
  import("@/components/SecretsDialog").then((m) => ({ default: m.SecretsDialog })),
);

export function LazySecretsDialog(props: SecretsDialogProps) {
  return (
    <Suspense fallback={props.trigger ?? null}>
      <SecretsDialog {...props} />
    </Suspense>
  );
}
