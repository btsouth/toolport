# Password manager references

Choose **From a password manager** in server credentials, select the provider,
and enter a reference. **Test** reads it once and shows success or a fixed error
state. Only the reference is saved and synced. Sign in to the same provider on
all machines before starting Toolport. A team can restrict references with
`secretSources.allowedPrefixes`; an empty list denies every reference.

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
API key reference grammar. References cannot include whitespace, controls,
traversal, option-like components, query parameters, executable paths or options.
API keys must be one nonempty value without control characters.

Install each official CLI in PATH or its standard local install directory.
On Windows, Toolport uses executable CLIs, not shell command wrappers. The
Bitwarden Password Manager CLI requires an already unlocked `BW_SESSION` in
Toolport's local process environment. Secrets Manager requires a local
`BWS_ACCESS_TOKEN`; neither credential is part of sync. Providers use their own
local sign-in state. Toolport never runs login or writes a resolved key to its
registry, keychain, export or sync payload. Vendor-managed local caches are owned
by the vendor CLI.

Each CLI read has a 120-second deadline, allowing local biometric unlock prompts.
Terminal-only prompts need sign-in beforehand. Reads use a fixed argument vector
without a shell. Provider stdout/stderr are bounded and never included in errors.
Missing installation, locked/sign-in state, missing entry, timeout, malformed
output and other failures have distinct states. Keys resolve at connection start
and are held in transport memory; reconnecting or restarting reads them again.
