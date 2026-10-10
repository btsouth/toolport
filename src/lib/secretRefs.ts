export const SECRET_PROVIDERS = [
  { scheme: "op://", name: "1Password", example: "op://Engineering/Docs/key" },
  { scheme: "doppler://", name: "Doppler", example: "doppler://docs/prod/TOKEN" },
  {
    scheme: "infisical://",
    name: "Infisical",
    example: "infisical://docs/prod/services/TOKEN",
  },
  { scheme: "vault://", name: "HashiCorp Vault", example: "vault://secret/docs#token" },
  {
    scheme: "bws://",
    name: "Bitwarden Secrets Manager",
    example: "bws://be8e0ad8-d545-4017-a55a-b02f014d4158",
  },
  {
    scheme: "bw://",
    name: "Bitwarden Password Manager",
    example: "bw://be8e0ad8-d545-4017-a55a-b02f014d4158/password",
  },
  {
    scheme: "keeper://",
    name: "Keeper Secrets Manager",
    example: "keeper://8f8I-OqPV58o2r91wVgZ_A/field/password",
  },
  {
    scheme: "dl://",
    name: "Dashlane",
    example: "dl://QD145B53-B987-4CFE-9408-F25803DC47A4/password",
  },
  { scheme: "lpass://", name: "LastPass", example: "lpass://123456789/password" },
  { scheme: "env:", name: "Environment variable", example: "env:API_TOKEN" },
];
export function referenceProvider(reference?: string) {
  return SECRET_PROVIDERS.find((p) => reference?.startsWith(p.scheme));
}

export function secretReferenceReview(server: import("./types").ServerEntry): string[] {
  const destination = server.command
    ? [server.command, ...server.args].join(" ")
    : server.url || "unknown destination";
  const uses = [
    ...server.env.map((e) => ({ field: `env:${e.key}`, ref: e.source?.ref })),
    ...(server.launch?.inputs ?? []).map((i) => ({
      field: `input:${i.key}`,
      ref: i.source?.ref,
    })),
    ...(server.headerKeys ?? []).map((h) => ({
      field: `header:${h.key}`,
      ref: h.source?.ref ?? server.env.find((e) => e.key === h.env)?.source?.ref,
    })),
  ];
  if (!server.command && server.transport !== "stdio" && !server.headerKeys?.length) {
    const bearer = server.env.find((e) => e.secret && e.source);
    if (bearer)
      uses.push({
        field: `header:Authorization (env:${bearer.key})`,
        ref: bearer.source?.ref,
      });
  }
  return uses
    .filter((u) => u.ref)
    .map(
      (u) =>
        `${referenceProvider(u.ref)?.name ?? "Password manager"} entry ${JSON.stringify(u.ref)} will be sent to ${destination} (${u.field})`,
    );
}
