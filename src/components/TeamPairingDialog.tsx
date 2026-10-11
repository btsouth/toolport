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
        toast.success("Toolport is signed in to sync.");
        connected.current();
      } else if (event.state === "cancelled") {
        setShown(null);
        toast("Connection request cancelled.");
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
              <DialogTitle>Connection not completed</DialogTitle>
              <DialogDescription>
                {shown.message} Nothing was connected. Start again from the sync website
                when you are ready.
              </DialogDescription>
            </DialogHeader>
            <DialogFooter>
              <Button onClick={hide}>Close</Button>
            </DialogFooter>
          </>
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>Approve this device in your browser</DialogTitle>
              <DialogDescription>
                Device check: <span className="font-mono">{shown?.check}</span>. Approve
                only if the browser shows this same check, the intended setup and your
                account. This request expires in five minutes. You can hide this message;
                Toolport finishes connecting when you approve.
              </DialogDescription>
            </DialogHeader>
            <DialogFooter>
              <Button variant="destructive" onClick={cancel} disabled={cancelling}>
                {cancelling ? "Cancelling…" : "Cancel request"}
              </Button>
              <Button onClick={hide}>Hide</Button>
            </DialogFooter>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
