import type { ServerEntry } from "@/lib/types";
import {
  executionReviewLines,
  executionReviewFields,
  executionReviewFieldLine,
} from "@/lib/executionReview";

export default function ExecutionReview({ server }: { server: ServerEntry }) {
  return (
    <div className="max-h-[60vh] space-y-2 overflow-auto break-all font-mono text-xs">
      <p>Review the highlighted changes. Enable only a setup you trust.</p>
      {executionReviewLines(server).map((line, i) => (
        <p
          key={i}
          className={
            server.syncExecutionReview && line !== "New server"
              ? "rounded bg-amber-500/10 p-1 text-foreground"
              : undefined
          }
        >
          {line}
        </p>
      ))}
      {server.syncExecutionReview && (
        <details>
          <summary>Show full definition</summary>
          {Object.entries(executionReviewFields(server)).map(([key, value]) => (
            <p key={key}>{executionReviewFieldLine(key, value)}</p>
          ))}
        </details>
      )}
    </div>
  );
}
