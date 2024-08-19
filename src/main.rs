use std::path::{Path, PathBuf};

use argh::FromArgs;
use color_eyre::{
    eyre::{ensure, Context},
    Result,
};

use config::Config;
use init::{init_images, init_networking};
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};

mod config;
mod init;
mod machine;
mod util;

fn default_vm_assets_path() -> PathBuf {
    Path::new("vm/").to_owned()
}

fn default_rootfs_size_mb() -> u64 {
    800
}

fn default_max_parallel_vm_count() -> usize {
    16
}

fn default_private_net() -> Ipv4Net {
    Ipv4Net::new("172.16.0.1".parse().unwrap(), 24).unwrap()
}

#[derive(FromArgs)]
/// Reach new heights.
struct Codepot {
    /// path to where VM configuration and images lie.
    #[argh(option, default = "default_vm_assets_path()")]
    vm_assets: PathBuf,

    #[argh(subcommand)]
    subcommand: Subcommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
enum Subcommand {
    Init(Init),
}

#[derive(FromArgs, PartialEq, Debug)]
/// Initialize by downloading and building necessary images.
#[argh(subcommand, name = "init")]
struct Init {
    /// size of the VM rootfs image, in MB.
    #[argh(option, default = "default_rootfs_size_mb()")]
    rootfs_size: u64,

    /// maximum number of VMs allowed to coexist at the same time.
    #[argh(option, default = "default_max_parallel_vm_count()")]
    max_parallel_vm_count: usize,

    /// name of the interface on the host to use for NAT.
    #[argh(option)]
    host_interface: String,

    /// network for the microvms.
    #[argh(option, default = "default_private_net()")]
    net: Ipv4Net,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    let args: Codepot = argh::from_env();

    ensure!(
        args.vm_assets.try_exists()?,
        "VM assets path at {} does not exist, please create it and run `codepot init`",
        args.vm_assets.display(),
    );
    let kernel_image_path = args.vm_assets.join("kernel.img");
    let rootfs_image_path = args.vm_assets.join("rootfs.ext4");
    let config_path = args.vm_assets.join("config.json");

    match args.subcommand {
        Subcommand::Init(Init {
            rootfs_size,
            max_parallel_vm_count,
            host_interface,
            net,
        }) => {
            let rootfs_size = rootfs_size * 1024 * 1024;
            init_images(&kernel_image_path, &rootfs_image_path, rootfs_size)
                .context("Could not initialize images")?;

            let interfaces = init_networking(max_parallel_vm_count, &host_interface, net)
                .context("Could not setup networking")?;
            Config::new(max_parallel_vm_count, net, host_interface, interfaces)
                .write(&config_path)
                .context("Could not write config")?;
        }
    }

    Ok(())
}
