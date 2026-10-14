import { useI18n, LANGUAGES, type LanguageChoice } from "@/lib/i18n";
import { SecretsDialog } from "@/components/SecretsDialog";
import { TeamSharePreview } from "@/components/TeamSharePreview";
import { TeamPairingDialog } from "@/components/TeamPairingDialog";
import { RulesView } from "@/components/RulesView";
import { TeamsView } from "@/components/TeamsView";
import { JoinTeam } from "@/components/Onboarding";
import { Button } from "@/components/ui/button";
import { Dialog } from "@/components/ui/dialog";
import type { Registry, ServerEntry } from "@/lib/types";
import type { TeamPushPreview } from "@/lib/api";
import { fmtDollars } from "@/lib/utils";
import { emit } from "@tauri-apps/api/event";

const preview: TeamPushPreview = {
  baseVersion: 1,
  localFingerprint: "fixture-fingerprint",
  added: ["fixture-added"],
  changed: ["fixture-changed"],
  removed: ["fixture-removed"],
  definitions: [
    {
      id: "added",
      name: "fixture-added",
      transport: "stdio",
      change: "Added",
      fields: [{ label: "Command", value: "fixture-command --keep-raw" }],
    },
    {
      id: "changed",
      name: "fixture-changed",
      transport: "http",
      change: "Changed",
      fields: [{ label: "URL", value: "https://example.invalid/mcp" }],
    },
  ],
  selections: [],
};

// 製品の実コンポーネントを隔離IPC付きで操作するE2E専用入口。
export function LocalizationControls({
  server,
  registry,
}: {
  server: ServerEntry;
  registry: Registry;
}) {
  const { language, setLanguage, loading } = useI18n();
  const http: ServerEntry = {
    ...server,
    id: "fixture-http",
    name: "HTTP credential",
    transport: "http",
    command: null,
    args: [],
    env: [],
    url: "https://example.invalid/mcp",
  };
  const stdio: ServerEntry = {
    ...server,
    id: "fixture-env",
    name: "Environment credential",
    env: [{ key: "RESEND_API_KEY", value: "", secret: true }],
  };
  return (
    <main className="p-6 flex flex-col gap-6 text-foreground bg-background">
      <select
        id="fixture-language"
        aria-label="Fixture language"
        aria-busy={loading}
        value={language}
        onChange={(event) => void setLanguage(event.target.value as LanguageChoice)}
      >
        <option value="system">System</option>
        {LANGUAGES.map(({ code, label }) => (
          <option value={code} key={code}>
            {label}
          </option>
        ))}
      </select>
      <section data-testid="currency-samples">
        {[0.05, 9.5, 42.6, 999.5, 1000, 1234.56].map((value, index) => (
          <span key={index} data-testid={`currency-${index}`}>
            {fmtDollars(value)}
          </span>
        ))}
      </section>
      <div className="flex gap-3">
        <SecretsDialog
          server={http}
          onSaved={() => {}}
          trigger={<Button>HTTP credential</Button>}
        />
        <SecretsDialog
          server={stdio}
          onSaved={() => {}}
          trigger={<Button>Environment credential</Button>}
        />
      </div>
      <section data-testid="share-preview">
        <TeamSharePreview preview={preview} />
      </section>
      <TeamPairingDialog onConnected={() => {}} />
      <section data-testid="rules-state">
        <RulesView />
      </section>
      <section data-testid="teams-state">
        <Button
          onClick={() => void emit("team-servers-review", { review: 2, blocked: 1 })}
        >
          Emit team review
        </Button>
        <TeamsView registry={registry} onRegistryChange={() => {}} />
      </section>
      <section data-testid="join-state">
        <Dialog>
          <JoinTeam
            onBack={() => {}}
            onRegistryChange={() => {}}
            onClientsRefresh={() => {}}
            onFinish={() => {}}
          />
        </Dialog>
      </section>
    </main>
  );
}
