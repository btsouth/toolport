import { addCatalogServer } from "@/lib/api";
import type { CatalogEntry, Registry } from "@/lib/types";

/**
 * A Collection is a curated group of catalog servers added together. It is the
 * UI name for the bundles the backend still calls "stacks" (`stacks.rs`); nothing
 * is persisted, applying one just adds its catalog entries as servers.
 */

/** True when a server needs values from the user before it can run: a self-hosted
 * URL, credentials, or launch inputs. Counts the follow-up setup steps. */
export function needsSetup(entry: CatalogEntry): boolean {
  return (
    entry.credentialsUrl != null ||
    entry.envKeys.length > 0 ||
    !!entry.launch?.inputs.length
  );
}

export interface CollectionAdd {
  added: number;
  /** Of the added servers, how many still need credentials or launch values. */
  needSetup: number;
  /** Registry after the last add, or null when nothing was added. */
  registry: Registry | null;
}

/**
 * Add every server in a Collection that isn't already in Toolport, one at a time
 * so a mid-way failure keeps what already landed and the caller can reflect each
 * add. Shared by the catalog and onboarding so the add-every-server loop has one
 * implementation.
 */
export async function addCollection(
  entries: CatalogEntry[],
  existing: Set<string>,
  onAdded?: (registry: Registry) => void,
): Promise<CollectionAdd> {
  let registry: Registry | null = null;
  let added = 0;
  let needSetup = 0;
  for (const entry of entries) {
    if (existing.has(entry.name.toLowerCase())) continue;
    registry = await addCatalogServer(entry);
    added += 1;
    if (needsSetup(entry)) needSetup += 1;
    onAdded?.(registry);
  }
  return { added, needSetup, registry };
}
