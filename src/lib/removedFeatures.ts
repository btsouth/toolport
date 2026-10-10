// Same wording as `removed_features_message` and friends in src-tauri/src/registry.rs.
const LABELS: Record<string, string> = {
  agentRules: "Agent rules",
  agentPermissions: "Agent permissions",
  activityHooks: "Agent activity hooks",
  routines: "Routines",
  agentControl: "Agent control",
};

export const GO_BACK_TO_1X_URL =
  "https://github.com/btsouth/toolport/blob/main/docs/upgrading-to-2.md#go-back-to-124";

export function removedFeatureNames(features: string[]): string[] {
  return features.map((id) => LABELS[id] ?? id);
}

export function removedFeaturesMessage(features: string[]): string {
  const names = removedFeatureNames(features);
  const list =
    names.length === 1
      ? names[0]
      : names.length === 2
        ? `${names[0]} and ${names[1]}`
        : `${names.slice(0, -1).join(", ")}, and ${names[names.length - 1]}`;
  return `Toolport 2.0 no longer includes ${list}, which you used in 1.x. Your settings for them are saved in the exports folder, and files Toolport wrote for them were left as they were. Tell us if you need one back, or go back to 1.24.`;
}

export function removedFeaturesIssueUrl(features: string[]): string {
  const names = removedFeatureNames(features).join(", ");
  const title = `I need ${names} in Toolport 2.0`;
  const body = `Toolport 2.0 removed: ${names}.\n\nWhat I used it for:\n\n`;
  return `https://github.com/btsouth/toolport/issues/new?title=${encodeURIComponent(title)}&body=${encodeURIComponent(body)}`;
}
