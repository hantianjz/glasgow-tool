#!/usr/bin/env python3
"""Build and cache the fixed native-UART resources for Glasgow revC boards."""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import importlib.resources
import json
import os
from pathlib import Path
import shutil
import tempfile
from typing import Any

import fx2.format
from glasgow.hardware.assembly import (
    HardwareInOutPipe,
    HardwareRORegister,
    HardwareRWRegister,
)

from glasgow_tool_gateware import UPSTREAM_COMMIT
from glasgow_tool_gateware.uart import assemble


REVISIONS = ("C0", "C1", "C2", "C3")
SCHEMA_VERSION = 1
PROFILE = {
    "port": "A",
    "voltage": 3.3,
    "rx": "A0",
    "tx": "A1",
    "data_bits": 8,
    "parity": "none",
    "stop_bits": 1,
    "flow_control": "none",
    "inverted": False,
}
CACHE_FILE = "build-cache.json"


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def require_private(obj: object, owner: str, name: str, expected_type: type) -> Any:
    if not hasattr(obj, name):
        raise RuntimeError(
            f"pinned Glasgow {UPSTREAM_COMMIT} changed private symbol {owner}.{name}"
        )
    value = getattr(obj, name)
    if not isinstance(value, expected_type):
        raise RuntimeError(
            f"pinned Glasgow {UPSTREAM_COMMIT} changed layout of {owner}.{name}: "
            f"expected {expected_type.__name__}, got {type(value).__name__}"
        )
    return value


def firmware_segments() -> tuple[list[dict[str, Any]], bytes]:
    resource = importlib.resources.files("glasgow.hardware").joinpath("firmware-fx2.ihex")
    try:
        source = resource.read_bytes()
        with resource.open("rt", encoding="ascii") as stream:
            parsed = fx2.format.input_data(stream, fmt="ihex")
    except Exception as error:
        raise RuntimeError(
            "pinned Glasgow "
            f"{UPSTREAM_COMMIT} changed software/glasgow/hardware/firmware.ihex "
            "(packaged as glasgow.hardware/firmware-fx2.ihex)"
        ) from error

    segments = []
    for address, data in parsed:
        if not isinstance(address, int) or not 0 <= address <= 0xFFFF:
            raise RuntimeError("FX2 firmware segment address is outside 16-bit RAM")
        payload = bytes(data)
        if address + len(payload) > 0x1_0000:
            raise RuntimeError("FX2 firmware segment exceeds 16-bit RAM")
        segments.append({"address": address, "data_hex": payload.hex()})
    if not segments:
        raise RuntimeError("pinned Glasgow FX2 firmware has no loadable segments")
    return segments, source


def parse_usb_config(segments: list[dict[str, Any]]) -> list[dict[str, Any]]:
    memory = bytearray(0x1_0000)
    present = bytearray(0x1_0000)
    for segment in segments:
        address = segment["address"]
        data = bytes.fromhex(segment["data_hex"])
        memory[address : address + len(data)] = data
        present[address : address + len(data)] = b"\x01" * len(data)

    interface_variants: set[tuple[int, int]] = set()
    endpoint_variants: set[tuple[int, int]] = set()
    for offset in range(len(memory) - 9):
        if not present[offset]:
            continue
        if (
            memory[offset] == 9
            and memory[offset + 1] == 4
            and all(present[offset : offset + 9])
            and memory[offset + 5] == 0xFF
        ):
            interface_variants.add((memory[offset + 2], memory[offset + 3]))
        if (
            memory[offset] == 7
            and memory[offset + 1] == 5
            and all(present[offset : offset + 7])
            and memory[offset + 3] & 0x03 == 2
        ):
            packet_size = int.from_bytes(memory[offset + 4 : offset + 6], "little")
            endpoint_variants.add((memory[offset + 2], packet_size))

    interface_numbers = sorted(
        interface for interface, alternate in interface_variants
        if interface != 0 and alternate == 0
    )
    endpoints = sorted(
            endpoint
            for endpoint, packet_size in endpoint_variants
            if packet_size == 512 and endpoint & 0x0F != 1
        )
    if interface_numbers != [1, 2, 3, 4] or endpoints != [0x02, 0x04, 0x86, 0x88]:
        raise RuntimeError(
            f"pinned Glasgow {UPSTREAM_COMMIT} FX2 firmware has an unexpected API-9 USB layout"
        )

    descriptors: list[dict[str, Any]] = []
    for interface, endpoint in zip(interface_numbers, endpoints, strict=True):
        for alternate in sorted(
            alternate for number, alternate in interface_variants if number == interface
        ):
            item = {
                "interface": interface,
                "alternate_setting": alternate,
                "endpoints": [],
            }
            if alternate != 0:
                item["endpoints"] = [{"endpoint": endpoint, "max_packet": 512}]
            descriptors.append(item)
    return descriptors


def pipe_metadata(assembly: object, pipe: object, descriptors: list[dict[str, Any]]) -> dict[str, Any]:
    if not isinstance(pipe, HardwareInOutPipe):
        raise RuntimeError(
            f"pinned Glasgow {UPSTREAM_COMMIT} changed HardwareAssembly.add_inout_pipe return type"
        )
    in_streams = require_private(assembly, "HardwareAssembly", "_in_streams", list)
    out_streams = require_private(assembly, "HardwareAssembly", "_out_streams", list)
    if len(in_streams) != 1 or len(out_streams) != 1:
        raise RuntimeError("fixed UART profile must allocate exactly one bidirectional pipe")

    enabled = [item for item in descriptors if item["interface"] != 0]
    interface_numbers = sorted({item["interface"] for item in enabled})
    if len(interface_numbers) % 2 != 0:
        raise RuntimeError("API-9 data interface count is not even")
    midpoint = len(interface_numbers) // 2
    out_interface = interface_numbers[0]
    in_interface = interface_numbers[midpoint]
    alternate = 2

    def select(interface: int) -> dict[str, Any]:
        matches = [
            item
            for item in enabled
            if item["interface"] == interface
            and item["alternate_setting"] == alternate
        ]
        if len(matches) != 1 or len(matches[0]["endpoints"]) != 1:
            raise RuntimeError(
                f"API-9 interface {interface} alternate {alternate} is not a single-endpoint pipe"
            )
        endpoint = matches[0]["endpoints"][0]
        if endpoint["max_packet"] != 512:
            raise RuntimeError("API-9 high-speed pipe max-packet size is not 512 bytes")
        return {
            "interface": interface,
            "alternate_setting": alternate,
            "endpoint": endpoint["endpoint"],
            "max_packet": endpoint["max_packet"],
        }

    rx = select(in_interface)
    tx = select(out_interface)
    if rx["endpoint"] & 0x80 == 0 or tx["endpoint"] & 0x80 != 0:
        raise RuntimeError("API-9 UART pipe endpoint direction mismatch")
    return {"rx": rx, "tx": tx}


def register_metadata(register: object, access: str) -> dict[str, Any]:
    expected = HardwareRWRegister if access == "rw" else HardwareRORegister
    if not isinstance(register, expected):
        raise RuntimeError(
            f"pinned Glasgow {UPSTREAM_COMMIT} changed {expected.__name__} allocation type"
        )
    address = require_private(register, expected.__name__, "_address", int)
    shape = register.shape
    width = shape.width
    storage_bytes = require_private(register, expected.__name__, "_width", int)
    if not 0 <= address <= 0x7F or not 1 <= width <= 32 or storage_bytes != (width + 7) // 8:
        raise RuntimeError("allocated register metadata is outside the API-9 register range")
    return {
        "address": address,
        "width_bits": width,
        "storage_bytes": storage_bytes,
        "endian": "little",
        "access": access,
    }


def input_digest(root: Path) -> str:
    hasher = hashlib.sha256()
    inputs = [root / "pyproject.toml", root / "uv.lock", root / "build_uart.py"]
    inputs.extend(sorted((root / "src" / "glasgow_tool_gateware").glob("*.py")))
    for path in inputs:
        hasher.update(path.relative_to(root).as_posix().encode())
        hasher.update(b"\0")
        hasher.update(path.read_bytes())
        hasher.update(b"\0")
    hasher.update(UPSTREAM_COMMIT.encode())
    hasher.update(json.dumps(PROFILE, sort_keys=True).encode())
    hasher.update(json.dumps(REVISIONS).encode())
    return hasher.hexdigest()


def cache_valid(output: Path, digest: str) -> bool:
    try:
        cache = json.loads((output / CACHE_FILE).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    if cache.get("input_sha256") != digest or set(cache.get("artifacts", {})) != {
        f"{revision}.{suffix}" for revision in REVISIONS for suffix in ("bit", "json")
    }:
        return False
    for name, expected in cache["artifacts"].items():
        try:
            actual = sha256((output / name).read_bytes())
        except OSError:
            return False
        if actual != expected:
            return False
    return True


async def build_all(destination: Path, source_digest: str) -> None:
    segments, firmware_source = firmware_segments()
    descriptors = parse_usb_config(segments)
    artifact_hashes: dict[str, str] = {}

    for revision in REVISIONS:
        assembly, resources = assemble(revision)
        plan = assembly.artifact()
        bitstream = await plan.get_bitstream()
        bitstream_name = f"{revision}.bit"
        bitstream_path = destination / bitstream_name
        bitstream_path.write_bytes(bitstream)

        manifest = {
            "schema_version": SCHEMA_VERSION,
            "upstream_commit": UPSTREAM_COMMIT,
            "revision": revision,
            "profile": PROFILE,
            "bitstream": {
                "file": bitstream_name,
                "id": plan.bitstream_id.hex(),
                "sha256": sha256(bitstream),
            },
            "registers": {
                "baud_divisor": register_metadata(resources.baud_divisor, "rw"),
                "rx_errors": register_metadata(resources.rx_errors, "ro"),
                "rx_overflow": register_metadata(resources.rx_overflow, "ro"),
                "tx_state": register_metadata(resources.tx_state, "ro"),
            },
            "pipe": pipe_metadata(assembly, resources.pipe, descriptors),
            "firmware": {
                "api_level": 9,
                "source_sha256": sha256(firmware_source),
                "segments": segments,
            },
        }
        manifest_name = f"{revision}.json"
        manifest_data = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
        (destination / manifest_name).write_bytes(manifest_data)
        artifact_hashes[bitstream_name] = sha256(bitstream)
        artifact_hashes[manifest_name] = sha256(manifest_data)

    cache = {
        "schema_version": 1,
        "input_sha256": source_digest,
        "artifacts": artifact_hashes,
    }
    (destination / CACHE_FILE).write_text(
        json.dumps(cache, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def replace_directory(staged: Path, output: Path) -> None:
    backup = output.with_name(f".{output.name}.old-{os.getpid()}")
    if backup.exists():
        shutil.rmtree(backup)
    if output.exists():
        os.replace(output, backup)
    try:
        os.replace(staged, output)
    except Exception:
        if backup.exists() and not output.exists():
            os.replace(backup, output)
        raise
    if backup.exists():
        shutil.rmtree(backup)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    root = Path(__file__).resolve().parent
    output = args.output.resolve()
    digest = input_digest(root)
    if cache_valid(output, digest):
        print(f"reusing Glasgow UART resources in {output}")
        return

    output.parent.mkdir(parents=True, exist_ok=True)
    staged = Path(tempfile.mkdtemp(prefix=f".{output.name}.tmp-", dir=output.parent))
    try:
        asyncio.run(build_all(staged, digest))
        replace_directory(staged, output)
    finally:
        if staged.exists():
            shutil.rmtree(staged)
    print(f"built Glasgow UART resources in {output}")


if __name__ == "__main__":
    main()
