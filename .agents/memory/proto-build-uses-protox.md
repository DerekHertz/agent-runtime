---
name: proto-build-uses-protox
description: proto/runtime.proto compiles with protox + tonic-prost-build 0.14, no system protoc
metadata:
  type: project
---

`crates/proto/build.rs` compiles `proto/runtime.proto` with the pure-Rust `protox` crate and `tonic_prost_build::configure().compile_fds(..)` (tonic/prost 0.14). The machine has no `protoc`.

**Why:** keeps the build self-contained; adding a protoc dependency would add a system protoc requirement to fresh checkouts.

**How to apply:** when later issues add RPCs (AfterTool, Release, Attach, Orchestration), extend `proto/runtime.proto` only; don't switch to `tonic_build::compile_protos`, which needs protoc.
