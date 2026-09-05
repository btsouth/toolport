import { Globe, Terminal } from "lucide-react";
import { cn } from "@/lib/utils";
import { serverLogoKey } from "@/lib/serverLogo";

const URLS = import.meta.glob("../assets/server-logos/*.svg", {
  query: "?url&no-inline",
  import: "default",
  eager: true,
}) as Record<string, string>;

const LOGOS: Record<string, string> = Object.fromEntries(
  Object.entries(URLS).map(([path, url]) => [
    path.split("/").pop()!.replace(".svg", ""),
    url,
  ]),
);

/** A local provider mark for known servers, with a neutral transport fallback. */
export function ServerLogo({
  name,
  transport,
  size = 28,
  className,
}: {
  name: string;
  transport: string;
  size?: number;
  className?: string;
}) {
  const key = serverLogoKey(name);
  const url = key ? LOGOS[key] : null;

  return (
    <span
      aria-hidden
      className={cn(
        "inline-flex shrink-0 items-center justify-center rounded-lg border border-black/10 bg-white/90 p-1.5 text-muted-foreground",
        className,
      )}
      style={{ width: size, height: size }}
    >
      {url ? (
        <img src={url} alt="" className="size-full object-contain" />
      ) : transport === "stdio" ? (
        <Terminal className="size-full" />
      ) : (
        <Globe className="size-full" />
      )}
    </span>
  );
}
