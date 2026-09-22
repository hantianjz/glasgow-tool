# Native UART tools with Envy, Rust, and build-time Glasgow

## Context

The repository is already a Rust 2024 project managed by Envy, with Rust 1.98.1, Python 3.13.15, uv 0.12.8, `bin/b` as the supported build entry point, and only a placeholder `src/main.rs`. The imported `docs/uart-plan.md` has useful protocol, observability, and validation requirements, but its language decision, separate native-project layout, checked-out Glasgow source assumptions, all-revision scope, and libusb architecture do not match this repository. Implement two Python-free runtime binaries, `guart` and `c232uart`, with Linux as the fully validated platform, Windows/macOS as compile-and-package targets, and only revC Glasgow resources (`C0` through `C3`).

Current fixture state is explicit: C232HD-DDHSP-0 serial `FT61SVEW` enumerates as `0403:6014`, `/dev/ttyUSB0`, with stable VCP path `/dev/serial/by-id/usb-FTDI_C232HD-DDHSP-0_FT61SVEW-if00-port0`; Glasgow is physically USB-connected according to the user but does not enumerate as either `20b7:9db1` or `04b4:8613`. The devices are not electrically connected, and normal-user tty/direct-USB permissions have not been qualified.

## Approach

### 1. Convert the root package into the two-tool application

- Keep one root Cargo package and add explicit binaries at `src/bin/guart.rs` and `src/bin/c232uart.rs`; move shared behavior into `src/lib.rs`. Remove the placeholder `src/main.rs` rather than retaining a third binary or compatibility alias.
- Add dependency families with Cargo.lock pinning the concrete resolution: `clap` 4 for derive-based CLI parsing, `serde`/`serde_json` for manifests and event records, `sha2` for embedded-resource integrity, `thiserror` 2 for typed errors, `nusb = 0.2.7` for pure-Rust native USB, `serialport = 4.10.1` with Linux `libudev` disabled, `crossterm` for safe terminal raw-mode/restore handling, and `ctrlc` for bounded cancellation. Preserve `unsafe_code = "forbid"`; all OS/USB unsafety stays inside reviewed dependencies.
- Use this module boundary:
  - `src/cli.rs`: shared option types, exit-code mapping, and selectors.
  - `src/session.rs`: bounded full-duplex pump, cancellation, drain, and byte/error accounting.
  - `src/events.rs`: stable NDJSON event schema.
  - `src/terminal.rs`: console raw-mode guard and guaranteed restoration.
  - `src/resources.rs`: generated embedded Glasgow resource table.
  - `src/glasgow/{device,management,uart}.rs`: passive discovery, API-9 management, FPGA load, and UART transport.
  - `src/c232/{mod,vcp,usb}.rs`: FTDI identity selection, VCP backend, and direct-USB backend.
- Change `bin/r` and `bin/r.bat` to require `guart` or `c232uart` as their first argument and execute the corresponding debug binary; reject any other name. Keep `bin/b`/`bin/b.bat` as the single development build entry point and preserve Cargo argument forwarding.

### 2. Generate and embed revC Glasgow resources during the standard build

- Add an isolated `gateware/pyproject.toml` and `gateware/uv.lock`. Pin Glasgow to commit `b2a15e9c797b90167d96257a557218ddb7984e71` from its `software` subdirectory with the `builtin-toolchain` extra. Envy continues to provide Python and uv; developers do not install system Python packages, Yosys, or nextpnr.
- Add a project-owned Python package under `gateware/src/glasgow_tool_gateware/`. Its fixed UART applet imports and reuses Glasgow's `glasgow.gateware.uart.UART` bit engine and hardware assembly APIs; it does not fork the UART serializer/deserializer.
- Recreate only the small upstream host-facing UART wrapper so this profile can expose the observability the native runtime requires:
  - 32-bit saturating RX framing/parity error counter.
  - 32-bit saturating RX overflow counter.
  - Read-only `tx_state` register containing FIFO fill count plus an explicit transmitter-idle bit.
  - Runtime baud divisor register.
  - One bidirectional pipe.
  - Fixed non-inverted profile: port A at 3.3 V, A0 as RX, A1 as TX, 8 data bits, no parity, one stop bit, no flow control.
- Add `gateware/build_uart.py`. Build exactly `C0`, `C1`, `C2`, and `C3`. For each revision emit the raw FPGA bitstream plus a JSON manifest containing schema version, upstream commit, revision, fixed profile, bitstream ID, SHA-256, register address/width/endian metadata, pipe interface/alternate-setting/endpoint/max-packet metadata, and bundled API-9 FX2 firmware segments parsed from upstream `software/glasgow/hardware/firmware.ihex`.
- Derive register and pipe addresses from the actual objects returned by the assembly and assert their expected types/ranges. Do not duplicate allocation constants in Rust. Treat any upstream private-field/layout change as a build-time hard failure with the pinned commit and symbol named in the error.
- Make generation content-addressed over the lockfile, upstream commit, local gateware sources, revision, and fixed profile. Reuse `target/glasgow-uart/` only when every input hash and artifact digest matches; otherwise rebuild atomically through a temporary directory.
- Add root `build.rs`. It validates all four manifests and hashes, copies the selected artifacts into Cargo `OUT_DIR`, and generates the Rust resource table consumed by `src/resources.rs`. It must not invoke Python or download tools. If artifacts are absent, fail with `run bin/b to generate Glasgow UART resources`.
- Update `bin/b`/`bin/b.bat` to run `bin/uv run --project gateware --frozen python gateware/build_uart.py --output target/glasgow-uart` before `bin/cargo build "$@"`. This is the standard development build. Release binaries embed resources and have no Python, uv, Amaranth, or FPGA-toolchain runtime dependency.

### 3. Define one stable CLI and event contract

- Commands:
  - `guart list [--json]`
  - `guart console [--serial SERIAL] [--baud RATE] [--event-log PATH]`
  - `guart stream [--serial SERIAL] [--baud RATE] [--rx-idle-timeout DURATION] [--drain-timeout DURATION] [--event-log PATH]`
  - `c232uart list [--json]`
  - `c232uart console [--backend vcp|usb] [--serial SERIAL] [--port PATH] [--baud RATE] [--event-log PATH]`
  - `c232uart stream` with the same backend/selector/timing options.
- Defaults: baud 115200, fixed 8N1, no hardware/software flow control, C232 backend `vcp`, stream RX-idle timeout 2 s, drain timeout 5 s. Accept 9,600 through 12,000,000 baud only when the backend can represent the requested divisor with no more than 2% error; report requested and actual baud.
- Selection invariant: zero matches is an error, one match is selected, more than one requires `--serial`; `--port` is VCP-only and cannot be combined with a conflicting serial. Never select the first device silently.
- Output invariant: byte payload uses stdout only; diagnostics use stderr; `list --json` emits one JSON array to stdout; `--event-log` writes NDJSON to a file so stream payload cannot be corrupted by telemetry. Stable exit codes: 0 success, 2 CLI/selection, 3 access/busy, 4 protocol/resource mismatch, 5 timeout/cancellation, 6 validation mismatch.
- `console` uses raw local terminal mode, restores it through RAII on every normal/error/signal path, forwards bytes bidirectionally, and reserves Ctrl-\\ (`0x1c`) as the local exit sequence. `stream` is fully byte-transparent: stdin EOF starts TX drain, RX continues until drain completes and the RX-idle window expires, and SIGINT cancels both directions with bounded waits.
- Event records include monotonic timestamp, tool, backend, selected serial/path, requested/actual baud, phase, TX bytes accepted, TX bytes completed, RX bytes, hardware error/overflow counters, timeout/cancel reason, and final exit code. Console status output may summarize these records but cannot invent additional state.

### 4. Implement the shared full-duplex session before device-specific backends

- Define a narrow transport interface: `read`, `write`, `drain`, `cancel`, `counters`, and immutable identity/capability metadata. Implement it with blocking worker threads rather than introducing an async runtime.
- Use one RX worker and one TX worker with bounded 64 KiB application queues. The coordinator owns cancellation and terminal/event cleanup. The first terminal condition or error cancels the peer, joins both workers within the configured deadline, then closes the transport.
- Account transfer bytes before interpreting completion status; USB cancellations and failures may report a nonzero partial length. Retry only explicit interrupted/would-block conditions and preserve all completed bytes. No busy loop, unbounded queue, detached thread, or swallowed device error.
- Drain means device-observable transmission completion, not merely successful host write. Glasgow polls `tx_state`; VCP polls the driver output queue; FTDI direct USB polls line-status TEMT after all USB completions. Exceeding `--drain-timeout` is exit code 5 with final counters.

### 5. Deliver the C232HD VCP backend first

- Enumerate only USB serial ports with VID:PID `0403:6014` and product `C232HD-DDHSP-0`. Use serial number as identity and path as a current attachment detail; never persist `/dev/ttyUSB0` as identity.
- On Linux, correlate `serialport` paths with `/dev/serial/by-id` and `/sys/class/tty` so `FT61SVEW` resolves to the observed stable path without libudev. On Windows/macOS use `serialport` USB metadata and platform port names; these remain compile/package targets until hardware-tested.
- Open exclusively when the platform supports it, configure exactly 8N1/no flow control, disable unexpected DTR/RTS assertion, set bounded read timeouts, and expose driver output-queue depth for drain. Return a clear access/busy error containing the selected identity and path.
- Prove the common session layer through this backend before implementing Glasgow: list the attached `FT61SVEW`, then perform a temporary orange-TX to yellow-RX loopback with exact payload comparison and terminal restoration checks.

### 6. Implement passive Glasgow discovery and API-9 management with `nusb`

- `guart list` is strictly passive: read descriptors and supported vendor metadata only. It must never upload RAM firmware, load an FPGA, change voltage, or call upstream Python `GlasgowDevice.enumerate()`, whose apparent enumeration path may mutate a device.
- Match normal Glasgow identity `20b7:9db1`, expose USB serial and exact revision, and report API compatibility. A generic Cypress `04b4:8613` is only a recovery candidate and is never auto-claimed or auto-flashed.
- `console`/`stream` select a normal Glasgow by serial, open it through `nusb`, verify API level 9, and choose the embedded resource matching the reported `C0`/`C1`/`C2`/`C3` revision. Reject unknown revisions and digest/schema/profile mismatches before changing hardware.
- If a `20b7:9db1` Glasgow reports incompatible/missing RAM firmware, upload only the embedded pinned FX2 firmware through the documented Cypress RAM protocol, reset, close, and wait up to 10 s for the same serial/path identity to re-enumerate. Do not apply this path to bare `04b4:8613` devices.
- Follow upstream API-9 FPGA lifecycle exactly: read FPGA status; clear configuration with `FPGA_LOAD` OUT and `wValue=0`; poll status at 10 ms intervals with a 2 s deadline; stream the raw bitstream in repeated control writes, advancing by each transfer's actual completed length; finalize with `FPGA_LOAD` OUT and `wValue=1`; poll configuration complete; claim manifest-declared UART interfaces/alternate settings; then enable the fixed 3.3 V profile.
- Program the baud divisor, verify actual baud within 2%, then start continuous queued bulk IN before accepting TX. Maintain eight 32 KiB transfers per direction, immediately resubmitting RX buffers. Poll widened error/overflow counters during the session and `tx_state` during drain.
- Every failure path cancels transfers, disables the applet/VIO where API state permits, releases interfaces, and closes the device. Never leave a worker owning a handle after the coordinator returns.

### 7. Add the C232HD direct-USB backend without changing CLI behavior

- Use the same `nusb` abstraction and selector. Detach/claim the FTDI interface only after the user explicitly chooses `--backend usb`; remember kernel-driver state and reattach on close where the platform supports it. Failure to detach is an access/busy error, not a fallback to VCP.
- Apply the FT232H SIO sequence: reset, purge RX/TX, set latency timer, calculate the high-speed baud divisor, set 8N1, disable flow control, and keep DTR/RTS inactive. Validate the returned actual baud under the same 2% rule.
- Discover endpoints and max-packet size from descriptors. Strip the two FTDI status bytes at the start of every USB packet, including short packets; convert OE/PE/FE/BI status changes into counters/events while preserving payload order.
- Queue eight 32 KiB reads and writes. Account partial completions before status. Drain after the final bulk completion by polling FTDI line status for TEMT until the shared timeout.
- Run the same loopback and stream corpus against `vcp` and `usb`; backend differences may appear only in identity/capability/event metadata, not payload bytes or lifecycle semantics.

### 8. Keep validation external and byte-exact

- Add `tools/lab.py`, invoked through the pinned gateware Python environment, only as an orchestration/validation tool. It launches the real binaries, never imports their internals, and writes a JSON report under `target/`.
- Corpus: empty input, every byte value, CR/LF/NUL-heavy data, seeded pseudorandom chunks crossing 512-byte USB and 64 KiB application boundaries, and 16 MiB sustained data. Validate exact bytes and lengths in both directions independently.
- Record requested/actual baud, elapsed time, throughput, TX/RX byte counts, Glasgow and FTDI hardware counters, timeouts, and process exit codes. A rate passes only with exact data, zero unexplained device errors/overflows, successful drain, and clean process shutdown.
- Retain unit tests only for plausible protocol bugs: revision/resource selection, manifest digest/schema rejection, baud-divisor boundaries/error threshold, FTDI per-packet status stripping across short/full packets, partial USB completion accounting, and stream drain/idle state transitions. Use the real binaries and physical fixture—not mocks—as proof of end-to-end behavior.

### 9. Prepare the hardware safely before cross-device tests

- Required equipment:
  - Existing revC Glasgow and C232HD-DDHSP-0.
  - Two known-good USB data cables; Glasgow's USB-C source must provide at least 5 V/1 A.
  - The proper Glasgow port-A fly-wire loom or labelled breakout, plus three short 2.54 mm male-male couplers/header pins if both loose ends are female.
  - Insulation for unused FTDI leads, strain relief, and a DMM for unpowered continuity and voltage checks.
  - Optional two-channel logic analyzer or oscilloscope for independently confirming physical baud and signal integrity above 1 Mbaud.
- Resolve Glasgow enumeration before connecting signal wires. With all flying leads disconnected, observe power/FX2 LEDs, try one known-good data cable directly on a host port rather than the current hub, wait 10 s, and inspect USB/kernel events. Change one variable per trial: cable, then port, then another host if available. Classify `20b7:9db1` as normal Glasgow; classify `04b4:8613` only as a Cypress recovery candidate and stop for a deliberate official recovery procedure. If no attach event remains after bounded substitutions, collect LED/cable/port/log evidence and stop; do not rewrite EEPROM, use boot jumpers, inject voltage, or attempt native firmware recovery.
- After normal enumeration, record the exact revC stepping and serial, then use the matching revision manual and connector pin-1 marking. The exact standard revC3 port-A harness nets are `IO0/A0` on connector pin 3 (official Glasgow loom orange), `GND` on pin 4 or 6 (official loom black), and `IO1/A1` on pin 5 (official loom green). The names `A0`, `A1`, and `GND` are authoritative; do not apply revC3 pin numbers or infer connector orientation on an unidentified or modified board.
- Install a scoped local-access rule through project documentation, not automatically from the binary:
  - `SUBSYSTEM=="usb", ATTR{idVendor}=="20b7", ATTR{idProduct}=="9db1", TAG+="uaccess"`
  - `SUBSYSTEM=="usb", ATTR{idVendor}=="0403", ATTR{idProduct}=="6014", TAG+="uaccess"`
  - `SUBSYSTEM=="tty", ATTRS{idVendor}=="0403", ATTRS{idProduct}=="6014", TAG+="uaccess"`
  Reload rules and replug only when intentionally performing setup. Do not grant generic Cypress `04b4:8613` access. The current user is not in `uucp`, so access must be verified after this step rather than assumed.
- Qualify each endpoint separately before cross-wiring:
  1. FTDI loopback: orange TXD to yellow RXD; all other colored leads isolated; run both backends, then remove the loop.
  2. Glasgow loopback: configure port A 3.3 V, connect A1 TX to A0 RX, validate, disable VIO/app, then remove the loop.
- Final cross-device wiring, made while both USB cables are unplugged:

  | C232HD-DDHSP-0 cable lead | Cable signal/direction | Glasgow port-A net | Standard revC3 connector / official loom |
  | --- | --- | --- | --- |
  | **Orange** | TXD, output from FTDI | **A0 / IO0**, configured as Glasgow RX | **Pin 3 / orange loom wire** |
  | **Yellow** | RXD, input to FTDI | **A1 / IO1**, configured as Glasgow TX | **Pin 5 / green loom wire** |
  | **Black** | GND | **GND** | **Pin 4 or pin 6 / black loom wire** |
  | **Red** | +3.3 V power output | **No connection** | Insulate individually |
  | Green | RTS output | **No connection** | Insulate individually |
  | Brown | CTS input | **No connection** | Insulate individually |
  | Grey | DTR output | **No connection** | Insulate individually |
  | Purple | DSR input | **No connection** | Insulate individually |
  | White | DCD input | **No connection** | Insulate individually |
  | Blue | RI input | **No connection** | Insulate individually |

  In wire-to-wire terms with the official Glasgow loom: **FTDI orange to Glasgow orange; FTDI yellow to Glasgow green; FTDI black to one Glasgow black ground wire**. This crossing is intentional: each transmitter goes to the other device's receiver. Leave Glasgow pin 1 `SENSE`, pin 2 `VIO`, all unused A signals, and port B physically unconnected. Glasgow configures its own port-A I/O rail to 3.3 V; never join that rail to the FTDI red lead. Connect ground first, then FTDI orange-to-A0, then FTDI yellow-to-A1. After checking continuity and confirming no red/VIO connection, reconnect both USB cables for the harness test. Unplug both USB cables before any later rewiring. Do not otherwise color-match the cable assemblies: FTDI green is RTS, while Glasgow green is A1.

### 10. Integrate, document, and package only after physical proof

- Update `docs/uart-plan.md` to this approved Rust/revC design and append the observed fixture serial, stable path, safe wiring, permissions, and bounded Glasgow-enumeration procedure. Keep `docs/c232hd-ddhsp-0.md` as the cable's electrical authority rather than duplicating or changing its facts.
- Linux is tier 1: build, list, console, stream, VCP, direct USB, Glasgow provisioning, and physical test matrix must pass. Windows and macOS must compile and package both binaries; label runtime backends unvalidated until real hardware exercises them.
- Produce one binary per tool/platform with embedded revC resources, a checksum file, and no Python/libusb/FPGA-toolchain runtime dependency. Preserve third-party notices for Rust crates and the pinned Glasgow/Amaranth-derived bitstreams/source obligations.
## Critical files and anchors

- `Cargo.toml`, `Cargo.lock`, `build.rs`, `src/lib.rs`, `src/bin/{guart,c232uart}.rs` — package split, dependency lock, generated-resource embedding, and executable entry points.
- `src/session.rs`, `src/glasgow/{device,management,uart}.rs`, `src/c232/{vcp,usb}.rs` — byte/lifecycle invariants and both native transports.
- `gateware/pyproject.toml`, `gateware/uv.lock`, `gateware/build_uart.py`, `gateware/src/glasgow_tool_gateware/` — pinned upstream source, fixed UART applet, revC builds, manifests, and FX2 firmware extraction.
- `bin/b`, `bin/b.bat`, `bin/r`, `bin/r.bat`, `envy.lua` — standard Envy generation/build/run workflow; existing tool pins remain authoritative.
- `docs/uart-plan.md`, `docs/c232hd-ddhsp-0.md`, `tools/lab.py` — approved design/hardware procedure, local cable reference, and physical acceptance harness.

## Verification

1. Reproducible build and resource integrity:
   - `bin/b`
   - Re-run `bin/b` and confirm the content-addressed gateware step reuses identical artifacts.
   - `bin/cargo test`
   - `bin/uv run --project gateware --frozen python -m unittest discover -s gateware/tests`
   - Inspect generated manifests for exactly C0/C1/C2/C3, pinned commit, fixed A0/A1/3.3 V profile, and matching SHA-256 values.
2. Attached-device discovery before wiring:
   - `bin/r c232uart list --json` returns exactly `FT61SVEW`, `0403:6014`, and its stable/current VCP paths without opening or changing it.
   - `bin/r guart list --json` currently returns an empty list and exit 0; after cable/enumeration setup it returns the exact Glasgow serial/revision/API without loading firmware or FPGA resources.
3. Separate loopbacks:
   - With only orange-to-yellow connected, pipe seeded binary data through `c232uart stream` using both `--backend vcp` and `--backend usb`; compare output byte-for-byte, require clean drain and zero FTDI line errors.
   - With only A1-to-A0 connected, pipe the same corpus through `guart stream`; require clean drain, zero framing/parity/overflow counters, and VIO/app disable on exit.
   - Interrupt each console and stream path during active I/O; verify terminal restoration, bounded worker exit, interface release, and immediate successful reopen.
4. Cross-device acceptance after the three-net fixture is checked:
   - `bin/uv run --project gateware --frozen python tools/lab.py --guart-serial <SERIAL> --c232-serial FT61SVEW --rates 9600,115200,1000000,3000000,12000000 --payload-size 16777216 --json-report target/uart-lab.json`
   - Require exact data in both directions, actual baud within 2%, zero unexplained hardware errors/overflows, clean drain, and no hangs at every passing rate. Record rather than hide a rate-specific physical failure.
5. Portability and packaging:
   - Build/package both binaries on Linux, Windows, and macOS CI runners from the lockfiles.
   - On Linux, unpack the release in a clean environment and repeat list plus 115200 cross-device stream without Python, uv, libusb, Yosys, or nextpnr installed.

## Assumptions and contingencies

- The user's board is a revC Glasgow but its exact C0/C1/C2/C3 stepping is unknown. Building all four revC resources avoids guessing; runtime must refuse every other revision.
- Glasgow's failure to enumerate does not block software, gateware, C232 VCP, or C232 direct-USB implementation. It blocks Glasgow hardware proof and the final cross-device acceptance matrix; report those checks as unrun until normal USB identity is established.
- `04b4:8613` is not unique to Glasgow. Native discovery and recovery never mutate that identity automatically.
- The FTDI's current tty and USB bus/device numbers are ephemeral. `FT61SVEW` is the fixture identity; paths remain reported attachment data.
- The 12 Mbaud ceiling is a validation target, not a present hardware claim. Only rates meeting divisor error, exact-byte, counter, drain, and signal-integrity checks are reported as supported.
- If the pinned upstream commit changes, update the git pin and lockfile deliberately, rebuild all resources, inspect allocation assertions, rerun protocol/unit tests, and repeat the full physical matrix before accepting new artifacts.