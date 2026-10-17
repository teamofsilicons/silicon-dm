#!/usr/bin/env python3
"""Copy the canonical guides into the CLI crate, which packages them (`dm docs`).

`docs/history` (records from before DM moved to Silicon Accounts and Silicon Apps) and
`docs/migration` (the migration record) are not packaged. With `--check` nothing is written:
the script fails when the packaged copies differ from the canonical guides.
"""

import argparse
import filecmp
import shutil
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
source = root / "docs"
destination = root / "crates" / "cli" / "docs"
EXCLUDED = {"history", "migration"}
REQUIRED = [
    Path("README.md"),
    Path("cli/README.md"),
    Path("client/runtime.md"),
    # `dm docs voice` and `dm docs expressive` embed these two.
    Path("start/voice.md"),
    Path("start/expressive-writing.md"),
]


def expected() -> dict[Path, Path]:
    files = {}
    for path in sorted(source.rglob("*.md")):
        relative = path.relative_to(source)
        if relative.parts[0] in EXCLUDED:
            continue
        files[relative] = path
    files[Path("openapi.yaml")] = root / "openapi.yaml"
    return files


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="compare only; exit 1 when out of date")
    args = parser.parse_args()
    for required in REQUIRED:
        if not (source / required).is_file():
            print(f"docs/{required} must exist before synchronizing CLI guides", file=sys.stderr)
            return 1
    files = expected()
    present = (
        {path.relative_to(destination) for path in destination.rglob("*") if path.is_file()}
        if destination.exists()
        else set()
    )
    stale = sorted(present - set(files))
    differing = sorted(
        relative
        for relative, path in files.items()
        if not (destination / relative).is_file()
        or not filecmp.cmp(path, destination / relative, shallow=False)
    )
    if args.check:
        if stale or differing:
            for relative in differing:
                print(f"out of date: crates/cli/docs/{relative}", file=sys.stderr)
            for relative in stale:
                print(f"not a canonical guide: crates/cli/docs/{relative}", file=sys.stderr)
            print("Run scripts/sync-cli-docs.py to synchronize.", file=sys.stderr)
            return 1
        print(f"crates/cli/docs matches {len(files)} canonical files")
        return 0
    for relative, path in files.items():
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
    for relative in stale:
        (destination / relative).unlink()
    for directory in sorted((p for p in destination.rglob("*") if p.is_dir()), reverse=True):
        if not any(directory.iterdir()):
            directory.rmdir()
    print(
        f"Synchronized {len(files) - 1} Markdown guides and OpenAPI into crates/cli/docs"
        + (f"; removed {len(stale)} stale files" if stale else "")
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
