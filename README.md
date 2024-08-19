# Codepot - WIP

An online coding scratchpad for Rust, C++/C, Zig and Go using Firecracker MicroVMs, written in
Rust. Work-in-Progress.

## Setup
To download and build the necessary images, use `codepot init`. Required utilities for image generation:
- `buildah` (and `fuse-overlayfs`)
- `mkfs.ext4` (`e2fsprogs`)




Idea: put ssh key as boot kernel param and then parse it from /proc/cmdline, put it into authorized keys
