import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { takePendingTrayApprovals } from "@/lib/api";

/**
 * Subscribe before claiming the persisted request so a tray click cannot fall
 * into the gap between checking backend state and installing the live listener.
 */
export async function subscribeToTrayApprovals(
  openApprovals: () => void,
): Promise<UnlistenFn> {
  let handledLiveRequest = false;
  const unlisten = await listen("tray-open-approvals", () => {
    handledLiveRequest = true;
    openApprovals();
  });

  try {
    const pending = await takePendingTrayApprovals();
    if (pending && !handledLiveRequest) openApprovals();
  } catch {
    // A live listener still works against an older backend without the command.
  }

  return unlisten;
}
