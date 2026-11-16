/** One server editor row: the name its value was stored under in the
 * keychain when the dialog opened, and the row as it is now. */
export interface SecretRow {
  storedAs?: string;
  key: string;
  value: string;
  secret?: boolean;
  source?: { ref: string };
}

export interface SecretKeyChanges {
  /** Stored values the user removed, made plain with a new value, or pointed
   * at a password manager. */
  removed: string[];
  /** Keychain variables renamed without a new value: [old, new]. */
  renamed: [string, string][];
  /** Rows that would lose their stored value with nothing to replace it. */
  blocked: string[];
}

/**
 * The keychain changes a save asks for. Mirrors `editor_secret_changes` in
 * the Rust backend, which the GTK editor uses and which checks it again: a
 * stored value is never deleted unless its replacement is in hand.
 */
export function secretKeyChanges(
  vaultedAtOpen: string[],
  rows: SecretRow[],
): SecretKeyChanges {
  const changes: SecretKeyChanges = { removed: [], renamed: [], blocked: [] };
  for (const row of rows) {
    if (row.storedAs === undefined) continue;
    const old = row.storedAs.trim();
    const blank = !row.value.trim();
    const plain = row.secret === false;
    if (plain && blank) changes.blocked.push(old);
    else if (plain || row.source) changes.removed.push(old);
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

/** Whether a row with a stored value needs a value typed before saving. */
export function needsValue(row: SecretRow): boolean {
  return row.storedAs !== undefined && row.secret === false && !row.value.trim();
}
