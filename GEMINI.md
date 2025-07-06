# Razel Project Details for AI Agents

This document outlines key architectural and technological choices for the Razel project.

## Core Technologies

*   **Language**: Rust
    *   Chosen for its performance, memory safety, and concurrency features.
*   **Asynchronous Runtime**: Tokio
    *   To enable high-concurrency and efficient I/O operations.
*   **gRPC Framework**: Tonic
    *   For communication between Razel components and potentially with other services, compatible with Bazel's gRPC APIs.
*   **Tracing and Logging**: Fastrace
    *   For detailed performance tracing and logging, helping in diagnostics and optimization.

## Subcommands

The primary interface will be through subcommands mirroring Bazel's CLI.

*   `razel version`: Displays the current version of Razel.
*   Other subcommands (e.g., `build`, `test`, `run`, `query`): Initially, these will be placeholders (`unimplemented!()`) and will be developed incrementally.

## Development Guidelines

*   Follow standard Rust coding conventions.
*   Write unit and integration tests for new features.
*   Ensure code is well-documented.
