# Package a DM release

Install the released CLI with `honeycomb install 'tos>dm'`, then `dm login <slt>`.
Honeycomb handles updates. Rust clients remain ordinary Cargo dependencies.

The release workflow builds the prebuilt `dm` command on native Linux, Windows and
macOS runners for both x86_64 and aarch64. Its package job stages exactly these six
executables and `honeycomb.yaml` at the archive root. The manifest version must
match the app's root Cargo version, and tagged runs must use that version.

To package existing target builds locally:

```sh
python3 scripts/package-release.py --honeycomb /path/to/honeycomb
```

By default binaries come from `target/<rust-triple>/release/dm[.exe]`. With
`--binaries-dir DIR`, provide `DIR/<honeycomb-target>/dm[.exe]`. The script checks
native executable format and architecture, runs `honeycomb validate`, then
`honeycomb pack`, then validates the resulting archive and its exact file list.
Output is `dist/dm-<version>.tar.gz` plus archive/binary SHA-256 checksums.
Missing targets and wrong binaries fail packaging; source files and local credentials
are never substituted. See [Honeycomb package format](https://docs.honeycomb.teamofsilicons.com/).

The workflow uploads reviewable build artifacts; it does not publish or deploy.
No complete six-target archive is produced merely by committing the workflow.
