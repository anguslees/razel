# Razel

Razel (pronounced "razzle") is a new build tool that aims to be a drop-in replacement for Bazel. It is written in Rust and designed for performance and scalability.

## Features

*   **Bazel Compatibility**: Razel aims to be compatible with existing Bazel projects and BUILD files.
*   **Performance**: Leveraging Rust's performance and concurrency features, Razel is designed to be fast.
*   **Remote first**: Razel leans heavily on [RBE](https://bazel.build/remote/rbe).
*   **Concurrent**: Razel does not use a workspace-wide lock.  Multiple `razel build` commands may execute concurrently.
*   **Serverless**: Razel does not have a separate server process and instead shares state via the local cache.
*   **Modern Tooling**: Built with modern Rust libraries like Tokio, Tonic, and Fastrace.

## Status

This project is in its very early stages of development.
