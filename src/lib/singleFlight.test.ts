import { describe, expect, it, vi } from "vitest";
import { createSingleFlight } from "./singleFlight";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("createSingleFlight", () => {
  it("returns the authoritative in-flight result to every concurrent caller", async () => {
    const flight = createSingleFlight<string[]>();
    const first = deferred<string[]>();
    const start = vi.fn(() => first.promise);

    const a = flight.run(start);
    const b = flight.run(start);
    expect(start).toHaveBeenCalledTimes(0);

    await Promise.resolve();
    expect(start).toHaveBeenCalledTimes(1);
    first.resolve(["healthy"]);
    await expect(a).resolves.toEqual(["healthy"]);
    await expect(b).resolves.toEqual(["healthy"]);
  });

  it("shares failures and permits a fresh probe afterward", async () => {
    const flight = createSingleFlight<string[]>();
    const first = deferred<string[]>();
    const start = vi
      .fn<() => Promise<string[]>>()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce([]);

    const a = flight.run(start);
    const b = flight.run(start);
    await Promise.resolve();
    first.reject(new Error("backend unavailable"));
    await expect(a).rejects.toThrow("backend unavailable");
    await expect(b).rejects.toThrow("backend unavailable");

    await expect(flight.run(start)).resolves.toEqual([]);
    expect(start).toHaveBeenCalledTimes(2);
  });

  it("coalesces mutation-sensitive callers onto one trailing fresh run", async () => {
    const flight = createSingleFlight<string[]>();
    const beforeMutation = deferred<string[]>();
    const afterMutation = deferred<string[]>();
    const start = vi
      .fn<() => Promise<string[]>>()
      .mockReturnValueOnce(beforeMutation.promise)
      .mockReturnValueOnce(afterMutation.promise);

    const oldSnapshot = flight.run(start);
    await Promise.resolve();
    const freshA = flight.runAfterCurrent(start);
    const freshB = flight.runAfterCurrent(start);

    beforeMutation.resolve(["server-a"]);
    await expect(oldSnapshot).resolves.toEqual(["server-a"]);
    await Promise.resolve();
    expect(start).toHaveBeenCalledTimes(2);

    afterMutation.resolve(["server-a", "server-b"]);
    await expect(freshA).resolves.toEqual(["server-a", "server-b"]);
    await expect(freshB).resolves.toEqual(["server-a", "server-b"]);
  });

  it("queues another fresh run for a mutation during the trailing flight", async () => {
    const flight = createSingleFlight<string[]>();
    const first = deferred<string[]>();
    const second = deferred<string[]>();
    const third = deferred<string[]>();
    const start = vi
      .fn<() => Promise<string[]>>()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)
      .mockReturnValueOnce(third.promise);

    const initial = flight.run(start);
    await Promise.resolve();
    const afterFirstMutation = flight.runAfterCurrent(start);
    first.resolve(["server-a"]);
    await initial;
    await vi.waitFor(() => expect(start).toHaveBeenCalledTimes(2));

    const afterSecondMutation = flight.runAfterCurrent(start);
    second.resolve(["server-a", "server-b"]);
    await expect(afterFirstMutation).resolves.toEqual(["server-a", "server-b"]);
    await vi.waitFor(() => expect(start).toHaveBeenCalledTimes(3));

    third.resolve(["server-a", "server-b", "server-c"]);
    await expect(afterSecondMutation).resolves.toEqual([
      "server-a",
      "server-b",
      "server-c",
    ]);
  });
});
