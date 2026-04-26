# VPFS

A versioned peer-to-peer file system built with [Iroh](https://github.com/n0-computer/iroh) (QUIC). This repository builds three binaries: the VPFS daemon, a demo shell, and a conflict resolver. The daemon must be running on a machine before any other program can interact with the file system on that machine.

## Building

```sh
cargo build --release
```

## Usage

### Daemon

```sh
cargo run --bin daemon -- -n <name> [options]
```

| Flag | Description | Default |
|------|-------------|---------|
| `-n, --name <name>` | Name for this node. Must be unique across the system and consistent across restarts. | *(required)* |
| `-p, --port <port>` | Iroh (QUIC) port for peer-to-peer communication. | `8081` |
| `-l, --listen-port <port>` | TCP port for connections from local client programs. | `8082` |
| `-c, --conflict-port <port>` | TCP port used to contact the conflict resolver on concurrent modifications. | `8083` |
| `--remote-id <pubkey>` | Iroh public key of an existing node to connect to. Omit to start a new network. | — |
| `-s, --cache-size <bytes>` | Maximum size of the local file cache in bytes. | `65536` |

The daemon persists state across restarts in the `./files/` directory: a `log`, a `file_system` snapshot, a `cache` state file, and any files owned or cached by the node.

### Shell

A basic demo shell for testing VPFS functionality.

```sh
cargo run --bin sh [-- options]
```

| Flag | Description | Default |
|------|-------------|---------|
| `-p, --port <port>` | Listen port of the local VPFS daemon. | `8082` |

The shell supports input redirection (`<`), output redirection (`>`), and pipes (`|`). External programs can be launched from the shell — VPFS paths are resolved before the program starts, so any program that reads from stdin or writes to stdout can interact with VPFS files through these mechanisms.

### Conflict Resolver

The conflict resolver runs independently of the daemon and must be started separately.

```sh
cargo run --bin conflict_resolver [-- options]
```

| Flag | Description | Default |
|------|-------------|---------|
| `-p, --port <port>` | Port to listen on for connections from the local daemon. Must match the daemon's `--conflict-port`. | `8083` |
