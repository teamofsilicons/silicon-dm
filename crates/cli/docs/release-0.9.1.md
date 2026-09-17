# DM 0.9.1

The CLI can initialize and update its local state on Windows without `Access is denied`. Configuration files are synced and closed before replacement; directory syncing is restricted to Unix. Regression coverage includes a fresh home, replacing saved configuration, and replacing an existing home-directory pointer.

This is a client/CLI patch using the 0.9.0 API and WebSocket contracts. The production backend remains 0.9.0. The release package contains native Windows, Linux, and macOS executables for x86_64 and aarch64.
