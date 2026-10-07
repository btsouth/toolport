# Catalog package pins

Curated catalog executables use exact versions. Adding a server copies that
version into its native npx/uvx command. Existing servers, live registry entries
and user-added custom commands retain their launch behavior.

Run `node scripts/refresh-catalog-pins.mjs` to refresh the curated pins and launch
arguments. The script reads npm/PyPI metadata without executing packages. Review
the version, artifact URL and integrity diff in `src-tauri/catalog-pins.json`, run
the catalog tests, then commit it. A changed artifact under an unchanged version
makes the refresh fail. Pins change only through this reviewed update, not at runtime.

npm verifies registry-provided artifact integrity during installation. The separately
reviewed SHA-512 and SHA-256 digests are recorded for update review; npx/uvx do not
accept a separate integrity lock on their native command line. In particular, uvx
does not enforce wheel URL hash fragments. The pins prevent a newer publish from
running automatically, but do not defend against a compromised registry serving
changed bytes for an existing version. Upstream dependency ranges remain governed
by the upstream package and package manager. Remote HTTP endpoints can also change
independently. Tool-definition integrity checks still apply when servers connect.
