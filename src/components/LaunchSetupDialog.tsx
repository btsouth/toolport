import { useState, type ReactNode } from "react";
import { toast } from "sonner";
import { toastError } from "@/lib/toast";
import { setLaunchInputValue, setLaunchSecret } from "@/lib/api";
import type { Registry, ServerEntry } from "@/lib/types";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

interface Props {
  server: ServerEntry;
  trigger: ReactNode;
  onSaved: (registry: Registry) => void;
  onChanged?: () => void;
}

/** Setup values are member-local even when the server definition comes from a team. */
export function LaunchSetupDialog({ server, trigger, onSaved, onChanged }: Props) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [values, setValues] = useState<Record<string, string>>({});
  const inputs = server.launch?.inputs ?? [];

  function onOpenChange(next: boolean) {
    if (next) {
      setValues(
        Object.fromEntries(
          inputs.map((input) => [input.key, input.secret ? "" : (input.value ?? "")]),
        ),
      );
    }
    if (!busy) setOpen(next);
  }

  async function save() {
    setBusy(true);
    let result: Registry | undefined;
    try {
      for (const input of inputs) {
        const value = values[input.key] ?? "";
        if (input.secret) {
          if (value) result = await setLaunchSecret(server.id, input.key, value);
        } else if (value !== (input.value ?? "")) {
          result = await setLaunchInputValue(server.id, input.key, value || null);
        }
      }
      if (result) {
        onSaved(result);
        onChanged?.();
      }
      toast.success(result ? "Launch setup saved" : "Launch setup unchanged");
      setOpen(false);
    } catch (error) {
      if (result) onSaved(result);
      toastError(`Could not save launch setup: ${error}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogTrigger asChild>{trigger}</DialogTrigger>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Launch setup for {server.name}</DialogTitle>
        </DialogHeader>
        <div className="flex flex-col gap-3 py-2">
          {inputs.map((input) => (
            <div className="flex flex-col gap-1" key={input.key}>
              <Label htmlFor={`setup-${server.id}-${input.key}`}>
                {input.label}
                {input.required ? " *" : ""}
              </Label>
              <Input
                id={`setup-${server.id}-${input.key}`}
                type={input.secret ? "password" : "text"}
                value={values[input.key] ?? ""}
                placeholder={
                  input.secret ? "Leave blank to keep any vaulted value" : input.label
                }
                onChange={(event) =>
                  setValues((current) => ({
                    ...current,
                    [input.key]: event.target.value,
                  }))
                }
              />
            </div>
          ))}
          <p className="text-xs text-muted-foreground">
            Secret values stay in Toolport's vault. This server can be enabled after its
            required setup and credentials are present.
          </p>
        </div>
        <DialogFooter>
          <Button disabled={busy} onClick={save}>
            {busy ? "Saving…" : "Save setup"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
