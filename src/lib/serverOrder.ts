import type { ServerEntry } from "./types";

/** Health and enabled state never move a row out from under its toggle. */
export function serverNameOrder(
  a: Pick<ServerEntry, "name" | "id">,
  b: Pick<ServerEntry, "name" | "id">,
): number {
  return (
    a.name.toLowerCase().localeCompare(b.name.toLowerCase()) || a.id.localeCompare(b.id)
  );
}
