#!/usr/bin/env python3
"""Byte-exact physical acceptance harness for the real guart/c232uart binaries."""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeout
import hashlib
import json
import math
import os
from pathlib import Path
import random
import subprocess
import time
from typing import Any


def corpus(payload_size: int) -> list[tuple[str, bytes]]:
    rng = random.Random(0x4755_4152_54)
    crossing = rng.randbytes(64 * 1024 + 513)
    sustained = rng.randbytes(payload_size)
    return [
        ("empty", b""),
        ("every_byte", bytes(range(256))),
        ("cr_lf_nul", (b"\x00\r\n\r\x00\n" * 1024)),
        ("boundary_crossing", crossing),
        ("sustained", sustained),
    ]


def opposite(payload: bytes) -> bytes:
    return bytes(value ^ 0xA5 for value in reversed(payload))


def final_event(path: Path) -> dict[str, Any] | None:
    try:
        records = [json.loads(line) for line in path.read_text().splitlines() if line]
    except (OSError, ValueError):
        return None
    return records[-1] if records else None


def within_two_percent(event: dict[str, Any] | None) -> bool:
    if not event:
        return False
    requested = event.get("requested_baud")
    actual = event.get("actual_baud")
    return (
        isinstance(requested, int)
        and isinstance(actual, int)
        and abs(actual - requested) * 100 <= requested * 2
    )


def wait_running(process: Any, event_path: Path, timeout: float = 10.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        event = final_event(event_path)
        if event is not None and event.get("phase") == "running":
            return
        return_code = process.poll()
        if return_code is not None:
            raise RuntimeError(f"process exited before running phase with code {return_code}")
        time.sleep(0.01)
    raise TimeoutError(f"process did not reach running phase within {timeout:g}s")


def discard_until_quiet(
    pipe: Any, quiet_seconds: float = 0.25, timeout: float = 2.0
) -> bytes:
    descriptor = pipe.fileno()
    discarded = bytearray()
    deadline = time.monotonic() + timeout
    quiet_deadline = time.monotonic() + quiet_seconds
    os.set_blocking(descriptor, False)
    try:
        while time.monotonic() < deadline and time.monotonic() < quiet_deadline:
            try:
                chunk = os.read(descriptor, 64 * 1024)
            except BlockingIOError:
                time.sleep(0.01)
                continue
            if not chunk:
                break
            discarded.extend(chunk)
            quiet_deadline = time.monotonic() + quiet_seconds
    finally:
        os.set_blocking(descriptor, True)
    return bytes(discarded)




def run_pair(
    root: Path,
    report_dir: Path,
    guart_serial: str,
    c232_serial: str,
    backend: str,
    rate: int,
    name: str,
    c232_tx: bytes,
) -> dict[str, Any]:
    guart_tx = opposite(c232_tx)
    stem = f"{backend}-{rate}-{name}"
    guart_log = report_dir / f"{stem}-guart.ndjson"
    c232_log = report_dir / f"{stem}-c232.ndjson"
    drain_seconds = max(5, math.ceil(len(c232_tx) * 12 / rate) + 5)
    guart_cmd = [
        str(root / "target" / "debug" / "guart"),
        "stream",
        "--serial",
        guart_serial,
        "--baud",
        str(rate),
        "--drain-timeout",
        f"{drain_seconds}s",
        "--event-log",
        str(guart_log),
    ]
    c232_cmd = [
        str(root / "target" / "debug" / "c232uart"),
        "stream",
        "--backend",
        backend,
        "--serial",
        c232_serial,
        "--baud",
        str(rate),
        "--drain-timeout",
        f"{drain_seconds}s",
        "--event-log",
        str(c232_log),
    ]
    started = time.monotonic()
    guart = subprocess.Popen(
        guart_cmd,
        cwd=root,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    c232: subprocess.Popen[bytes] | None = None
    readiness_error = ""
    startup_guart = b""
    startup_c232 = b""
    timed_out = False
    guart_out = guart_err = c232_out = c232_err = b""
    try:
        wait_running(guart, guart_log)
        c232 = subprocess.Popen(
            c232_cmd,
            cwd=root,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        wait_running(c232, c232_log)
        startup_guart = discard_until_quiet(guart.stdout)
        startup_c232 = discard_until_quiet(c232.stdout)
        timeout = max(30.0, len(c232_tx) * 12 / max(rate, 1) + 20.0)
        with ThreadPoolExecutor(max_workers=2) as executor:
            guart_future = executor.submit(guart.communicate, guart_tx)
            c232_future = executor.submit(c232.communicate, c232_tx)
            guart_out, guart_err = guart_future.result(timeout=timeout)
            c232_out, c232_err = c232_future.result(timeout=timeout)
    except FutureTimeout:
        timed_out = True
        for process in (guart, c232):
            if process is not None and process.poll() is None:
                process.kill()
        guart_out, guart_err = guart.communicate()
        if c232 is not None:
            c232_out, c232_err = c232.communicate()
    except (RuntimeError, TimeoutError) as error:
        readiness_error = str(error)
        for process in (guart, c232):
            if process is not None and process.poll() is None:
                process.kill()
        guart_out, guart_err = guart.communicate()
        if c232 is not None:
            c232_out, c232_err = c232.communicate()
    elapsed = time.monotonic() - started
    guart_event = final_event(guart_log)
    c232_event = final_event(c232_log)
    exact_guart_rx = guart_out == c232_tx
    exact_c232_rx = c232_out == guart_tx
    if not exact_guart_rx:
        (report_dir / f"{stem}-guart-actual.bin").write_bytes(guart_out)
        (report_dir / f"{stem}-guart-expected.bin").write_bytes(c232_tx)
    if not exact_c232_rx:
        (report_dir / f"{stem}-c232-actual.bin").write_bytes(c232_out)
        (report_dir / f"{stem}-c232-expected.bin").write_bytes(guart_tx)
    counters_clean = all(
        event
        and event.get("hardware", {}).get("rx_errors") == 0
        and event.get("hardware", {}).get("rx_overflow") == 0
        for event in (guart_event, c232_event)
    )
    guart_returncode = guart.returncode
    c232_returncode = c232.returncode if c232 is not None else None
    passed = all(
        [
            not timed_out,
            not readiness_error,
            guart_returncode == 0,
            c232_returncode == 0,
            exact_guart_rx,
            exact_c232_rx,
            counters_clean,
            within_two_percent(guart_event),
            within_two_percent(c232_event),
            guart_event is not None and guart_event.get("exit_code") == 0,
            c232_event is not None and c232_event.get("exit_code") == 0,
        ]
    )
    total_bytes = len(c232_tx) + len(guart_tx)
    return {
        "name": name,
        "backend": backend,
        "requested_baud": rate,
        "passed": passed,
        "timed_out": timed_out,
        "readiness_error": readiness_error,
        "elapsed_seconds": elapsed,
        "throughput_bytes_per_second": total_bytes / elapsed if elapsed else 0.0,
        "c232_tx_bytes": len(c232_tx),
        "guart_tx_bytes": len(guart_tx),
        "guart_rx_bytes": len(guart_out),
        "c232_rx_bytes": len(c232_out),
        "startup_guart_rx_bytes": len(startup_guart),
        "startup_c232_rx_bytes": len(startup_c232),
        "startup_guart_rx_sha256": hashlib.sha256(startup_guart).hexdigest(),
        "startup_c232_rx_sha256": hashlib.sha256(startup_c232).hexdigest(),
        "guart_rx_sha256": hashlib.sha256(guart_out).hexdigest(),
        "c232_rx_sha256": hashlib.sha256(c232_out).hexdigest(),
        "exact_guart_rx": exact_guart_rx,
        "exact_c232_rx": exact_c232_rx,
        "guart_exit_code": guart_returncode,
        "c232_exit_code": c232_returncode,
        "guart_stderr": guart_err.decode(errors="replace"),
        "c232_stderr": c232_err.decode(errors="replace"),
        "guart_event": guart_event,
        "c232_event": c232_event,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--guart-serial", required=True)
    parser.add_argument("--c232-serial", required=True)
    parser.add_argument("--rates", default="9600,115200,1000000,3000000,12000000")
    parser.add_argument("--payload-size", type=int, default=16 * 1024 * 1024)
    parser.add_argument("--backends", default="vcp,usb")
    parser.add_argument("--json-report", type=Path, required=True)
    args = parser.parse_args()
    if args.payload_size < 0:
        parser.error("--payload-size must not be negative")
    rates = [int(value) for value in args.rates.split(",")]
    backends = args.backends.split(",")
    if any(rate < 9_600 or rate > 12_000_000 for rate in rates):
        parser.error("all rates must be between 9600 and 12000000")
    if any(backend not in {"vcp", "usb"} for backend in backends):
        parser.error("--backends accepts only vcp and usb")

    root = Path(__file__).resolve().parents[1]
    args.json_report.parent.mkdir(parents=True, exist_ok=True)
    report_dir = args.json_report.parent / f"{args.json_report.stem}-events"
    report_dir.mkdir(parents=True, exist_ok=True)
    results = []
    for backend in backends:
        for rate in rates:
            for name, payload in corpus(args.payload_size):
                result = run_pair(
                    root,
                    report_dir,
                    args.guart_serial,
                    args.c232_serial,
                    backend,
                    rate,
                    name,
                    payload,
                )
                results.append(result)
                print(
                    f"{backend} {rate} {name}: {'PASS' if result['passed'] else 'FAIL'}",
                    flush=True,
                )
    report = {
        "schema_version": 1,
        "guart_serial": args.guart_serial,
        "c232_serial": args.c232_serial,
        "rates": rates,
        "backends": backends,
        "payload_size": args.payload_size,
        "passed": all(result["passed"] for result in results),
        "results": results,
    }
    args.json_report.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
