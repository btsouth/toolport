import { useI18n } from "@/lib/i18n";
import { useState } from "react";
import { Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { toastError } from "@/lib/toast";
import { createProfile, deleteProfile, setActiveProfile } from "@/lib/api";
import type { Registry } from "@/lib/types";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

interface Props {
  registry: Registry;
  onChange: (registry: Registry) => void;
}

export function ProfileBar({ registry, onChange }: Props) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const activeId = registry.activeProfileId ?? registry.profiles[0]?.id;

  async function handleSwitch(id: string) {
    try {
      onChange(await setActiveProfile(id));
    } catch (e) {
      toastError(t("Couldn't switch profile: {value1}", { value1: String(e) }));
    }
  }

  async function handleCreate() {
    const trimmed = name.trim();
    if (!trimmed) return;
    try {
      onChange(await createProfile(trimmed));
      toast.success(t('Created profile "{value1}"', { value1: trimmed }));
      setName("");
      setOpen(false);
    } catch (e) {
      toastError(t("Couldn't create profile: {value1}", { value1: String(e) }));
    }
  }

  async function handleDelete() {
    if (registry.profiles.length <= 1 || !activeId) return;
    try {
      onChange(await deleteProfile(activeId));
    } catch (e) {
      toastError(t("Couldn't delete profile: {value1}", { value1: String(e) }));
    }
  }

  function handleOpenChange(nextOpen: boolean) {
    setOpen(nextOpen);
    if (!nextOpen) {
      setName("");
    }
  }

  return (
    <div className="flex items-center gap-1.5">
      <Select value={activeId} onValueChange={handleSwitch}>
        <SelectTrigger size="sm" className="flex-1">
          <SelectValue placeholder={t("Profile")} />
        </SelectTrigger>
        <SelectContent>
          {registry.profiles.map((p) => (
            <SelectItem key={p.id} value={p.id}>
              {p.name}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>

      <Button
        variant="ghost"
        size="icon"
        className="size-8 shrink-0"
        aria-label={t("New profile")}
        onClick={() => setOpen(true)}
      >
        <Plus className="size-4" />
      </Button>

      {registry.profiles.length > 1 && (
        <ConfirmDialog
          trigger={
            <Button
              variant="ghost"
              size="icon"
              className="size-8 shrink-0 text-muted-foreground hover:text-destructive"
              aria-label={t("Delete current profile")}
            >
              <Trash2 className="size-4" />
            </Button>
          }
          title={t('Delete "{value1}"?', {
            value1:
              registry.profiles.find((p) => p.id === activeId)?.name ?? "this profile",
          })}
          description={t(
            "This removes the profile and its server scoping. Your servers and their secrets are not deleted.",
          )}
          confirmLabel={t("Delete")}
          destructive
          onConfirm={handleDelete}
        />
      )}

      <Dialog open={open} onOpenChange={handleOpenChange}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>{t("New profile")}</DialogTitle>
          </DialogHeader>
          <p className="text-xs text-muted-foreground">
            {t(
              "A profile is a set of servers a client can see. Credentials live on each server, not the profile, so to keep separate work and personal accounts, add the server twice and put one in each profile. ",
            )}
          </p>
          <div className="flex flex-col gap-2 py-2">
            <Label htmlFor="profile-name">{t("Name")}</Label>
            <Input
              id="profile-name"
              value={name}
              autoFocus
              placeholder={t("Work")}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleCreate();
              }}
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => handleOpenChange(false)}>
              {t("Cancel ")}
            </Button>
            <Button onClick={handleCreate} disabled={!name.trim()}>
              {t("Create ")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
