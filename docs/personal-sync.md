# Personal sync and local execution

Personal sync carries definitions and explicitly portable nonsecret values. Secrets,
reference approvals, execution consent and ambient environment consent stay on each
machine. Both shells show command, arguments, working directory, transport, URL,
launch bindings, every environment key and value, every launch input and its value,
and `inheritEnv` before enabling a held server. Secret input values are masked;
their names and references stay visible. Controls, bidi controls and invisible
Unicode are escaped. `CHANGED` marks fields that differ from the last local review.

Synced definitions and portable values cannot declare execution environment
overrides: `PATH`, `LD_PRELOAD`, `LD_LIBRARY_PATH`, `LD_AUDIT`, `DYLD_*`,
`NODE_OPTIONS`, `NODE_PATH`, `npm_config_*` (case insensitive), `PYTHONPATH`,
`PYTHONSTARTUP`, `PIP_INDEX_URL`, `PIP_EXTRA_INDEX_URL`, `UV_INDEX_URL`,
`UV_EXTRA_INDEX_URL`, `UV_DEFAULT_INDEX`, `GIT_SSH_COMMAND`, `DOCKER_HOST`,
`RUSTC_WRAPPER`, `BASH_ENV`, `ENV`, `ZDOTDIR` and `GCONV_PATH`. This refusal applies
regardless of value or reference source, including governed team imports. Keep
servers requiring these overrides local. A refused pending server remains saved
and reports its own error; other servers continue syncing. `env:` secret references
also remain local only.

Loopback and private network HTTP destinations require local review, including URL
changes. Link-local and cloud metadata destinations remain blocked by the shared
classifier. Choosing “Keep on this machine only” leaves other machines' cloud copy
intact. Removing a synced server still publishes a deletion.

When an account becomes personal, existing private local servers stay local by
default. The Sync page asks once which to include. Changing back to governance
preserves installed identities, local values and destination-specific credential
namespaces. Account status errors keep the current mode and appear in status;
they do not prevent governed configuration pulls.
