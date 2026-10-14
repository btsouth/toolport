import { useI18n } from "@/lib/i18n";
import type { TeamPushPreview } from "@/lib/api";
import { cn } from "@/lib/utils";
import { teamShareAction, teamShareUploads } from "@/lib/teamShare";

/** Only the backend's display allowlist, never a registry entry or resolved credentials. */
export function TeamSharePreview({ preview }: { preview: TeamPushPreview }) {
  const { t } = useI18n();
  const showChanges = teamShareUploads(preview) || preview.selections.length === 0;
  return (
    <div className="grid max-h-[60vh] gap-3 overflow-y-auto text-left">
      <p>
        {t(
          "Other team servers, instructions and policies stay unchanged. Your personal servers remain saved, and other profiles stay unchanged. ",
        )}
      </p>
      <p>
        {t(
          "Credential values are never uploaded. Each member uses their own credentials locally. ",
        )}
      </p>
      {preview.selections.map((selection) => (
        <section key={selection.id} className="min-w-0 rounded-md border p-2">
          <div className="break-words font-medium text-foreground">
            {selection.name} · {selection.teamChange}
          </div>
          <p className="mt-1">{selection.teamDetail}</p>
          {selection.notes.map((note, i) => (
            <p key={i} className="mt-1 text-xs">
              {note}
            </p>
          ))}
          <p
            className={cn(
              "mt-1",
              selection.local.outcome === "attention"
                ? "text-destructive"
                : "text-foreground",
            )}
          >
            {selection.local.message}
          </p>
        </section>
      ))}
      {!showChanges && (
        <p>
          {teamShareAction(preview)
            ? t("Nothing new is uploaded to the team. Only this profile changes.")
            : t("Nothing to upload or switch for this selection.")}
        </p>
      )}
      {showChanges &&
        (["Added", "Changed", "Removed"] as const).map((label) => {
          const names = preview[label.toLowerCase() as "added" | "changed" | "removed"];
          return (
            <section key={label}>
              <div className="font-medium text-foreground">
                {t(label)} ({names.length})
              </div>
              {names.length === 0 ? (
                <p className="mt-1">{t("None")}</p>
              ) : label === "Removed" ? (
                <ul className="mt-1 list-disc pl-5">
                  {names.map((name, i) => (
                    <li key={i} className="break-all">
                      {name}
                    </li>
                  ))}
                </ul>
              ) : (
                <div className="mt-1 grid gap-2">
                  {preview.definitions
                    .filter((d) => d.change === label)
                    .map((d) => (
                      <details
                        key={d.id}
                        open={preview.definitions.length === 1}
                        className="min-w-0 rounded-md border p-2"
                      >
                        <summary className="cursor-pointer break-all text-foreground">
                          {d.name}{" "}
                          <span className="text-muted-foreground">· {d.transport}</span>
                        </summary>
                        <dl className="mt-2 grid gap-2">
                          {d.fields.map((field) => (
                            <div key={field.label}>
                              <dt className="text-xs font-medium">{t(field.label)}</dt>
                              <dd className="mt-0.5 whitespace-pre-wrap break-all font-mono text-xs text-foreground">
                                {field.value}
                              </dd>
                            </div>
                          ))}
                        </dl>
                      </details>
                    ))}
                </div>
              )}
            </section>
          );
        })}
      <p>
        {t(
          "If the team or your local servers change before saving, Toolport stops and asks you to review again. ",
        )}
      </p>
    </div>
  );
}
