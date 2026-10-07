# Review an older HTTP client connection

Existing Shared HTTP and mcp-remote client connections keep working unchanged at
startup. New connections use stdio and put no bearer credential on their argv.

When an older Toolport connection has a literal bearer credential in its process
arguments, Clients offers **Review migration** (GTK: **Review migration to stdio**).
The confirmation explains the replacement before anything is written. Toolport
backs up the client config first, replaces only its gateway entry with the stdio
command, and keeps other native MCP entries in place. Customized gateway commands,
arguments and headers require that same explicit confirmation. Cancel leaves the
config untouched. Restart the client after confirming.

The backup is the recovery path if you need the previous connection. Review it
privately: older backups can contain the original credential. Do not share it.
