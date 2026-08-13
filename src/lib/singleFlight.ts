/** Coalesce concurrent callers onto one authoritative asynchronous result.
 * The slot clears after success or failure so the next caller starts fresh. */
export function createSingleFlight<T>() {
  let current: Promise<T> | null = null;
  let trailing: Promise<T> | null = null;

  function startFlight(start: () => Promise<T>): Promise<T> {
    const pending = Promise.resolve()
      .then(start)
      .finally(() => {
        if (current === pending) current = null;
      });
    current = pending;
    return pending;
  }

  return {
    run(start: () => Promise<T>): Promise<T> {
      if (current) return current;
      if (trailing) return trailing;
      return startFlight(start);
    },

    /** Mutation-sensitive callers need a snapshot that starts after any current
     * flight. Coalesce all of them onto one trailing run. */
    runAfterCurrent(start: () => Promise<T>): Promise<T> {
      if (trailing) return trailing;
      if (!current) return startFlight(start);
      const active = current;
      const queued = active
        .catch(() => undefined)
        .then(() => {
          // This request is no longer merely queued: it is taking its snapshot now.
          // Clear the queue marker first so a mutation during this flight can request
          // one additional coalesced run behind it.
          if (trailing === queued) trailing = null;
          return startFlight(start);
        });
      trailing = queued;
      return queued;
    },
  };
}
