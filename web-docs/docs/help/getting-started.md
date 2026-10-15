# Getting Started

## Requirements

The current development environment targets Linux x86-64.

Install:

- Rust via `rustup`
- `just`
- Git
- SQLite development/runtime support as required by your distribution

For VM and Evolution World work later, QEMU/KVM will also be required.

## Build

```sh
git clone https://github.com/Doldas/flowerly_os.git
cd flowerly_os
just check
just test
just build
```

## Run the development runtime

```sh
just serve
```

In another terminal:

```sh
just status
```

When finished:

```sh
just stop
```

## Control plane

The project includes `agenticctl`, the administrative CLI used to query and eventually operate Agentic OS services.

Check the repository README and `agenticctl --help` for commands available in the current revision.

## What not to expect yet

The current development version does **not** yet provide a polished graphical shell, always-available voice, persistent PTY-backed Flow sessions, autonomous self-evolution, or a production-ready installer.
