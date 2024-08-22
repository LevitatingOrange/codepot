# Codepot - WIP

An online coding scratchpad for Rust, C++/C, Zig and Go using Firecracker MicroVMs, written in
Rust. Work-in-Progress.

## Setup
To download and build the necessary images, use `codepot init`. Required utilities for image generation:
- `buildah` (and `fuse-overlayfs`)
- `mkfs.ext4` (`e2fsprogs`)


## TODOs
- [ ] Use initrd for rootfs, have an extra home directory that is an in-memory image. (https://github.com/marcov/firecracker-initrd )
- [ ] Enable logging and use boot timer so we know how long a VM needs to startup
- [ ] Support for arm (look at the config, boot params, the downloaded kernel, the boot signaler and rust installation)
- [ ] Install Rust, go and zig into the image
- [ ] SSH device gen at image build or at runtime?
- [ ] Initrd takes too much memory
