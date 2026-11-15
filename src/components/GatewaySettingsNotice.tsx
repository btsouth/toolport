import { useI18n } from "@/lib/i18n";

/** The client settings Toolport adds to its own entry, listed before the user
 * confirms a connect or a reset. Renders nothing when it adds none. */
export function GatewaySettingsNotice({
  clientName,
  lines,
}: {
  clientName: string;
  lines: readonly string[] | undefined;
}) {
  const { t } = useI18n();
  if (!lines?.length) return null;
  return (
    <div data-slot="gateway-settings" className="rounded-lg border p-3 text-meta">
      <p>
        {t(
          "Toolport also adds these settings to its {client} entry, each only if it is not set. A setting you delete stays deleted until you connect again.",
          { client: clientName },
        )}
      </p>
      {/* A setting line has few spaces; let it break anywhere rather than overflow. */}
      <ul className="mt-2 font-mono wrap-anywhere">
        {lines.map((line) => (
          <li key={line}>{line}</li>
        ))}
      </ul>
    </div>
  );
}
