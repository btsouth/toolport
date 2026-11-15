import { msg, Trans, translate, useI18n, type SourceMessage } from "@/lib/i18n";
import { useCallback, useEffect, useMemo, useState } from "react";
import { Check, CloudOff, ExternalLink, Plus, Search, ShieldCheck } from "lucide-react";
import { toast } from "sonner";
import { toastError } from "@/lib/toast";
import { openExternal } from "@/lib/openUrl";
import { addCatalogServer, popularCatalog, searchCatalog } from "@/lib/api";
import {
  catalogIdentity,
  catalogInstalledIdentities,
  installed,
} from "@/lib/catalogIdentity";
import type { CatalogEntry, CatalogSearch, Registry } from "@/lib/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SkeletonRows } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/ui/empty-state";
import { SectionHeader } from "@/components/ui/section-header";
import { ErrorCallout, StatusCard } from "@/components/ui/status-card";
import { ServerDialog } from "@/components/ServerDialog";
import { ServerLogo } from "@/components/ServerLogo";

/** Section order for the browse view; categories not listed fall to the end. */
const CATEGORY_ORDER: SourceMessage[] = [
  msg("Code & infrastructure"),
  msg("Databases"),
  msg("Search & knowledge"),
  msg("Web & automation"),
  msg("Apps & productivity"),
  msg("Local tools"),
];

interface Props {
  registry: Registry | null;
  onAdded: (registry: Registry) => void;
}

export function CatalogView({ registry, onAdded }: Props) {
  const { t } = useI18n();
  const [query, setQuery] = useState("");
  const [popular, setPopular] = useState<CatalogEntry[]>([]);
  const [results, setResults] = useState<CatalogEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [popularLoading, setPopularLoading] = useState(true);
  const [popularError, setPopularError] = useState(false);
  // A failed live search is distinct from a genuinely empty result: without this a
  // network/registry failure would render as an innocent "no results for …".
  const [searchError, setSearchError] = useState(false);
  const [registryStatus, setRegistryStatus] =
    useState<CatalogSearch["registryStatus"]>("notQueried");
  const [searchNonce, setSearchNonce] = useState(0);
  const [configEntry, setConfigEntry] = useState<CatalogEntry | null>(null);

  const have = new Set((registry?.servers ?? []).flatMap(catalogInstalledIdentities));

  const reloadPopular = useCallback(() => {
    setPopularLoading(true);
    setPopularError(false);
    popularCatalog()
      .then(setPopular)
      .catch(() => setPopularError(true))
      .finally(() => setPopularLoading(false));
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Start the catalog request on mount with its existing loading state.
    reloadPopular();
  }, [reloadPopular]);

  // Live search as you type: debounce, and ignore stale responses so a slow
  // earlier query can't overwrite a newer one.
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- Reset stale search state immediately when the query changes.
      setResults(null);
      setSearchError(false);
      setRegistryStatus("notQueried");
      setLoading(false);
      return;
    }
    setLoading(true);
    setSearchError(false);
    setRegistryStatus("notQueried");
    let cancelled = false;
    const t = setTimeout(() => {
      searchCatalog(q)
        .then((r) => {
          if (!cancelled) {
            setResults(r.entries);
            setRegistryStatus(r.registryStatus);
          }
        })
        .catch(() => {
          // Distinguish a failed search from an empty one so we can offer a retry
          // instead of implying the registry has nothing for this query.
          if (!cancelled) {
            setResults([]);
            setSearchError(true);
          }
        })
        .finally(() => {
          if (!cancelled) setLoading(false);
        });
    }, 300);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [query, searchNonce]);

  /** Returns true if the entry needs the ServerDialog (has credentials, a
   * user-supplied URL, or args the user should review). False = safe to
   * immediate-add with no configuration. */
  function needsConfig(entry: CatalogEntry): boolean {
    return (
      entry.urlHint != null ||
      entry.envKeys.length > 0 ||
      entry.args.length > 0 ||
      !!entry.launch?.inputs.length
    );
  }

  async function add(entry: CatalogEntry) {
    // Self-hosted servers or entries with credentials/args: open the dialog
    // so the user can enter their URL, paste API keys, or adjust args before
    // the server is created.
    if (needsConfig(entry)) {
      setConfigEntry(entry);
      return;
    }
    setBusy(entry.name);
    try {
      onAdded(await addCatalogServer(entry));
      toast.success(translate("Added {value0}", { value0: entry.name }), {
        description: translate("Added and turned on. Check its status under Servers."),
      });
    } catch (e) {
      toastError(
        translate("Couldn't add {value1}: {value2}", {
          value1: entry.name,
          value2: String(e),
        }),
      );
    } finally {
      setBusy(null);
    }
  }

  const shown = results ?? popular;
  const browsing = !query.trim();

  // Browse view: group the popular picks into category sections. Search results
  // stay flat (they're query-driven, including the long-tail registry).
  const byCategory = useMemo(() => {
    const groups = new Map<string, CatalogEntry[]>();
    for (const e of popular) {
      const cat = e.category || "Other";
      const arr = groups.get(cat);
      if (arr) arr.push(e);
      else groups.set(cat, [e]);
    }
    const ord = (c: string) => {
      const i = CATEGORY_ORDER.findIndex((category) => category === c);
      return i === -1 ? 999 : i;
    };
    return [...groups.entries()].sort((a, b) => ord(a[0]) - ord(b[0]));
  }, [popular]);

  const card = (entry: CatalogEntry) => (
    <CatalogCard
      key={`${entry.source}:${entry.name}:${catalogIdentity(entry)}`}
      entry={entry}
      added={installed(have, entry)}
      busy={busy === entry.name}
      onAdd={() => add(entry)}
    />
  );

  return (
    <div className="flex min-w-0 w-full flex-col gap-4">
      <div className="relative">
        <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
        <Input
          autoFocus
          value={query}
          placeholder={t(
            "Search the MCP Registry (e.g. github, postgres, figma, slack)…",
          )}
          className="h-11 pl-9 text-base"
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            // Escape leaves the search and returns to the browse list, as in
            // the GTK catalog. While an input method is composing, Escape
            // cancels the composition instead (229 is the IME key code).
            const composing = e.nativeEvent.isComposing || e.keyCode === 229;
            if (e.key === "Escape" && !composing && query !== "") setQuery("");
          }}
        />
      </div>

      <div className="flex items-center justify-between text-meta text-muted-foreground">
        <span>
          {!browsing
            ? loading
              ? translate(msg("Searching the MCP Registry…"))
              : registryStatus === "unavailable" || registryStatus === "timedOut"
                ? translate(
                    shown.length === 1
                      ? msg("{count} curated match")
                      : msg("{count} curated matches"),
                    { count: shown.length },
                  )
                : t(
                    shown.length === 1
                      ? msg("{count} result (popular picks first, then the MCP Registry)")
                      : msg(
                          "{count} results (popular picks first, then the MCP Registry)",
                        ),
                    { count: shown.length },
                  )
            : translate("Popular servers")}
        </span>
        {results !== null && shown.length > 0 && (
          <button className="hover:text-foreground" onClick={() => setQuery("")}>
            {t("Clear search")}
          </button>
        )}
      </div>

      {!loading &&
        (registryStatus === "unavailable" || registryStatus === "timedOut") && (
          <StatusCard
            role="status"
            variant="warning"
            icon={<CloudOff />}
            title={
              registryStatus === "timedOut"
                ? translate("The live MCP Registry took too long to respond.")
                : translate("The live MCP Registry is unavailable.")
            }
            detail={translate("Showing curated matches only.")}
            actions={
              <Button
                variant="outline"
                size="sm"
                onClick={() => setSearchNonce((n) => n + 1)}
              >
                {translate("Try again")}
              </Button>
            }
          />
        )}

      {shown.length === 0 ? (
        browsing && popularLoading ? (
          <SkeletonRows label={translate("Loading curated servers…")} />
        ) : browsing && popularError ? (
          <ErrorCallout
            title={t("Catalog couldn't load")}
            cause={t("Toolport couldn't load the curated picks. Try again in a moment.")}
            onRetry={reloadPopular}
            retryLabel={t("Try again")}
          />
        ) : !browsing && searchError ? (
          <ErrorCallout
            title={t("Search failed")}
            cause={t(
              "Toolport couldn't reach the MCP Registry. Check your connection, then retry.",
            )}
            onRetry={() => setSearchNonce((n) => n + 1)}
            retryLabel={t("Try again")}
          />
        ) : (
          !loading && (
            <EmptyState
              icon={<Search />}
              title={
                results !== null
                  ? registryStatus === "unavailable" || registryStatus === "timedOut"
                    ? translate('No curated matches for "{value0}"', { value0: query })
                    : translate('No catalog results for "{value0}"', { value0: query })
                  : translate("No popular servers available")
              }
              description={
                results !== null
                  ? t(
                      "Try a provider name, app name, or shorter query. You can also clear the search to browse popular servers.",
                    )
                  : t(
                      "Use search to query the MCP Registry, or try again later if the browse list stays empty.",
                    )
              }
              action={
                results !== null ? (
                  <Button variant="outline" onClick={() => setQuery("")}>
                    {t("Clear search")}
                  </Button>
                ) : (
                  <Button variant="outline" onClick={reloadPopular}>
                    {t("Try again")}
                  </Button>
                )
              }
            />
          )
        )
      ) : browsing ? (
        <div className="flex flex-col gap-6">
          {byCategory.map(([cat, entries]) => (
            <section key={cat}>
              <SectionHeader count={entries.length}>
                {CATEGORY_ORDER.find((category) => category === cat)
                  ? t(CATEGORY_ORDER.find((category) => category === cat)!)
                  : cat === "Other"
                    ? t("Other")
                    : cat}
              </SectionHeader>
              <div className="divide-y overflow-hidden rounded-xl border bg-card">
                {entries.map(card)}
              </div>
            </section>
          ))}
        </div>
      ) : (
        <div className="divide-y overflow-hidden rounded-xl border bg-card">
          {shown.map(card)}
        </div>
      )}
      {configEntry && (
        <ServerDialog
          onSaved={onAdded}
          onClose={() => setConfigEntry(null)}
          initial={{
            id: "",
            name: configEntry.name,
            transport: configEntry.transport,
            command: configEntry.command,
            args: configEntry.args,
            launch: configEntry.launch,
            env: configEntry.envKeys.map((key) => ({
              key,
              value: null,
              secret: true,
            })),
            url: configEntry.url,
            source: `catalog:${configEntry.source}`,
          }}
          existingNames={(registry?.servers ?? []).map((s) => s.name)}
          autoOpen
          urlHint={configEntry.urlHint ?? undefined}
          trigger={<span className="hidden" />}
        />
      )}
    </div>
  );
}

/** Source-tier + publisher signal. Honest provenance: where the entry came
 * from and who published it, not a cryptographic attestation. */
function Provenance({ entry }: { entry: CatalogEntry }) {
  const { t } = useI18n();
  const tier =
    entry.source === "curated"
      ? { label: t("Toolport verified"), cls: "text-success" }
      : entry.source === "registry"
        ? { label: translate("MCP Registry"), cls: "text-info" }
        : { label: t("Your pick"), cls: "text-owned" };
  return (
    <div className="flex items-center gap-1.5 text-meta text-muted-foreground">
      <ShieldCheck className={`size-3 shrink-0 ${tier.cls}`} />
      <span className={tier.cls}>{tier.label}</span>
      {entry.publisher && (
        <span className="truncate text-muted-foreground">· {entry.publisher}</span>
      )}
    </div>
  );
}

function CatalogCard({
  entry,
  added,
  busy,
  onAdd,
}: {
  entry: CatalogEntry;
  added: boolean;
  busy: boolean;
  onAdd: () => void;
}) {
  const { t } = useI18n();
  const target = entry.command
    ? [entry.command, ...entry.args].join(" ")
    : (entry.url ?? "");
  return (
    <div className="flex h-14 items-center gap-3 px-4 transition-colors hover:bg-foreground/4">
      <ServerLogo name={entry.name} transport={entry.transport} size={28} />
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        <div className="flex min-w-0 items-center gap-2">
          <span className="truncate text-sm font-semibold">{entry.name}</span>
          {entry.homepage && (
            <button
              onClick={() => openExternal(entry.homepage)}
              aria-label={t("Open docs")}
              className="shrink-0 text-muted-foreground hover:text-foreground"
            >
              <ExternalLink className="size-3" />
            </button>
          )}
          <Provenance entry={entry} />
        </div>
        <p
          className="flex min-w-0 gap-2 text-meta text-muted-foreground"
          title={`${entry.description} · ${target}`}
        >
          <span className="min-w-0 flex-1 truncate">{entry.description}</span>
          <span className="max-w-1/2 shrink-0 truncate font-mono">{target}</span>
        </p>
      </div>
      <div className="shrink-0">
        {added ? (
          <span className="inline-flex items-center gap-1 text-meta text-success">
            <Trans
              message={msg("{value1}in Toolport")}
              values={{ value1: <Check className="size-3" /> }}
            />
          </span>
        ) : (
          <Button size="sm" variant="outline" disabled={busy} onClick={onAdd}>
            <Trans message={msg("{value1}Add")} values={{ value1: <Plus /> }} />
          </Button>
        )}
      </div>
    </div>
  );
}
