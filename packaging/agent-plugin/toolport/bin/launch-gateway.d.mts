import type { ChildProcess, SpawnOptions } from "node:child_process";

interface FsOps {
  readFileSync(path: string, encoding: "utf8"): string;
  readdirSync(path: string): string[];
  statSync(path: string): { mtimeMs: number };
}

export function gatewayCandidates(options?: {
  platform?: NodeJS.Platform;
  env?: NodeJS.ProcessEnv;
  home?: string;
  fsOps?: FsOps;
  version?: string;
}): string[];

export function spawnFirst(
  binaries: string[],
  options?: {
    args?: string[];
    spawnImpl?: (
      command: string,
      args: readonly string[],
      options: SpawnOptions,
    ) => ChildProcess;
    stdio?: SpawnOptions["stdio"];
    windowsHide?: boolean;
  },
): Promise<number>;

export function validateGatewayOverride(override?: string): string | null;
