# glasgow-tool

A collection of tools built around the Glasgow Digital Interface Explorer.

## Development toolchain

The repository uses [envy](https://github.com/envy-package-manager/envy) for hermetic, per-project developer tools. Envy itself, Rust, Python, and uv are pinned by version and checksum. Cargo remains responsible for Rust dependencies; uv remains responsible for Python dependencies.

No global Envy, Rust, or Python installation is required. The committed wrappers bootstrap tools on first use:

```sh
bin/envy sync
bin/b
bin/r
```

Windows equivalents are `bin\envy.bat sync`, `bin\b.bat`, and `bin\r.bat`.

Pinned tools can be invoked directly through `bin/`, for example:

```sh
bin/cargo --version
bin/rustc --version
bin/python3 --version
bin/uv --version
```
