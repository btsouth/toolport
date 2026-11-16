/** One server editor row: the name its value was stored under in the
 * keychain when the dialog opened, and the row as it is now. */
export interface SecretRow {
  storedAs?: string;
  key: string;
  value: string;
  secret?: boolean;
  source?: { ref: string };
}

export const KEEP_IN_KEYCHAIN = "Enter a value, or leave it in the keychain.";
export const NAME_THIS_VARIABLE = "Give this variable a name, or remove it.";
export const NAME_USED_BY_ARGUMENT =
  "This name is already used by a secret argument. Choose another name.";

export interface SecretKeyChanges {
  /** Stored values the user removed, made plain with a new value, or pointed
   * at a password manager. */
  removed: string[];
  /** Keychain variables renamed without a new value: [old, new]. */
  renamed: [string, string][];
  /** Rows that cannot be saved as they are: [stored name, message]. */
  blocked: [string, string][];
}

/**
 * Why a row with a stored value cannot be saved as it is, or null. Mirrors
 * `editor_secret_changes` in the Rust backend: a stored value is never
 * dropped without its replacement in hand.
 */
export function rowProblem(
  row: SecretRow,
  launchSecretKeys: string[] = [],
): string | null {
  if (row.storedAs === undefined) return null;
  const old = row.storedAs.trim();
  const key = row.key.trim();
  if (!key) return NAME_THIS_VARIABLE;
  if (row.secret === false && !row.value.trim()) return KEEP_IN_KEYCHAIN;
  const keychain = row.secret !== false && !row.source;
  if (keychain && key !== old && launchSecretKeys.some((k) => k.trim() === key)) {
    return NAME_USED_BY_ARGUMENT;
  }
  return null;
}

/**
 * The keychain changes a save asks for. Mirrors `editor_secret_changes` in
 * the Rust backend, which the GTK editor uses and which checks it again.
 */
export function secretKeyChanges(
  vaultedAtOpen: string[],
  rows: SecretRow[],
  launchSecretKeys: string[] = [],
): SecretKeyChanges {
  const changes: SecretKeyChanges = { removed: [], renamed: [], blocked: [] };
  for (const row of rows) {
    if (row.storedAs === undefined) continue;
    const old = row.storedAs.trim();
    const problem = rowProblem(row, launchSecretKeys);
    if (problem) {
      changes.blocked.push([old, problem]);
      continue;
    }
    const blank = !row.value.trim();
    if (row.secret === false || row.source) changes.removed.push(old);
    else if (row.key.trim() !== old) {
      if (blank) changes.renamed.push([old, row.key.trim()]);
      else changes.removed.push(old);
    }
  }
  for (const key of vaultedAtOpen) {
    if (!rows.some((row) => row.storedAs?.trim() === key.trim())) {
      changes.removed.push(key.trim());
    }
  }
  return changes;
}
