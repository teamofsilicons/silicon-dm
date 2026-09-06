#!/usr/bin/env python3
"""Copy the reviewed canonical manuals into the independently packaged CLI crate."""

from pathlib import Path
import shutil

root = Path(__file__).resolve().parent.parent
source = root / "docs"
destination = root / "crates" / "cli" / "docs"
manuals = sorted(source.rglob("*.md"))
if not (source / "client" / "runtime.md").is_file():
    raise SystemExit("docs/client/runtime.md must exist before synchronizing CLI manuals")
for manual in manuals:
    target = destination / manual.relative_to(source)
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(manual, target)
shutil.copyfile(root / "openapi.yaml", destination / "openapi.yaml")
print(f"Synchronized {len(manuals)} Markdown guides and OpenAPI into crates/cli/docs")
