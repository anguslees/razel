# Razel Project Details for AI Agents

`razel` seeks to be a drop-in modern replacement for `bazel` command line build tool.

It is implemented in Rust, understands bazel MODULE.bazel and BUILD.bazel files, takes full advantage of Remote Build Execution protocol, and leans heavily on Rust async Futures for lazy evaluation.  Unlike `bazel`, it does not hold an exclusive lock during execution and does not use a separate server process.

## Core Technologies

*   **Language**: Rust
*   **Asynchronous Runtime**: Tokio
*   **gRPC Framework**: Tonic
*   **Tracing and Logging**: Fastrace

## Subcommands

The `razel` command must mimick the `bazel` command, with the same subcommands.  Command line flags may differ, but should follow bazel wherever possible.

## Development Guidelines

*   Follow standard Rust coding conventions.
*   Write unit and integration tests for new features.
*   Ensure code is well-documented.
*   Run `cargo fmt --all -- --check` to ensure proper formatting before committing Rust code.