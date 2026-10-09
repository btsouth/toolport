import { useState } from "react";
import { testSecretReference } from "@/lib/api";
import { SECRET_PROVIDERS, referenceProvider } from "@/lib/secretRefs";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";

export function SecretReferenceField({ serverId, value, onChange }: {
  serverId: string; value: string; onChange: (reference: string) => void;
}) {
  const [result, setResult] = useState("");
  const [busy, setBusy] = useState(false);
  const provider = referenceProvider(value) ?? SECRET_PROVIDERS[0];
  function change(reference: string) { setResult(""); onChange(reference); }
  async function test() {
    setBusy(true); setResult("");
    try { await testSecretReference(serverId, value); setResult("Success. This machine can read the key."); }
    catch (error) {
      setResult(typeof error === "object" && error && "message" in error ? String(error.message) : "Could not test the reference. Check the provider locally and retry.");
    } finally { setBusy(false); }
  }
  return <div className="flex w-full flex-col gap-2">
    <label className="text-xs">Provider
      <select aria-label="Password manager provider" className="ml-2 rounded border bg-background p-1" value={provider.scheme} disabled={busy} onChange={(e) => change(SECRET_PROVIDERS.find((p) => p.scheme === e.target.value)!.example)}>
        {SECRET_PROVIDERS.map((p) => <option key={p.scheme} value={p.scheme}>{p.name}</option>)}
      </select>
    </label>
    <div className="flex gap-2">
      <Input aria-label="Secret reference" value={value} disabled={busy} placeholder={provider.example} autoComplete="off" onChange={(e) => change(e.target.value)} />
      <Button type="button" variant="outline" size="sm" disabled={busy || !value} onClick={() => void test()}>{busy ? "Testing…" : "Test"}</Button>
    </div>
    <p className="text-xs text-muted-foreground">Only the reference syncs. Sign in to this provider on each machine.</p>
    {result && <p role="status" className="text-xs">{result}</p>}
  </div>;
}
