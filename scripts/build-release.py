#!/usr/bin/env python3
"""Build DM CLIs on a Mac and pack one Silicon Apps archive per target.

    python3 scripts/build-release.py                         # the six targets DM ships
    python3 scripts/build-release.py --targets macos-aarch64,linux-aarch64
    python3 scripts/build-release.py --stage-only            # pack earlier builds again

The release workflow (.github/workflows/release.yml) builds every target on its
own native runner and is the normal path. This script is the local alternative:
macOS targets build with cargo, Linux targets with cargo-zigbuild against
glibc 2.28 (so every Linux the Silicons use can run them), Windows targets with
cargo-xwin. Each binary is then packed by scripts/package-apps.sh into
dist/apps/dm-<version>-<target>.tar.gz, and dist/apps/SHA256SUMS lists the
archives this run packed. It uploads and publishes nothing.

Needs Xcode command-line tools, the Rust targets you build, Zig and
cargo-zigbuild for Linux, cargo-xwin with LLVM/LLD for Windows, and
silicon-apps 0.2 (`cargo install --locked silicon-apps-cli --version 0.2.0`).
"""
import argparse
import importlib.util
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("package_apps", ROOT / "scripts" / "package_apps.py")
package_apps = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_apps)

# The six targets DM ships, with their Rust triples.
SHIPPED = {
    "linux-x86_64": "x86_64-unknown-linux-gnu",
    "linux-aarch64": "aarch64-unknown-linux-gnu",
    "windows-x86_64": "x86_64-pc-windows-msvc",
    "windows-aarch64": "aarch64-pc-windows-msvc",
    "macos-x86_64": "x86_64-apple-darwin",
    "macos-aarch64": "aarch64-apple-darwin",
}
LINUX_GLIBC = "2.28"


def run(command, env=None):
    print("+", " ".join(map(str, command)), flush=True)
    subprocess.run(command, cwd=ROOT, env=env, check=True)


def built_binary(build_root, target):
    family = target.split("-", 1)[0]
    return build_root / family / SHIPPED[target] / "release" / package_apps.binary_name(target)


def build(family, targets, build_root, jobs, zig, llvm_bin, lld_bin):
    env = os.environ.copy()
    triples = [SHIPPED[target] for target in targets if target.startswith(family + "-")]
    if not triples:
        return
    if family == "linux":
        zig = zig or shutil.which("zig") or shutil.which("python-zig")
        if not zig:
            raise SystemExit("Linux builds need Zig on PATH or --zig /path/to/zig")
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
        triples = [f"{triple}.{LINUX_GLIBC}" for triple in triples]
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
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--targets", default=",".join(SHIPPED),
                        help="comma-separated Silicon Apps targets (default: the six DM ships)")
    parser.add_argument("--stage-only", action="store_true", help="pack existing builds without building")
    parser.add_argument("--build-root", type=Path, default=ROOT / "target" / "release-builds")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist" / "apps")
    parser.add_argument("--silicon-apps", help="the silicon-apps executable (default: SILICON_APPS, then PATH)")
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--zig", help="Zig executable (python-zig is also supported)")
    parser.add_argument("--llvm-bin", type=Path, help="directory containing clang-cl and the llvm tools")
    parser.add_argument("--lld-bin", type=Path, help="directory containing lld-link")
    args = parser.parse_args()
    targets = [target.strip() for target in args.targets.split(",") if target.strip()]
    unknown = [target for target in targets if target not in SHIPPED]
    if unknown or not targets:
        raise SystemExit(f"--targets must name some of {', '.join(SHIPPED)}; unknown: {', '.join(unknown) or 'none'}")
    if args.jobs < 1:
        raise SystemExit("--jobs must be positive")
    try:
        version = package_apps.release_version()
    except package_apps.PackageError as error:
        raise SystemExit(f"build-release: {error}")
    build_root = args.build_root.resolve()
    if not args.stage_only:
        if platform.system() != "Darwin":
            raise SystemExit("Use the release workflow outside macOS")
        for family in ("macos", "linux", "windows"):
            build(family, targets, build_root, args.jobs, args.zig, args.llvm_bin, args.lld_bin)
    missing = [str(built_binary(build_root, target)) for target in targets if not built_binary(build_root, target).is_file()]
    if missing:
        raise SystemExit("Missing builds:\n  " + "\n  ".join(missing))
    output = args.output_dir.resolve()
    archives = []
    for target in targets:
        try:
            archives.append(package_apps.package(version, target, built_binary(build_root, target), output, "auto",
                                                 args.silicon_apps))
        except (package_apps.PackageError, OSError, subprocess.SubprocessError) as error:
            raise SystemExit(f"build-release: {target}: {error}")
    lines = [f"{package_apps.sha256(archive)}  {archive.name}" for archive in sorted(archives)]
    (output / "SHA256SUMS").write_text("\n".join(lines) + "\n")
    print(f"{len(archives)} archive(s) and SHA256SUMS in {output}")


if __name__ == "__main__":
    sys.exit(main())
