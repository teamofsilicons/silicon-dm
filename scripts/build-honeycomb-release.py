#!/usr/bin/env python3
"""Build all six native DM CLIs from macOS, populate targets/, and pack a release.

Requires Xcode, all six Rust targets, cargo-zigbuild with Zig, cargo-xwin with
LLVM/LLD, and Honeycomb. --stage-only packages previously completed builds.
The native-runner GitHub workflow remains the portable CI release path.
"""
import argparse
import os
from pathlib import Path
import platform
import runpy
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = runpy.run_path(str(ROOT / "scripts/package-release.py"))
TARGETS = PACKAGE["TARGETS"]


def run(command, env=None):
    print("+", " ".join(map(str, command)), flush=True)
    subprocess.run(command, cwd=ROOT, env=env, check=True)


def build(family, build_root, jobs, zig, llvm_bin, lld_bin):
    env = os.environ.copy()
    triples = [triple for target, triple in TARGETS.items() if target.startswith(family + "-")]
    if family == "linux":
        zig = zig or shutil.which("zig") or shutil.which("python-zig")
        if not zig:
            raise SystemExit("Linux builds require Zig on PATH or --zig /path/to/zig")
        tools = build_root / "tools"
        tools.mkdir(parents=True, exist_ok=True)
        link = tools / "zig"
        if link.is_symlink():
            link.unlink()
        elif link.exists():
            raise SystemExit(f"Refusing to replace a non-symlink tool: {link}")
        link.symlink_to(Path(zig).absolute())
        env["PATH"] = str(tools) + os.pathsep + env.get("PATH", "")
        command = ["cargo", "zigbuild"]
        triples = [triple + ".2.28" for triple in triples]
    elif family == "windows":
        command = ["cargo", "xwin", "build"]
        env["PATH"] = os.pathsep.join(str(p) for p in (llvm_bin, lld_bin) if p) + os.pathsep + env.get("PATH", "")
    else:
        command = ["cargo", "build"]
    command += ["--release", "--locked", "-p", "silicon-dm-cli", "--bin", "dm",
                "--target-dir", str(build_root / family), "--jobs", str(jobs)]
    for triple in triples:
        command += ["--target", triple]
    run(command, env)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stage-only", action="store_true")
    parser.add_argument("--build-root", type=Path, default=ROOT / "target/honeycomb")
    parser.add_argument("--honeycomb", default="honeycomb")
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--zig", help="Zig executable (python-zig is also supported)")
    parser.add_argument("--llvm-bin", type=Path, help="Directory containing clang-cl and llvm tools")
    parser.add_argument("--lld-bin", type=Path, help="Directory containing lld-link")
    args = parser.parse_args()
    build_root = args.build_root.resolve()
    if not args.stage_only:
        if platform.system() != "Darwin":
            raise SystemExit("Use the native-runner release workflow outside macOS")
        if args.jobs < 1:
            raise SystemExit("--jobs must be positive")
        for family in ("macos", "linux", "windows"):
            build(family, build_root, args.jobs, args.zig, args.llvm_bin, args.lld_bin)
    payloads = []
    for target, triple in TARGETS.items():
        name = "dm.exe" if target.startswith("windows-") else "dm"
        source = build_root / target.split("-", 1)[0] / triple / "release" / name
        if not source.is_file():
            raise SystemExit(f"Missing build for {target}: {source}")
        PACKAGE["verify_binary"](source, target)
        payloads.append((source, ROOT / "targets" / target / "bin" / name))
    # Validate all six inputs before replacing any populated target.
    for source, destination in payloads:
        if destination.is_symlink() or any(p.is_symlink() for p in destination.parents if p != ROOT):
            raise SystemExit(f"Refusing a symlink in the release target path: {destination}")
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        destination.chmod(0o755)
    run([args.honeycomb, "validate", str(ROOT)])
    run([sys.executable, str(ROOT / "scripts/package-release.py"),
         "--target-tree", str(ROOT / "targets"), "--honeycomb", args.honeycomb])


if __name__ == "__main__":
    main()
