#!/usr/bin/env python3
"""Create deterministic, one-binary release archives and SHA-256 checksums."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import os
import platform
import sys
import tarfile
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TOOLS = ("guart", "c232uart")
NOTICE = ROOT / "THIRD_PARTY_NOTICES.txt"


def platform_tag() -> str:
    system = {"linux": "linux", "darwin": "macos", "win32": "windows"}.get(sys.platform)
    if system is None:
        raise SystemExit(f"unsupported packaging platform: {sys.platform}")
    machine = platform.machine().lower()
    arch = {
        "amd64": "x86_64",
        "x86_64": "x86_64",
        "arm64": "aarch64",
        "aarch64": "aarch64",
    }.get(machine, machine)
    return f"{system}-{arch}"


def version() -> str:
    with (ROOT / "Cargo.toml").open("rb") as stream:
        return tomllib.load(stream)["package"]["version"]


def tar_bytes(binary: Path) -> bytes:
    output = io.BytesIO()
    with gzip.GzipFile(fileobj=output, mode="wb", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as archive:
            for source, name, mode in (
                (binary, binary.name, 0o755),
                (NOTICE, NOTICE.name, 0o644),
            ):
                data = source.read_bytes()
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = mode
                info.mtime = 0
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                archive.addfile(info, io.BytesIO(data))
    return output.getvalue()


def zip_bytes(binary: Path) -> bytes:
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for source, name, mode in (
            (binary, binary.name, 0o755),
            (NOTICE, NOTICE.name, 0o644),
        ):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = mode << 16
            archive.writestr(info, source.read_bytes())
    return output.getvalue()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--platform", default=platform_tag())
    parser.add_argument("--bin-dir", type=Path, default=ROOT / "target" / "release")
    parser.add_argument("--output", type=Path, default=ROOT / "target" / "dist")
    args = parser.parse_args()

    if not NOTICE.is_file():
        parser.error(f"missing {NOTICE}")

    args.output.mkdir(parents=True, exist_ok=True)
    checksums: list[tuple[str, str]] = []
    windows = args.platform.startswith("windows-")
    for tool in TOOLS:
        binary = args.bin_dir / (f"{tool}.exe" if windows else tool)
        if not binary.is_file():
            parser.error(f"missing release binary: {binary}")
        suffix = ".zip" if windows else ".tar.gz"
        archive = args.output / f"{tool}-{version()}-{args.platform}{suffix}"
        payload = zip_bytes(binary) if windows else tar_bytes(binary)
        archive.write_bytes(payload)
        checksums.append((hashlib.sha256(payload).hexdigest(), archive.name))

    checksum_file = args.output / "SHA256SUMS"
    checksum_file.write_text(
        "".join(f"{digest}  {name}\n" for digest, name in sorted(checksums)),
        encoding="utf-8",
        newline="\n",
    )
    for digest, name in sorted(checksums):
        print(f"{digest}  {name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
