import { useI18n, translate, Trans } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { teamPairCancel, teamPairState, type TeamPairEvent } from "@/lib/api";

/** The browser-approval prompt for a Teams connection link. It follows the pairing
 * attempt: it closes as soon as pairing ends, success opens Teams, and a failure
 * replaces it with the reason. Hiding it leaves pairing running. */
export function TeamPairingDialog({ onConnected }: { onConnected: () => void }) {
  const { t } = useI18n();
  const [shown, setShown] = useState<TeamPairEvent | null>(null);
  const [cancelling, setCancelling] = useState(false);
  // The latest callback, so the subscription below runs once per mount.
  const connected = useRef(onConnected);
  useEffect(() => {
    connected.current = onConnected;
  });

  useEffect(() => {
    let active = true;
    const apply = (event: TeamPairEvent) => {
      if (!active) return;
      setCancelling(false);
      if (event.state === "connected") {
        setShown(null);
        toast.success(translate("Toolport connected to Teams."));
        connected.current();
      } else if (event.state === "cancelled") {
        setShown(null);
        toast(translate("Connection request cancelled."));
      } else {
        setShown(event);
      }
    };
    void teamPairState()
      .then((event) => {
        if (event) apply(event);
      })
      .catch(() => {});
    const unlisten = listen<TeamPairEvent>("team-pair", (event) => apply(event.payload));
    return () => {
      active = false;
      void unlisten.then((stop) => stop());
    };
  }, []);

  const hide = () => setShown(null);
  const cancel = async () => {
    setCancelling(true);
    try {
      await teamPairCancel();
    } catch {
      setCancelling(false);
    }
  };

  return (
    <Dialog open={shown !== null} onOpenChange={(open) => !open && hide()}>
      <DialogContent className="sm:max-w-md" showCloseButton={false}>
        {shown?.state === "failed" ? (
          <>
            <DialogHeader>
              <DialogTitle>{t("Connection not completed")}</DialogTitle>
              <DialogDescription>
                <Trans
                  message={
                    "{value1} Nothing was connected. Start again from the Teams website when you are ready."
                  }
                  values={{ value1: shown.message }}
                />
              </DialogDescription>
            </DialogHeader>
            <DialogFooter>
              <Button onClick={hide}>{t("Close")}</Button>
            </DialogFooter>
          </>
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>{t("Approve this device in your browser")}</DialogTitle>
              <DialogDescription>
                <Trans
                  message={
                    "Device check: {value1}. Approve only if the browser shows this same check, the intended team and your account. This request expires in five minutes. You can hide this message; Toolport finishes connecting when you approve."
                  }
                  values={{ value1: <span className="font-mono">{shown?.check}</span> }}
                />
              </DialogDescription>
            </DialogHeader>
            <DialogFooter>
              <Button variant="destructive" onClick={cancel} disabled={cancelling}>
                {cancelling ? t("Cancelling…") : t("Cancel request")}
              </Button>
              <Button onClick={hide}>{t("Hide")}</Button>
            </DialogFooter>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
