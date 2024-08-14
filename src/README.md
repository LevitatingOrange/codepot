# Codepot - WIP

A scratchpad for programming in Rust, Go, C/C++ and Zig all in the safety of an Amazon AWS firecracker VM.


## Setup
To download and build the necessary images, use `codepot init`. Required utilities for image generation:
- `buildah` (and `fuse-overlayfs`)
- `mkfs.ext4` (`e2fsprogs`)
