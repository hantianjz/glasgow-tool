# glasgow-tool

A collection of tools built around the Glasgow Digital Interface Explorer.

## Development toolchain

The repository uses [envy](https://github.com/envy-package-manager/envy) for hermetic, per-project developer tools. Envy itself, Rust, Python, and uv are pinned by version and checksum. Cargo remains responsible for Rust dependencies; uv remains responsible for Python dependencies.

No global Envy, Rust, or Python installation is required. The committed wrappers bootstrap tools on first use:

```sh
bin/envy sync
bin/b
bin/r guart --help
bin/r c232uart --help
```

Windows equivalents are `bin\envy.bat sync`, `bin\b.bat`, `bin\r.bat guart --help`, and `bin\r.bat c232uart --help`.

Pinned tools can be invoked directly through `bin/`, for example:

```sh
bin/cargo --version
bin/rustc --version
bin/python3 --version
bin/uv --version
```

`bin/b` generates and embeds the pinned Glasgow revC resources before building both Rust binaries. Runtime use does not require Python. See [`docs/uart-plan.md`](docs/uart-plan.md) for the CLI contract, device matching, safe wiring, permissions, and hardware-qualification procedure.

Create deterministic per-tool Linux release archives and checksums with:

```sh
bin/b --release --locked
bin/uv run --project gateware --frozen python tools/package.py
```

Native Windows and macOS packages are built by `.github/workflows/release.yml`. Release archives include `THIRD_PARTY_NOTICES.txt`.
