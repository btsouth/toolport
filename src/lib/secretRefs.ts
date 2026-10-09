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
