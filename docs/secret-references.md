# Password manager references

Choose **From a password manager** in server credentials, select the provider,
and enter a reference. **Test** reads it once in the desktop app environment and
shows success or a fixed error state. The MCP client launches its gateway with
its own environment variables and PATH, which may differ from the desktop.
Only the reference is saved and synced. Sign in to the same provider on
all machines before starting the gateway. A team can restrict references with
`secretSources.allowedPrefixes`; an empty list denies every reference.
Prefixes match complete path segments, so `op://Eng` cannot allow
`op://Engineering-Private`.

References received through Teams, Pro or shared setup imports require local
approval before resolution. Review shows the provider, exact reference, output
name and destination URL or command. Approval stays on that machine and is
invalidated when the reference or destination changes. Environment references
are only allowed for locally created servers; they cannot be synced or imported
from a shared setup. This includes your own personal Pro sync: an `env:` server
is **Blocked** on your other machines. Use a password manager reference instead.
Member-local password manager references survive team sync.

| Provider                   | Reference format                                                                 | Fixed CLI read                                                                                              | Official documentation                                                                                                   |
| -------------------------- | -------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| 1Password                  | `op://vault/item/field` or `op://vault/item/section/field`                       | `op read --no-newline REF`                                                                                  | [read](https://developer.1password.com/docs/cli/reference/commands/read/)                                                |
| Doppler                    | `doppler://project/config/KEY`                                                   | `doppler secrets get KEY --plain --project PROJECT --config CONFIG`                                         | [CLI](https://docs.doppler.com/docs/cli)                                                                                 |
| Infisical                  | `infisical://project-id/environment/folder/KEY`                                  | `infisical secrets get KEY --plain --silent --telemetry=false --projectId PROJECT --env ENV --path /FOLDER` | [secrets get](https://infisical.com/docs/cli/reference#secrets-get)                                                      |
| HashiCorp Vault            | `vault://mount/path#field`                                                       | `vault kv get -field=FIELD MOUNT/PATH`                                                                      | [KV get](https://developer.hashicorp.com/vault/docs/commands/kv/get)                                                     |
| Bitwarden Secrets Manager  | `bws://secret-uuid`                                                              | `bws secret get UUID --output json` (read the matching object's `value`)                                    | [Secrets Manager CLI](https://bitwarden.com/help/secrets-manager-cli/)                                                   |
| Bitwarden Password Manager | `bw://item-uuid/password`                                                        | `bw get password UUID`                                                                                      | [Password Manager CLI](https://bitwarden.com/help/cli/)                                                                  |
| Keeper Secrets Manager     | `keeper://record-uid/field/password` or `keeper://record-uid/custom_field/field` | `ksm secret notation REF`                                                                                   | [secret command](https://docs.keeper.io/keeperpam/secrets-manager/secrets-manager-command-line-interface/secret-command) |
| Dashlane                   | `dl://identifier/field`                                                          | `dcli read REF`                                                                                             | [read references](https://cli.dashlane.com/personal/secrets/read)                                                        |
| LastPass                   | `lpass://numeric-item-id/password`                                               | `lpass show --password --color=never ID`                                                                    | [lpass manual](https://lastpass.github.io/lastpass-cli/lpass.1.html)                                                     |
| Environment                | `env:VARIABLE_NAME`                                                              | Read the gateway process environment                                                                        | [environment](https://doc.rust-lang.org/std/env/fn.var.html)                                                             |

The existing Teams formats stay unchanged. Infisical's first component identifies
its project ID, not a locally selected default project. Use an explicit UUID/ID
where available. Dashlane titles can match the first duplicate; IDs avoid that.
Keeper attachment downloads and Dashlane transforms are deliberately outside the
API key reference grammar. Supported path segments may contain single interior
spaces, such as `op://Private/GitHub Token/credential`. References cannot include
outer whitespace, tabs, newlines, controls, traversal, option-like components,
query parameters, executable paths or options.
API keys must be one nonempty value without control characters.

Install each official CLI in PATH or its standard local install directory.
On Windows, Toolport uses executable CLIs, not shell command wrappers. The
Bitwarden Password Manager CLI requires an already unlocked `BW_SESSION` in
the gateway process environment. Secrets Manager requires a local
`BWS_ACCESS_TOKEN`; neither credential is part of sync. Providers use their own
local sign-in state. Toolport never runs login or writes a resolved key to its
registry, keychain, export or sync payload. Vendor-managed local caches are owned
by the vendor CLI.

Each CLI read has a 120-second deadline, allowing local biometric unlock prompts.
Terminal-only prompts need sign-in beforehand. Reads use a fixed argument vector
without a shell, from the user's home or Toolport data directory rather than a
project root. Provider stdout/stderr are bounded and never included in errors.
Missing installation, locked/sign-in state, missing entry, timeout, malformed
output and other failures have distinct states. A server resolves references
concurrently with at most four active reads. Identical in-flight references share
one result across servers and project roots. The gateway reuses successful values
for at most 15 minutes, then rereads them on the next use. A user restart or
supervisor reconnect clears that server's references, as does remote auth rejection. The first
connection shares one read across env, launch inputs and headers.
Restarting the gateway also reads them again. A later retry rereads a failed CLI
lookup. Test does not use this gateway cache.
