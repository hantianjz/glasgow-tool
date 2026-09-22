# Refactor Glasgow and C232 UART features into a consumable library API

## Context

The package already has a `glasgow_tool` library target, but its public surface exposes CLI types, device-backend internals, and a session function that installs a process-global Ctrl-C handler. Glasgow resources exist only under ignored `target/`, so a clean path or Git dependency cannot build with ordinary Cargo. Refactor the current Glasgow and C232HD UART behavior behind a bus-first `glasgow_tool::uart` API, preserve both executable contracts, and leave room for future top-level bus modules such as `spi` or `i2c` without adding speculative empty abstractions.

## Approach

### 1. Make the crate buildable as an ordinary dependency

- Move the generated C0/C1/C2/C3 manifest and bitstream pairs from `target/glasgow-uart/` into tracked `resources/glasgow-uart/{C0,C1,C2,C3}.{json,bit}`. Do not commit `build-cache.json`; add only that cache file to `.gitignore`.
- Change `build.rs` to validate and embed the tracked resource directory relative to `CARGO_MANIFEST_DIR`, retaining the current schema, pinned upstream commit, profile, register, pipe, and SHA-256 checks. Emit `cargo:rerun-if-changed` for all eight tracked inputs and keep Python/uv out of Cargo builds.
- Change `bin/b` and `bin/b.bat` to run `gateware/build_uart.py --output resources/glasgow-uart` before Cargo. The generator remains the deliberate resource-update path, while `cargo build`, path dependencies, and Git dependencies consume already-generated resources directly.
- Keep one package and one library crate. Do not split a workspace or add backend feature flags in this refactor: the default batteries-included crate is the least surprising consumer experience, and future buses can be added as sibling root modules.

### 2. Establish a bus-first root API and domain error

- Replace the current root exports in `src/lib.rs` with crate-level documentation plus `pub mod uart`, `pub use error::{Error, Result}`, and a private `mod error`. Remove the public `Tool` enum; executable names are presentation metadata, not hardware-domain state.
- Move `AppError` out of `src/cli.rs` into `src/error.rs` as `pub enum Error { Selection(String), Access(String), Protocol(String), Timeout(String), Validation(String) }` and define `pub type Result<T> = std::result::Result<T, Error>`. Preserve existing display strings and category semantics while migrating every source and test callsite; do not retain `AppError` as an alias.
- Create `src/uart/mod.rs` as the shallow public entry point. It publicly exposes `glasgow` and `c232`, privately owns `transport`, `session`, and `telemetry` implementation modules, and re-exports their consumer-facing types and functions. This makes a future bus a sibling such as `glasgow_tool::spi`, rather than another variant in a UART-specific enum.
- Add compile-checked crate documentation showing discovery/open/direct I/O and session usage for both providers. Examples that require hardware must be `no_run`, but all names and types must compile in doctests.

### 3. Wrap backend internals in a reusable UART port

- Move the current low-level transport contract from `src/session.rs` to `src/uart/transport.rs` and keep it extensible for downstream custom UART providers:
  - `pub trait Transport: Send + Sync + 'static` with `metadata`, `read`, `write`, `submit_tx`, `drain`, `cancel`, and `counters` methods preserving the current partial-completion rule.
  - `pub struct TransferOutcome`, `pub struct TransferFailure`, and `pub enum TransferFailureKind` with the current fields and constructors.
  - `pub struct Metadata { pub backend: &'static str, pub serial: Option<String>, pub path: Option<String>, pub requested_baud: u32, pub actual_baud: u32, pub minimum_baud: u32, pub maximum_baud: u32 }`.
  - `pub struct Counters { pub rx_errors: u64, pub rx_overflow: u64 }`.
- Add `pub struct Port` around `Arc<dyn Transport>` with `pub fn new<T: Transport>(transport: T) -> Self`, `metadata`, `counters`, `drain(Duration)`, and `cancel` methods. Glasgow and C232 factory functions return `Port`; concrete USB/VCP transport structs become private implementation details.
- Implement `std::io::Read` and `std::io::Write` for `Port` so ordinary consumers can use `read_exact`, `write_all`, and other standard adapters. A partial completion wins and returns its byte count; a zero-byte failure maps to the corresponding `io::ErrorKind` (`Interrupted`, `WouldBlock`, `TimedOut`, `PermissionDenied`, `InvalidData`, or `NotConnected`); a status-free zero-byte write returns `WriteZero`; `flush` calls `submit_tx`. Document that physical completion requires `Port::drain(timeout)`.
- Replace the fixed `events::Backend` enum with the stable metadata literals `"glasgow"`, `"vcp"`, and `"usb"`. A `&'static str` avoids per-event allocation and allows future providers without changing an exhaustive enum.

### 4. Turn the full-duplex pump into a process-neutral session API

- Move the pump to `src/uart/session.rs` and expose `CONSOLE_ESCAPE: u8 = 0x1d`, `SessionMode`, `SessionStatistics`, `StopReason`, and `SessionReport`. Remove `SessionReport::exit_code`, which is always success on a returned report and belongs to executable policy.
- Add `#[derive(Clone, Default)] pub struct CancellationToken` with `new()`, `cancel()`, and `is_cancelled()`. Change `SessionOptions` to contain `mode`, `rx_idle_timeout`, `drain_timeout`, and a token; provide `SessionOptions::console()` and `SessionOptions::stream()` using the current 2-second RX-idle and 5-second drain defaults.
- Remove `ctrlc::set_handler` from library code. The coordinator watches the supplied token and otherwise preserves the current bounded worker shutdown, partial-transfer accounting, console local echo, Ctrl-] consumption, stream drain/idle behavior, hardware counters, and error propagation.
- Expose `pub fn run_session<R, W>(port: Port, input: R, output: W, options: SessionOptions) -> Result<SessionReport>` and `pub fn run_session_observed<R, W, O>(port: Port, input: R, output: W, options: SessionOptions, observer: &mut O) -> Result<SessionReport>` with the existing `Read + Send + 'static` / `Write + Send + 'static` bounds and `O: SessionObserver + ?Sized`. The unobserved function delegates through `NoopObserver`.
- In `src/uart/telemetry.rs`, define `SessionPhase::{Running, Draining, Final}`, borrowed `SessionEvent<'a>` containing `&Metadata`, phase, statistics, counters, optional stop reason, and optional `&Error`, plus `pub trait SessionObserver { fn observe(&mut self, event: &SessionEvent<'_>) -> Result<()>; }` and `NoopObserver`. Emit `Running` before worker start, `Draining` after device-observable TX drain, and exactly one `Final` event after both workers join.
- Treat observer failure like any coordinator failure: cancel the transport, join both workers within `drain_timeout`, return the observer error, and do not call the failing observer again. A failure while emitting the initial `Running` event returns before workers start; a failure on the already-joined `Final` event returns directly.
- Add `pub struct NdjsonObserver<W: Write>` with `new(writer: W, source: impl Into<String>)` and `into_inner()`. Its `SessionObserver` implementation preserves NDJSON schema version 1 and existing field names/values; the configured source supplies `tool`, metadata supplies backend/identity/baud, and final errors map to the existing exit-code numbers 2 through 6. It flushes each line and maps writer failures to `Error::Access`.

### 5. Publish normalized Glasgow UART discovery and opening

- Move Glasgow UART implementation files under `src/uart/glasgow/`; keep management commands, manifest/register structs, bitstream loading, FX2 upload, and concrete `GlasgowTransport` private.
- Expose `pub struct DeviceInfo { pub serial: Option<String>, pub vendor_id: u16, pub product_id: u16, pub revision: String, pub api_level: u8, pub api_compatible: bool, pub path: String }`; keep the matching `nusb::DeviceInfo` as a private field so consumers do not depend on nusb internals.
- Expose passive `pub fn list_devices() -> Result<Vec<DeviceInfo>>`, normalized `pub struct RecoveryCandidate { pub vendor_id: u16, pub product_id: u16, pub path: String }`, and passive `pub fn list_recovery_candidates() -> Result<Vec<RecoveryCandidate>>`. Neither function opens or mutates hardware.
- Define `pub struct OpenOptions { pub serial: Option<String>, pub baud: u32 }`, defaulting to no serial and 115,200 baud, and `pub fn open(options: &OpenOptions) -> Result<Port>`. It performs the current zero/one/many deterministic selection, validates the matching revC resource, updates incompatible normal Glasgow firmware only on open, configures FPGA/VIO/A0/A1, and returns the wrapped UART port. Preserve cleanup and VIO-disable behavior on setup failure and drop.
- Keep resource validation tests inside the Glasgow implementation rather than exposing `management` or raw embedded resource tables as public API.

### 6. Publish normalized C232 UART discovery and opening

- Move the C232 implementations under `src/uart/c232/`; keep FTDI status parsing, divisor encoding, endpoint discovery, and concrete VCP/USB transport structs private.
- Define public `#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)] pub enum Backend { #[default] Vcp, Usb }` without Clap derives.
- Expose `pub struct DeviceInfo { pub backend: Backend, pub serial: String, pub vendor_id: u16, pub product_id: u16, pub product: String, pub path: String, pub stable_path: Option<PathBuf> }`. Use private backend discovery records to retain nusb/serialport attachment data; never expose those dependency types.
- Expose passive `pub fn list_devices(backend: Backend) -> Result<Vec<DeviceInfo>>`. VCP discovery retains Linux sysfs/by-id correlation; USB discovery uses descriptors only and does not detach a driver.
- Define `pub struct OpenOptions { pub backend: Backend, pub serial: Option<String>, pub port: Option<PathBuf>, pub baud: u32 }`, defaulting to VCP, no selector, and 115,200 baud, plus `pub fn open(options: &OpenOptions) -> Result<Port>`. Reject `port` for USB, preserve serial/path conflict detection and zero/one/many selection, and perform driver detach/reattach only for an explicitly selected USB open.
- Preserve 8N1/no-flow-control setup, DTR/RTS deassertion, 2% baud validation, FTDI packet-status stripping, line counters, device-observable drain, and bounded drop cleanup.

### 7. Reduce both executables to adapters over the public API

- Move Clap structs, duration/baud parsers, exit-code mapping, terminal raw-mode guard, list formatting/JSON views, and command dispatch into `src/bin/common/` shared by `src/bin/guart.rs` and `src/bin/c232uart.rs`. Remove `src/cli.rs`, `src/terminal.rs`, `src/events.rs`, `src/glasgow/mod.rs::run`, and `src/c232/mod.rs::run` after every caller is migrated; do not leave public aliases or deprecated paths.
- Keep command names, flags, defaults, selection messages, stdout/stderr separation, list JSON string formatting for VID/PID, console summary text, Ctrl-] behavior, and process exit codes unchanged. The private C232 CLI backend enum maps explicitly to `uart::c232::Backend`.
- For each console/stream invocation, create a `CancellationToken`, install the executable’s single Ctrl-C handler to call `cancel`, create either `NoopObserver` or `NdjsonObserver<BufWriter<File>>`, open the provider through its public `OpenOptions`, and call `run_session_observed`. Raw-terminal entry remains only around console calls.
- Keep the root `README.md` focused on `guart` as previously requested. Put the complete two-provider consumer guide in crate-level rustdoc so library documentation can cover C232 without changing that README scope.

## Critical files & anchors

- `src/session.rs: Transport and run_session` — currently combines low-level device I/O, global signal handling, telemetry, local echo, and pump orchestration; this is the central API split.
- `src/glasgow/uart.rs: GlasgowTransport::open` — owns firmware/resource/FPGA/VIO lifecycle and must remain behaviorally intact behind `uart::glasgow::open`.
- `src/c232/{vcp,usb}.rs: VcpTransport::open and UsbTransport::open` — preserve backend-specific selection, driver, status-byte, drain, and cleanup semantics behind one C232 facade.
- `src/{glasgow,c232}/mod.rs: run` — duplicated executable orchestration to delete after the binaries use the public API.
- `build.rs: main` — currently reads ignored `target/glasgow-uart`; changing this input is required for clean dependency builds.

## Verification

1. Run `bin/b`, then `bin/cargo fmt --check`, `bin/cargo clippy --all-targets -- -D warnings`, `bin/cargo test --locked`, and `bin/cargo test --locked --doc` from the repository root.
2. Replace the current cross-integration imports with the new shallow API and retain the protocol assertions for resource rejection, baud boundaries, FTDI packet stripping, partial completion accounting, stream drain/idle, console local echo, old Ctrl-\\ transmission, and Ctrl-] exit. Put custom fake transports through `Port::new` so the public extension point itself is exercised.
3. Add one integration test that runs a stream session and then a console session in the same test process with separate `CancellationToken`s. Both must complete, proving the library no longer registers a singleton Ctrl-C handler; assert exact payload, byte counts, stop reasons, and observer phase order.
4. Add an NDJSON observer test using an in-memory writer. Feed a deterministic fake transport and assert schema version 1, caller-supplied `tool`, backend string, identity/baud fields, counters, phases, stop reason, and final exit code without asserting incidental timestamps.
5. Create a throwaway external crate under `target/library-consumer` with `glasgow-tool = { path = "../.." }`. Compile code that imports `glasgow_tool::{Error, uart}`, lists both providers, constructs both `OpenOptions` types, and uses a fake `Transport` through `Port`, `Read`/`Write`, and `run_session`; run `bin/cargo check --manifest-path target/library-consumer/Cargo.toml`, then remove the throwaway crate. This must work without running `bin/b` inside the consumer or having Python/uv on its build path.
6. Verify unchanged CLI contracts with `bin/r guart --help`, all three `guart` subcommand help pages, `bin/r c232uart --help`, all three C232 subcommand help pages, and both `list --json` commands. Compare keys and value types against the pre-refactor schema, especially string VID/PID fields.
7. With the existing cross-wired fixture, run `bin/uv run --project gateware --frozen python tools/lab.py --guart-serial C3-20240903T135215Z --c232-serial FT61SVEW --rates 115200 --payload-size 65536 --backends vcp,usb --json-report target/library-refactor-lab.json`. Require exact data in both directions, clean drain/exit, zero unexplained counters, and passing final status for both backends.
8. Run a PTY smoke test for `guart console`: ordinary `K` must echo locally and reach the peer while active, Ctrl-\\ (`0x1c`) must echo/transmit without exit, and Ctrl-] (`0x1d`) must exit cleanly without reaching the peer. This covers the console behavior most exposed to orchestration changes.

## Assumptions & contingencies

- “cs232” means the existing C232HD-DDHSP-0 functionality and `c232uart` executable.
- The intended consumer model is synchronous/blocking Rust, matching every current backend. Do not introduce an async runtime; callers can place blocking operations on their own threads.
- The crate remains unpublished (`publish = false`) but must work as a path or Git dependency from a clean checkout. If `cargo package` reveals an implicit exclusion despite tracked resources, add an explicit Cargo `include` list covering Rust sources, `build.rs`, and `resources/glasgow-uart`; do not restore target-directory coupling.
- The existing Glasgow/C232 fixture serials and wiring are available for final physical verification. If either device is absent at execution time, complete all software, external-consumer, and available-device checks, then report the exact missing hardware verification rather than weakening it or substituting mocks.