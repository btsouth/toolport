import { useSyncExternalStore } from "react";

/** A notice on a client's details page, such as the restart reminder after a connect. */
export type ClientNotice = { clientId: string; title: string; text: string };

/**
 * The details page's notice and the Clients list's restart reminder. A connect puts the same
 * notice in both, as GTK does; dismissing it in either place clears it in both. Other
 * feedback replaces only the details notice. Kept outside the components because the list
 * and the details page are never mounted together.
 */
type State = { detail: ClientNotice | null; list: ClientNotice | null };

let state: State = { detail: null, list: null };
const listeners = new Set<() => void>();

function update(next: State) {
  state = next;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

const getSnapshot = () => state;

export function useClientNotices(): State {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/** The notice a client's details page shows: its own feedback first, else the connect
 * reminder the Clients list still shows for it, so other feedback that later replaced the
 * details notice never hides that reminder from the client's page. */
export function noticeFor(notices: State, clientId: string): ClientNotice | null {
  if (notices.detail?.clientId === clientId) return notices.detail;
  if (notices.list?.clientId === clientId) return notices.list;
  return null;
}

/** Show feedback on the details page. Other feedback for the client the list reminder
 * names (an error, a disconnect) replaces that reminder too, as GTK's
 * `set_detail_notice` does, so the list never keeps advice the details no longer give. */
export function setDetailNotice(notice: ClientNotice | null) {
  const replaces =
    notice !== null && state.list?.clientId === notice.clientId && state.list !== notice;
  update({ detail: notice, list: replaces ? null : state.list });
}

/** After a connect: the restart reminder on the details page and on the Clients list. */
export function setConnectNotice(notice: ClientNotice) {
  update({ detail: notice, list: notice });
}

/** Dismiss a notice; the connect reminder goes from both places at once. */
export function dismissClientNotice(notice: ClientNotice) {
  update({
    detail: state.detail === notice ? null : state.detail,
    list: state.list === notice ? null : state.list,
  });
}

/** Drop the list reminder for a client, for example once it is disconnected. */
export function clearListNotice(clientId: string) {
  if (state.list?.clientId !== clientId) return;
  update({
    detail: state.detail === state.list ? null : state.detail,
    list: null,
  });
}

/** Forget details feedback when its page closes, but keep the connect reminder. */
export function releaseDetailNotice() {
  if (state.detail && state.detail !== state.list) update({ ...state, detail: null });
}

/** Tests only. */
export function resetClientNotices() {
  update({ detail: null, list: null });
}
