# win-rust-pool

A fixed-size thread pool for Windows using WinAPI with prioritized queues, closure support, returnable tasks, safe critical section guard, and monitoring statistics.

## Overview

This crate provides a low-level implementation of a WinAPI-based thread pool. It exposes:

* Raw task submission via WinAPI function pointers
* `FnOnce()` closures support
* Tasks with return values (`u64`)
* Monitoring statistics (enqueue count, execution time, wait time)

> **Note:** The core implementation in this repository is taken directly from an existing project. It uses `unsafe` and global `static mut` state for simplicity.
>
> **For integration into your own codebase,** it is strongly recommended to wrap this low-level module with safe, idiomatic Rust APIs.

## Getting Started

```bash
cargo run --bin demo
```

This will:

* Submit high- and low-priority raw tasks
* Submit closure-based tasks
* Submit tasks with return values and collect results
* Print pool statistics
* Shutdown the pool
