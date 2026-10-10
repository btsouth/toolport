import { trustedClientName, clientIdentityTooltip } from "@/lib/clientIdentity";
import { getClientSessions } from "@/lib/api";
import type { ClientActivity } from "@/lib/types";
import { fmtAgo } from "@/lib/utils";
import { useEffect, useState } from "react";
import { ChevronRight, Download, MonitorCog, Puzzle } from "lucide-react";
import { ClientLogo } from "@/components/ClientLogo";
import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/ui/empty-state";
import { SectionHeader } from "@/components/ui/section-header";
import { Skeleton } from "@/components/ui/skeleton";
import { importableServers, type DetectedClient, type Registry } from "@/lib/types";

type ClientStatus = "connected" | "ready" | "customized" | "error" | "missing";

function statusOf(client: DetectedClient): ClientStatus {
  if (client.error) return "error";
  if (client.entryState === "customized") return "customized";
  if (client.gatewayInstalled) return "connected";
  if (client.appPresent) return "ready";
  return "missing";
}

function sortClients(clients: DetectedClient[]): DetectedClient[] {
  const rank = (client: DetectedClient) => {
    switch (statusOf(client)) {
      case "connected":
        return 0;
      case "ready":
        return 1;
      case "customized":
        return 2;
      case "error":
        return 3;
      case "missing":
        return 4;
    }
  };
  return [...clients].sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
}

function StatusBadge({ status }: { status: ClientStatus }) {
  switch (status) {
    case "connected":
      return <Badge variant="success">Connected</Badge>;
    case "ready":
      return <Badge variant="info">Ready to connect</Badge>;
    case "customized":
      return <Badge variant="warning">Custom configuration</Badge>;
    case "error":
      return <Badge variant="warning">Couldn&apos;t read config</Badge>;
    case "missing":
      return <Badge variant="outline">Not installed</Badge>;
  }
}

function ClientRow({
  client,
  importCount,
  onSelect,
}: {
  client: DetectedClient;
  importCount: number;
  onSelect: () => void;
}) {
  const status = statusOf(client);
  const detail =
    status === "missing"
      ? "Install this client to configure it with Toolport"
      : client.configPath ||
        (client.usesConnectors ? "Uses account connectors" : "Config path unavailable");

  return (
    <button
      type="button"
      onClick={onSelect}
      className={`grid min-h-14 w-full grid-cols-[32px_minmax(0,1fr)_auto_16px] items-center gap-3 border-b border-border/60 px-3.5 py-2.5 text-left transition-colors last:border-b-0 hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring ${
        status === "missing" ? "opacity-60" : ""
      }`}
    >
      <ClientLogo id={client.id} name={client.name} size={32} />
      <span className="min-w-0">
        <span className="flex items-center gap-1.5">
          <span className="truncate text-sm font-medium">{client.name}</span>
          {client.usesConnectors && (
            <Puzzle
              className="size-3.5 shrink-0 text-info"
              aria-label="Uses connectors"
            />
          )}
        </span>
        <span className="block truncate font-mono text-2xs text-muted-foreground">
          {detail}
        </span>
      </span>
      <span className="flex items-center gap-2">
        {importCount > 0 && status !== "error" && status !== "missing" && (
          <Badge variant="owned">
            <Download />
            {importCount} to import
          </Badge>
        )}
        <StatusBadge status={status} />
      </span>
      <ChevronRight className="size-4 text-muted-foreground/50" aria-hidden="true" />
    </button>
  );
}

export function ClientsView({
  clients,
  registry,
  loading = false,
  onSelectClient,
}: {
  clients: DetectedClient[];
  registry: Registry | null;
  loading?: boolean;
  onSelectClient: (id: string) => void;
}) {
  const [showMissing, setShowMissing] = useState(false);
  const [showRecent, setShowRecent] = useState(false);
  const [sessions, setSessions] = useState<ClientActivity[]>([]);
  const [sessionError, setSessionError] = useState(false);
  useEffect(() => {
    let current = true;
    getClientSessions()
      .then((rows) => {
        if (current) {
          setSessions(rows);
          setSessionError(false);
        }
      })
      .catch(() => {
        if (current) setSessionError(true);
      });
    return () => {
      current = false;
    };
  }, [clients]);
  const sorted = sortClients(clients);
  const present = sorted.filter((client) => statusOf(client) !== "missing");
  const missing = sorted.filter((client) => statusOf(client) === "missing");
  const connected = present.filter((client) => statusOf(client) === "connected");
  const available = present.filter((client) => statusOf(client) !== "connected");

  if (loading && clients.length === 0) {
    return (
      <div className="flex flex-col gap-2" aria-label="Loading AI clients">
        {Array.from({ length: 4 }).map((_, i) => (
          <Skeleton key={i} className="h-14 w-full rounded-lg" />
        ))}
      </div>
    );
  }

  if (clients.length === 0 && sessions.length === 0 && !sessionError) {
    return (
      <EmptyState
        icon={<MonitorCog />}
        title="No AI clients detected"
        description="Install Claude Desktop, Cursor, VS Code, or another supported client, then refresh."
      />
    );
  }

  return (
    <div className="flex flex-col gap-5">
      {sessionError && (
        <p role="status" className="text-xs text-muted-foreground">
          Client session history could not be read.
        </p>
      )}
      {present.length === 0 && (
        <EmptyState
          icon={<MonitorCog />}
          title="No supported clients installed"
          description="Install Claude Desktop, Cursor, VS Code, or another supported client, then refresh."
          className="py-12"
        />
      )}

      {connected.length > 0 && (
        <section>
          <SectionHeader count={connected.length}>Connected to Toolport</SectionHeader>
          <div className="overflow-hidden rounded-xl border border-success/20 bg-success/5">
            {connected.map((client) => (
              <ClientRow
                key={client.id}
                client={client}
                importCount={importableServers(client, registry).length}
                onSelect={() => onSelectClient(client.id)}
              />
            ))}
          </div>
        </section>
      )}

      {available.length > 0 && (
        <section>
          <SectionHeader count={available.length}>Available to connect</SectionHeader>
          <p className="mb-2 text-xs text-muted-foreground">
            Select an installed client to connect it or review its configuration.
          </p>
          <div className="overflow-hidden rounded-xl border border-border/60 bg-card/40">
            {available.map((client) => (
              <ClientRow
                key={client.id}
                client={client}
                importCount={importableServers(client, registry).length}
                onSelect={() => onSelectClient(client.id)}
              />
            ))}
          </div>
        </section>
      )}

      {missing.length > 0 && (
        <section>
          <button
            type="button"
            onClick={() => setShowMissing((value) => !value)}
            aria-expanded={showMissing}
            className="flex w-full items-center gap-2 rounded-md px-1 py-1.5 text-left text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          >
            <ChevronRight
              className={`size-3.5 transition-transform ${showMissing ? "rotate-90" : ""}`}
              aria-hidden="true"
            />
            <span className="font-medium">Not installed</span>
            <span>{missing.length}</span>
          </button>
          {showMissing && (
            <div className="mt-2 overflow-hidden rounded-xl border border-border/60 bg-card/30">
              {missing.map((client) => (
                <ClientRow
                  key={client.id}
                  client={client}
                  importCount={0}
                  onSelect={() => onSelectClient(client.id)}
                />
              ))}
            </div>
          )}
        </section>
      )}
      {sessions.length > 0 && (
        <section aria-label="Recent client activity">
          <button
            type="button"
            onClick={() => setShowRecent((value) => !value)}
            aria-expanded={showRecent}
            className="flex w-full items-center gap-2 rounded-md px-1 py-1.5 text-left text-xs text-muted-foreground hover:text-foreground"
          >
            <ChevronRight
              className={`size-3.5 ${showRecent ? "rotate-90" : ""}`}
              aria-hidden="true"
            />
            Recent client activity <span>{sessions.length}</span>
          </button>
          {showRecent && (
            <div className="mt-2 overflow-hidden rounded-xl border border-border/60 bg-card/40">
              {sessions.map((client) => (
                <div
                  key={trustedClientName(client)}
                  className="border-b border-border/60 px-3.5 py-2.5 last:border-b-0"
                >
                  <p
                    className="truncate text-sm font-medium"
                    title={
                      clientIdentityTooltip(client) ??
                      [
                        client.clientLabel && `Reports itself as: ${client.clientLabel}`,
                        `${client.sessionCount ?? 1} recorded sessions`,
                      ]
                        .filter(Boolean)
                        .join(" · ")
                    }
                  >
                    {trustedClientName(client)}
                  </p>
                  <p
                    className="text-xs text-muted-foreground"
                    title="Calls today counts retained server calls since local midnight. Older calls may no longer be retained."
                  >
                    {client.lastActiveMs
                      ? `Last active ${fmtAgo(client.lastActiveMs)}`
                      : "Last active time unavailable"}
                    {client.callsToday != null &&
                      ` · ${client.callsToday.toLocaleString()} ${client.callsToday === 1 ? "call" : "calls"} today`}
                    {client.firstCatalogSize != null &&
                      ` · Last saw ${client.firstCatalogSize.toLocaleString()} tools`}
                  </p>
                </div>
              ))}
            </div>
          )}
        </section>
      )}
    </div>
  );
}
