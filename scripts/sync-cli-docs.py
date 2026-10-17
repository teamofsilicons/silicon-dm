#!/usr/bin/env python3
"""Copy the canonical guides into the CLI crate, which packages them (`dm docs`).

docs/ is organised as index.md, start/ (instructive), learn/ (informative), reference/ and
operations/. Every page opens with front matter (title, description, kind, order, related),
which this script checks before copying: the docs site and `dm docs` both rely on it.

`docs/history` (records from before DM moved to Silicon Accounts and Silicon Apps) and
`docs/migration` (the migration record) are not packaged. The pages `dm docs` embeds are the
`guide!(…)` entries of crates/cli/src/docs.rs plus docs/index.md; they must exist, or the CLI
does not compile. With `--check` nothing is written: the script fails when the packaged copies
differ from the canonical guides or a page's front matter is incomplete.
"""

import argparse
import filecmp
import re
import shutil
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
source = root / "docs"
destination = root / "crates" / "cli" / "docs"
topics_module = root / "crates" / "cli" / "src" / "docs.rs"
EXCLUDED = {"history", "migration"}
KINDS = {"instructive", "informative", "reference"}
FRONT_MATTER_KEYS = ("title", "description", "kind", "order")


def required() -> list[Path]:
    """docs/index.md and every page crates/cli/src/docs.rs embeds with guide!(topic, title, path)."""
    text = topics_module.read_text(encoding="utf-8")
    paths = re.findall(r'guide!\(\s*"[^"]+",\s*"[^"]+",\s*"([^"]+)"\s*\)', text)
    if not paths:
        raise SystemExit(f"no guide!(…) entries found in {topics_module.relative_to(root)}")
    return [Path("index.md"), *(Path(path) for path in paths)]


def front_matter(path: Path) -> tuple[dict[str, object], list[str]]:
    """The page's front matter (a small YAML subset: `key: value` and `related:` lists) and its problems."""
    text = path.read_text(encoding="utf-8")
    problems: list[str] = []
    if not text.startswith("---\n"):
        return {}, ["does not start with front matter (---)"]
    end = text.find("\n---\n", 4)
    if end < 0:
        return {}, ["front matter is not closed with ---"]
    values: dict[str, object] = {}
    current_list: list[str] | None = None
    for line in text[4:end].splitlines():
        if line.startswith("  - ") and current_list is not None:
            current_list.append(line[4:].strip())
            continue
        key, separator, value = line.partition(":")
        if not separator or not key or key != key.strip():
            problems.append(f"unreadable front matter line: {line!r}")
            continue
        value = value.strip()
        if value == "":
            current_list = []
            values[key] = current_list
            continue
        current_list = None
        if ": " in value and not (value.startswith('"') and value.endswith('"')):
            problems.append(f"{key}: a value with ': ' must be quoted (YAML would misread it)")
        values[key] = value
    for key in FRONT_MATTER_KEYS:
        if not values.get(key):
            problems.append(f"front matter lacks {key}")
    if values.get("kind") and values["kind"] not in KINDS:
        problems.append(f"kind is {values['kind']!r}; use one of {', '.join(sorted(KINDS))}")
    if values.get("order") and not str(values["order"]).isdigit():
        problems.append("order must be a whole number")
    for related in values.get("related", []) if isinstance(values.get("related"), list) else []:
        if not (source / related).is_file():
            problems.append(f"related page docs/{related} does not exist")
    if not re.search(r"^# .+$", text[end + 5 :], re.M):
        problems.append("has no # title after the front matter")
    return values, problems


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
    missing = [path for path in required() if not (source / path).is_file()]
    for path in missing:
        print(f"docs/{path} must exist: crates/cli/src/docs.rs packages it", file=sys.stderr)
    files = expected()
    invalid = False
    for relative, path in files.items():
        if relative.suffix != ".md":
            continue
        _, problems = front_matter(path)
        for problem in problems:
            invalid = True
            print(f"docs/{relative}: {problem}", file=sys.stderr)
    if missing or invalid:
        print("Fix the pages above (front matter: title, description, kind, order, related).", file=sys.stderr)
        return 1
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
