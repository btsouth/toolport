import { useEffect } from "react";
import type { ServerEntry } from "@/lib/types";
import {
  executionReviewLines,
  executionReviewFields,
  executionReviewFieldLine,
} from "@/lib/executionReview";

export default function ExecutionReview({
  server,
  onReady,
}: {
  server: ServerEntry;
  onReady: (server: ServerEntry) => void;
}) {
  useEffect(() => onReady(server), [server, onReady]);
  return (
    <div className="max-h-[60vh] space-y-2 overflow-auto break-all font-mono text-xs">
      <p>Review the highlighted changes. Enable only a setup you trust.</p>
      {executionReviewLines(server).map((line, i) => (
        <p
          key={i}
          className={
            server.syncExecutionReview && line !== "New server"
              ? "whitespace-pre-wrap rounded bg-amber-500/10 p-1 text-foreground"
              : "whitespace-pre-wrap"
          }
        >
          {line}
        </p>
      ))}
      {server.syncExecutionReview && (
        <details>
          <summary>Show full definition</summary>
          {Object.entries(executionReviewFields(server)).map(([key, value]) => (
            <p key={key} className="whitespace-pre-wrap">
              {executionReviewFieldLine(key, value)}
            </p>
          ))}
        </details>
      )}
    </div>
  );
}
