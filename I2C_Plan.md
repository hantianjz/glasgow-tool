# Add native I²C, `gi2c`, and hardware loopback coverage

## Context

Add I²C as a transaction-oriented sibling of the existing UART API. Ship a Glasgow controller/target CLI named `gi2c`, a native FT232H controller companion named `c232i2c`, and two real-hardware verification phases:

1. C232HD-DDHSP-0 MPSSE controller → Glasgow host-serviced target, matching the existing two-device UART lab topology.
2. Glasgow controller on A0/A1 → deterministic FPGA target on B0/B1, proving the positive `gi2c` controller path that the C232 fixture cannot exercise because FT232H cannot act as an I²C target.

User-selected interface decisions:

- Primary CLI shape: discoverable `list`, `scan`, `read`, and `write` commands plus a general ordered-message `transfer` command.
- Electrical profile: fixed A0=SCL, A1=SDA, 3.3 V, integrated pull-ups, 100 kHz default; no runtime pin or voltage selection.
- Data representation: C-style numeric byte/address arguments, lowercase hexadecimal terminal output, optional raw-binary input/output, and explicit JSON output.

I²C is not a byte stream. Preserve `START`, ordered read/write messages, repeated `START`, `STOP`, ACK/NACK, address direction, and transaction boundaries. Do not reuse `uart::Transport`, `Port`, `run_session`, console mode, RX-idle logic, or TX-drain semantics.

## Approach

1. **Extract provider-neutral USB lifecycle code before adding the second bus.**
   - Use language-server references before moving the exported UART provider types or extending `Error`; update every source, test, and documentation callsite in the same cutover.
   - Move Glasgow descriptor discovery/selection, recovery-firmware upload, API-9 management commands, register access, FPGA status/load, endpoint claiming, and cleanup from `src/uart/glasgow/{device,management,transport}.rs` into crate-private `src/glasgow/` modules. Keep UART manifest validation, baud/counter registers, stream endpoints, and drain behavior under `src/uart/glasgow/`.
   - Replace `Management::set_fixed_profile` with resource-driven voltage/pull operations that can configure port A alone or ports A+B, then disable the same supplies and pulls on every close/error path. UART retains its current A0 pull/A1 TX profile; I²C profiles pull both SCL and SDA high.
   - Move direct-USB C232/FTDI discovery, exact VID/PID/product matching, deterministic serial selection, interface-0 claim/detach/reattach, endpoint discovery, vendor-control requests, purge/reset, latency control, and FTDI two-byte USB status-header stripping into crate-private `src/c232/`. Keep serial-port/VCP and UART line configuration in `src/uart/c232/`; I²C will consume only the native USB core. VCP must never be presented as an I²C backend.
   - Preserve the current public UART paths and behavior while switching their implementations to the shared internals; do not leave duplicate discovery or management stacks.

2. **Introduce the public transaction model in `glasgow_tool::i2c`.**
   - Export `i2c` from `src/lib.rs` and update crate-level/error documentation so errors are no longer described as UART-only.
   - Add `Address`, a validated unshifted 7-bit address (`0x00..=0x7f`) with canonical `0xNN` display; shifted 8-bit address forms and 10-bit addresses are rejected.
   - Add borrowed `Operation<'a>` variants carrying an address per message: `Write { address, data: &'a [u8] }` and `Read { address, data: &'a mut [u8] }`. Per-message addresses allow Linux `i2ctransfer`-style combined transactions.
   - Add public `Bus` methods `transfer`, `write`, `read`, `write_read`, `ping`, `scan`, and `metadata`. A private controller trait backs Glasgow and C232 implementations; providers return the same `Bus` type.
   - `transfer` emits one initial `START`, repeated `START` between operations, and one final `STOP`. Empty writes are valid address probes; reads must request at least one byte. Each operation is capped at 65,535 bytes, matching the Glasgow `u16` pipe protocol. Read messages ACK every byte except their final byte, which is NACKed before repeated `START` or `STOP`.
   - Add a structured root error for no-acknowledgement with address and source (`Address` or zero-based data-byte index), map it to existing protocol exit code 4, and let `scan` suppress only address-NACK outcomes. Access, timeout, malformed-response, and cleanup failures remain distinct.
   - `Metadata` records provider, serial/path, requested frequency, and realized frequency. Avoid an `embedded-hal` dependency for this initial API; the required multi-address transfer model is broader than its single-address transaction method.

3. **Generate three pinned Glasgow I²C resource families.**
   - Add `gateware/src/glasgow_tool_gateware/i2c.py` using the already pinned Glasgow commit `b2a15e9c797b90167d96257a557218ddb7984e71` and its `I2CInitiator`/`I2CTarget` engines.
   - Controller resource (`resources/glasgow-i2c-controller/C0..C3`): A0=SCL, A1=SDA, port-A VIO=3.3 V, Glasgow's switchable on-board 10 kΩ pull-ups enabled on both lines, one bidirectional API-9 pipe, and a 16-bit SCL divisor register. Preserve the pinned command bytes exactly: `Start=0x00`, `Stop=0x01`, `Write=0x02 + little-endian u16 length + payload`, `Read=0x03 + little-endian u16 length`; start/stop return one synchronization byte, write returns little-endian acknowledged-byte count, and read returns the requested bytes.
   - Target resource (`resources/glasgow-i2c-target/C0..C3`): the same A0/A1/3.3 V profile with Glasgow's on-board 10 kΩ pull-ups enabled on both target lines, an RW 7-bit address register, and one bidirectional event pipe. Preserve upstream event framing: `START=0x10`, `STOP=0x20`, `RESTART=0x30`, `WRITE=0x40` followed by one byte and one host ACK response, and `READ=0x50` followed by one host data response. Assert `I2CTarget.busy` while host input is required so SCL is stretched safely.
   - Self-test resource (`resources/glasgow-i2c-self-test/C0..C3`): controller on A0/A1 and an autonomous target at fixed address `0x52` on B0/B1, both ports at 3.3 V, Glasgow's on-board 10 kΩ pull-ups enabled only on A0/A1 (B0/B1 pulls disabled so they are not paralleled), and only the controller host pipe. The target implements the same 256-byte register-file semantics used by `gi2c target`: the first written byte selects an 8-bit pointer, later bytes store and post-increment with wrapping, and reads return/post-increment from that pointer. It must not need host round trips.
   - Add `gateware/build_i2c.py`, sharing resource/cache/firmware/descriptor helpers with `build_uart.py` rather than copying the pipeline. Emit role, pins, voltage, pulls, registers, pipe allocation, target address (self-test), API level, pinned commit, bitstream digest, and firmware segments in each manifest.
   - Generalize `build.rs` to validate and embed exact C0/C1/C2/C3 sets for UART, I²C controller, I²C target, and I²C self-test into separately named generated tables. Update `bin/b` and `bin/b.bat` to regenerate all resource families before Cargo builds. Extend `gateware/tests/test_resources.py` to reject missing/extra revisions, wrong profiles, unexpected endpoint layouts, incorrect register widths, stale hashes, or commit drift.

4. **Implement Glasgow controller, target, and self-test runtimes.**
   - Add `src/i2c/glasgow/{mod,controller,target,resources}.rs`. Re-export the normalized passive `DeviceInfo` through `i2c::glasgow`, with `list_devices()` remaining descriptor-only and state-free.
   - `i2c::glasgow::open_controller(OpenOptions { serial, frequency_hz, command_timeout })` validates `100_000..=4_000_000`, computes the 16-bit divisor from the 48 MHz system clock/four-phase initiator, records the actual frequency, loads the revision-matched controller resource, claims the generated pipe, programs the divisor, then enables the fixed 3.3 V profile.
   - Implement each transaction as controller commands without flattening boundaries: write `(address << 1) | direction`, check acknowledged counts, stream payload chunks without copying the entire message, read exact declared lengths, issue repeated starts, and attempt `STOP` after every started transaction even when a message NACKs. A malformed count, short response, timeout, or disconnect is fatal and triggers bounded interface/VIO cleanup.
   - `i2c::glasgow::open_target(TargetOptions { serial, address, command_timeout })` loads the target resource, programs the address before enabling VIO, and exposes a synchronous target-service loop. The loop decodes the five event types and cannot return to the FPGA without the required write ACK/read byte.
   - Add a public `TargetHandler` callback contract (`start`, `restart`, `stop`, `write -> bool`, `read -> u8`) and a `Target::serve` loop with cancellation checked between bounded USB reads. Implement the CLI’s 256-byte register file as a handler, not inside generic USB code.
   - Add a crate-private/self-test opener selecting the self-test resource while reusing the production Glasgow controller transport. `gi2c self-test` writes deterministic patterns at boundary/wrap offsets, performs combined pointer-write/read transactions, and fails on the first byte mismatch.

5. **Implement native C232 FT232H MPSSE I²C controller support.**
   - Add `src/i2c/c232.rs`; expose passive native-USB discovery and `open(OpenOptions { serial, frequency_hz, command_timeout })`. There is no backend selector and no `--port`: C232 I²C is direct USB/MPSSE only, mutually exclusive with UART/VCP on the single FT232H interface.
   - Reuse the extracted USB owner/cleanup. Reset and purge the FT232H, select MPSSE bit mode, verify MPSSE synchronization with invalid-command echoes, disable loopback/divide-by-five as required, enable three-phase clocking, enable drive-zero for SCL/SDA pins, and enable adaptive clocking for the blue ADBUS7 SCL-feedback lead. Never actively drive an I²C line high. After releasing both lines, issue MPSSE `GET_BITS_LOW` and require SDA input (ADBUS2) and SCL feedback (ADBUS7) to sample high before the first `START`; otherwise fail with `I²C bus is not idle-high after releasing SCL/SDA`.
   - Support 100 kHz and 400 kHz initially. Compute the closest FT232H divisor from the 30 MHz MPSSE base while compensating for three-phase I²C clocking (`1.5 ×` requested MPSSE clock, `2/3 ×` reported SCL), expose the realized frequency, and reject other values rather than implying unsupported Fast-mode Plus/High-speed behavior.
   - Compile operations into explicit MPSSE start/repeated-start/stop, byte-out, ACK-sample, byte-in, and ACK/NACK commands. Check each transmitted byte’s ACK before continuing; reads may be batched where reply framing remains unambiguous. Strip FTDI status headers at every USB packet boundary and consume exactly the response bytes belonging to the current command so stale bytes cannot leak into the next transaction.
   - On success or error, release SDA/SCL, reset MPSSE bit mode, release the interface, and reattach the kernel driver when this process detached it.

6. **Add the selected CLI surface and stable data representations.**
   - Register `gi2c` and `c232i2c` in `Cargo.toml`; add thin entrypoints and shared parsing/rendering under `src/bin/common/i2c.rs`. Glasgow-specific dispatch stays in `src/bin/common/gi2c.rs`; C232 dispatch stays in `src/bin/common/c232i2c.rs`.
   - `gi2c` commands: `list`, `scan`, `read`, `write`, `transfer`, `target`, and `self-test`. `c232i2c` commands: `list`, `scan`, `read`, `write`, and `transfer`.
   - Common active-controller options: `--serial`, `--frequency HZ` (default `100000`), and `--timeout DURATION` (default `2s`). Glasgow validates 100 kHz–4 MHz; C232 validates 100/400 kHz. `target` accepts `--address` (default `0x52`) and `--event-log`; `self-test` accepts normal Glasgow selection/timing plus `--json`.
   - Numeric parsers accept decimal or explicit `0x`/`0o`/`0b` prefixes and reject out-of-range values. Addresses are always logical 7-bit values; bytes are `0..=255`.
   - `write ADDRESS BYTE...` sends raw bytes. `--input FILE` or `--input -` reads an exact binary payload and is mutually exclusive with positional bytes. Success is silent unless `--json`, which emits `{ "address": N, "bytes_written": N }`.
   - `read ADDRESS LENGTH` prints one lowercase, two-digit, space-separated hex line by default. `--binary` writes only raw bytes and no newline; `--json` emits `{ "address": N, "data": [N, ...] }`. `--binary` and `--json` are mutually exclusive.
   - `transfer` follows the established `i2ctransfer` descriptor grammar: `w<LENGTH>[@ADDRESS]` followed by exactly that many byte tokens, and `r<LENGTH>[@ADDRESS]`; an omitted address reuses the preceding message address. Example: `gi2c transfer w1@0x50 0x00 r16`. Require an address on the first descriptor, allow `w0` as a probe, reject `r0`, preserve descriptor order, and execute all messages under one start/stop envelope. Default output is one hex line per read message; binary output concatenates read payloads in known descriptor order; JSON emits a `messages` array preserving address, direction, declared length, and each read payload.
   - `scan` probes non-reserved `0x08..=0x77` by default and reports canonical `0xNN` values; JSON is `{ "addresses": [N, ...] }`. An `--all --accept-risk` pair may extend to reserved addresses; document that I²C has no universal side-effect-free discovery operation.
   - `target` emits no payload on stdout. It logs readiness and optional transaction events as NDJSON, hosts the 256-byte register-file handler, and runs until Ctrl-C or an error. All diagnostics remain on stderr so binary controller output stays clean.

7. **Build the two-phase hardware laboratory harness and update existing documentation.**
   - Add `tools/i2c_lab.py` with `--phase c232-target|glasgow-self-test`, explicit device serial options, bounded subprocess timeouts, exact byte comparison, SHA-256 summaries, captured stderr/event logs, nonzero failure, and a JSON report. The script must invoke the real Rust binaries; it must not implement I²C in Python.
   - C232-target wiring uses only the named loose-wire colors/pins below; disconnect USB power before wiring:
     - **SCL node:** join C232 **orange** (PCB pad 2, ADBUS0/SK) and C232 **blue** (PCB pad 9, ADBUS7/RTCK clock-stretch feedback) to Glasgow port-A **IO0/A0 orange** harness lead (pin 3).
     - **SDA node:** join C232 **yellow** (PCB pad 3, ADBUS1/DO) and C232 **green** (PCB pad 4, ADBUS2/DI) to Glasgow port-A **IO1/A1 green** harness lead (pin 5).
     - **Ground:** connect C232 **black** (PCB pad 10) to either adjacent Glasgow port-A **GND black** harness lead (pin 4 or pin 6).
     - Leave C232 **red** (pad 1, 3.3 V output), Glasgow port-A **VIO red** (pin 2), Glasgow port-A **SENSE blue** (pin 1), and every unused signal individually insulated and unconnected. In particular, do not confuse the unconnected Glasgow blue SENSE lead with the C232 blue RTCK lead that belongs on SCL, and never join the two red supply outputs.
     - Use no external resistors in the primary fixture. `gi2c target` sets VIOA to 3.3 V and enables Glasgow's internal 10 kΩ pull-ups on A0/SCL and A1/SDA; those pull-ups are internally tied to VIOA, so the exposed VIO lead is not part of the wiring.
     - Start `gi2c target`, wait for its ready event, then run C232 scan, adjacent-address NACK, raw write/read, combined repeated-start transfer, binary stdin/stdout, all-byte-value, and pointer-wrap cases at 100 kHz.
   - Glasgow self-test wiring uses the two Glasgow breakout harnesses after the C232 cable is fully disconnected:
     - **SCL:** connect port-A **IO0/A0 orange** (harness pin 3) directly to port-B **IO0/B0 orange** (harness pin 3).
     - **SDA:** connect port-A **IO1/A1 green** (harness pin 5) directly to port-B **IO1/B1 green** (harness pin 5).
     - Do not add a ground jumper; both ports already share Glasgow ground. Do not join VIOA to VIOB, and leave both red VIO leads, both blue SENSE leads, and every unused signal insulated and unconnected.
     - The self-test resource sets both VIO rails to 3.3 V and enables Glasgow's internal 10 kΩ pull-ups only on A0/A1; B0/B1 remain open-drain with their pull resistors disabled. No external pull-up resistors are part of the primary setup.
     - Run `gi2c self-test`; exercise write-only, read-only-after-pointer, combined write/read, NACK at an adjacent address, zero-length probe, `0x00`/`0xff`, all 256 byte values, and pointer wrap. Report requested/actual frequency and exact data digests.
   - Expand the existing `README.md` rather than creating another documentation file: broaden the project overview, document all commands/options/examples, fixed electrical profile, address/data conventions, output modes, scan risk, target memory semantics, and both lab phases. Include the exact wire-color/harness-pin tables above and state that Glasgow's on-board 10 kΩ pull-ups are the primary pull-up source with no external resistor wiring.
   - Update `docs/c232hd-ddhsp-0.md` with the exact C232-pad/Glasgow-harness connection table, mandatory yellow+green and orange+blue joins, Glasgow port-A IO0/IO1/GND/VIO/SENSE pin identities, internal 10 kΩ pull-up behavior, fallback resistor connection, red-lead isolation, MPSSE/VCP exclusivity, safe wiring/startup/shutdown order, and the initial 100/400 kHz support boundary. Update `src/lib.rs` and module rustdoc for the public APIs.


## Critical files & anchors

- `src/i2c/{mod,controller,glasgow/*,c232}.rs` — public transaction contract plus Glasgow controller/target and FT232H MPSSE implementations.
- `src/{glasgow,c232}/`, with callers in `src/uart/{glasgow,c232}/` — shared USB ownership, provisioning, pull/VIO control, and clean UART migration boundaries.
- `gateware/src/glasgow_tool_gateware/i2c.py`, `gateware/build_i2c.py`, `build.rs`, and `resources/glasgow-i2c-*` — three role-specific pinned bitstreams, manifests, and embedded tables.
- `src/bin/{gi2c,c232i2c}.rs`, `src/bin/common/{i2c,gi2c,c232i2c}.rs`, and `Cargo.toml` — exact commands, descriptor parsing, binary/JSON rendering, and provider dispatch.
- `tools/i2c_lab.py`, `README.md`, and `docs/c232hd-ddhsp-0.md` — two-phase physical proof and the authoritative wire-color/harness-pin/pull-up instructions.
## Verification

1. Run `bin/b` and confirm all four revision resources for UART and all three I²C roles regenerate, validate, embed, and compile without stale-manifest or endpoint mismatches.
2. Run `bin/cargo test --all-targets`. Retain focused tests for address/descriptor boundaries, descriptor length/address reuse rules, NACK source/index mapping, exact target register-file transitions, FTDI status-packet framing, MPSSE ACK/NACK decisions, and cleanup-visible state; do not add source-text or field-copy tests.
3. Run `bin/uv run --project gateware --frozen python -m unittest discover -s gateware/tests`. Include simulations that prove controller command framing/repeated starts/NACK counts, target event/response clock stretching, and self-test target pointer/store/read/wrap behavior, plus manifest/resource validation.
4. Smoke the real CLIs: `gi2c --help`, `c232i2c --help`, passive `list --json` for both providers, parser rejection for shifted/out-of-range addresses and mismatched transfer lengths, and binary-output cleanliness with diagnostics redirected to stderr.
5. With the documented C232→Glasgow wiring and no external pull-up resistors installed, run `tools/i2c_lab.py --phase c232-target ...`. Require the FT232H idle-high sample to pass using Glasgow's enabled on-board 10 kΩ pulls, then require exact scan/address results, adjacent-address NACK, exact write/read/combined-transfer bytes, target event ordering, binary stdin/stdout equality, digest equality, timeout enforcement, and clean process/device teardown. Record whether the run used `glasgow-10k` or the contingency `glasgow-10k-plus-external-4k7` pull-up mode.
6. Rewire A0↔B0 and A1↔B1, then run `tools/i2c_lab.py --phase glasgow-self-test ...`. Require exact all-byte-value and wraparound comparisons, repeated-start behavior, expected NACK, realized-frequency reporting, and VIO/interface cleanup.
7. After both hardware phases, reopen UART through `guart` and `c232uart` in their existing modes to prove MPSSE/FPGA cleanup did not strand a kernel driver, interface, VIO rail, or FTDI bit mode.

## Assumptions & contingencies

- Scope is 7-bit, single-controller I²C. No 10-bit addressing, multi-controller arbitration, SMBus PEC/block semantics, database probing, or I²C High-speed-mode controller code.
- Glasgow controller commands expose 100 kHz–4 MHz because that is the pinned gateware contract; only 100 kHz is hardware-qualified by this long-cable loopback. C232 exposes only 100 and 400 kHz until separate electrical/timing qualification justifies Fast-mode Plus.
- The fixed electrical contract is A0=SCL, A1=SDA, 3.3 V, with Glasgow's switchable on-board 10 kΩ pull-ups as the only pull-up source in the primary C232 and self-test fixtures. Because the C232 cable is 1.8 m and the upstream Glasgow documentation warns that 10 kΩ may be insufficient for long or fast buses, qualify at 100 kHz first. If the idle-high check or byte-exact transactions fail after the wiring is rechecked, add one 4.7 kΩ resistor from SCL to Glasgow VIOA (port-A red harness pin 2) and one from SDA to the same VIOA pin, leave the on-board pulls enabled, keep C232 red isolated, rerun the same phase, and record that contingency in the report; do not silently lower the clock or change voltage.
- FT232H is controller-only. Its VCP cannot issue MPSSE commands, and MPSSE/UART cannot coexist on the cable’s single interface.
- The C232→Glasgow host-serviced target requires clock stretching and the blue ADBUS7 feedback connection. If hardware qualification shows FT232H adaptive clocking cannot tolerate the measured USB host round trips, keep the CLI/address/memory contract but replace the target resource’s host-per-byte response path with the already specified autonomous FPGA register-file target; do not ship an intermittently stretching host target.
- The two hardware phases require one revC Glasgow, one C232HD-DDHSP-0, and jumper wiring, but not a second Glasgow, external I²C peripheral, or external pull-up resistors for the primary attempt. They run separately because the wiring differs.
- Runtime remains native Rust with embedded gateware. Python/Glasgow dependencies remain build-time and gateware-test-only.