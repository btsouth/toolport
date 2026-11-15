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

## How secrets are kept out of logs and messages

Every secret Toolport resolves is registered with a redactor: vaulted and inline
env values, secret arguments, URL credentials, launch inputs, stored HTTP and
OAuth credentials, and values read through a reference. The gateway registers
everything it can read without a prompt before it writes any diagnostic text.
References are registered when they are first resolved, before the server that
uses them starts.

The redactor replaces a value with `[secret:NAME]` in every file Toolport writes
in its data directory (Activity, security events, the inspector, search traces,
savings, the tool cache, the quarantine store, `gateway.log`, `daemon.log`, the
OAuth debug log) and in every message sent to a client: responses, notifications,
progress and server requests. It matches the value as written and trimmed, in
upper and lower case, NFC and NFKC, percent and form encoding, hex (in any case),
JSON escaped once or twice, base64 in four alphabets, base64 inside a longer
blob, and PEM lines. It also decodes text that uses JSON escapes, percent or
form encoding or HTML entities (numeric or named, with or without `;`), mixed
character by character, and searches every decoded level up to four deep (for
example a value percent-encoded twice inside a JSON string inside a log line).
A match at any level replaces the encoded text where it is written and leaves
everything around it as it was. A value that is itself hex or base32 matches in
any case. A JWT is matched whole, and by pieces only on its signature side: its
header is the same in every token an issuer signs.
An object key that is a registered value is replaced too, and so is a JSON
number that is a numeric secret of 6 or more digits. In error text and stderr,
an 8-character piece of a secret (12 characters or longer) is redacted as well,
and so is the piece of one at the edge of a cut.

Each Activity row for a tool call records which secrets the result carried, by
name and count (`secretsReplacedInResult`), never the values. It counts the
replacements in the result sent to the client only.

Text written in pieces is matched as one text, so a secret split over any
number of pieces of any size is found. The pieces are the string fields of one
message, a log line or a definition (however they are nested), the lines and
chunks of a stderr stream, the lines of a server's stderr tail in an error, the
text lines of a log being cleaned up, and the progress messages of one call.
What sits between the pieces (other JSON, line endings) is kept where it was.
A stream holds back only the ending that could still become a secret, at any
decoding level, which is never longer than the longest registered form. At the
end of a stream a held ending shorter than 6 characters is written as it is;
a longer one that is the start of a secret is replaced. A progress message
that ends that way is held whole and merged into the next one, or delivered
when its call ends, before the response.

If the redactor cannot be built for the configured secrets, Toolport does not
fall back to writing text as is. Every response a client gets is a valid JSON-RPC
error with its `id`, notifications become a fixed log message, log lines keep
their JSON structure with `[withheld: secret redaction unavailable]` in place of
every value, and a `toolport_status` call answers with what is wrong.

Some things are deliberately left alone:

- A value the client sent in its own request is returned as the server sent it,
  in a response or in that call's progress. The client already holds it, and
  replacing it would change what it asked for, such as a file it read. This is
  keyed on the registered value, not on how it was spelled: a value the client
  sent encoded is its own too, and another secret under the same name is not.
- Tool, prompt and resource definitions are matched by whole value only, so an
  ordinary name that shares a few characters with a secret is never renamed. A
  name or argument name that holds a secret is advertised with a marker that
  carries 16 hex digits of a keyed hash, such as
  `[secret:API_KEY~3f9a0c12d4e5b6a7]`. The hash is the same on every start for
  the same name, so integrity pins, quarantine and Activity (which all use the
  advertised name) stay stable. Two different names never share one: if two
  ever hashed alike, the second item is not advertised and the gateway log says
  so. Calls still reach the server under its real name. Descriptions and other
  text in a definition are matched as one text, like a result.
- The registry and client config backups hold inline values the way you typed
  them. They are your configuration, not logs.

Known limits:

- A secret shorter than 6 characters is matched only as a whole word. A copy
  run into other text can be written. `toolport_status` names each one (by
  server and key, never the value) so you can use a longer one. A numeric
  secret under 6 digits is not replaced where it appears as a JSON number,
  because ports and counts would be replaced too; status names those as well.
- Pieces are matched within one text only: one message, one log record, one
  stderr, one server's tail, the progress of one call. A secret split across
  two different calls, servers or log records, across an object key and a
  value, between fields under different keys with other text fields between
  them, mixed with other text at the break, encoded more than four levels deep,
  or transformed in a way not listed above (a piece on its own in base64 or
  hex, encrypted, compressed, reversed) is not matched. Output a hard kill cuts
  off while it is held is lost rather than written unredacted.
- Every Toolport process (each gateway role and both apps) passes its stderr
  through the redactor from the moment its first secret is registered (a
  process with none keeps its stderr as it is and pays nothing), so a client's
  own log of the gateway's stderr gets the same markers. That log is outside Toolport's files, so a copy written before
  a secret was configured is not rewritten. If the redactor cannot take over
  stderr, stderr is discarded instead and `toolport_status` says so.

When a stored secret is replaced or deleted, Toolport first scrubs its data
directory for the old value. If that cleanup fails, the old value stays in the
vault (never on disk) until a later start finishes it, and `toolport_status`
says so. A log file that has other hard links is never changed (that would
change the other file too): the cleanup stays unfinished and status names the
file and why. Each process also scrubs, once, for every value it knows, so copies an
older build wrote are rewritten. A scrub that is queued when the process exits
runs first, or is left marked for the next start.
