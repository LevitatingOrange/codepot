use std::path::{Path, PathBuf};

use argh::FromArgs;
use color_eyre::{
    eyre::{ensure, Context},
    Result,
};

use config::Config;
use init::{init_images, init_networking};
use ipnet::Ipv4Net;
use machine::config::MachineConfigurator;
use rand::distributions::{Alphanumeric, DistString};
use serde::{Deserialize, Serialize};
use tracing::warn;

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
    Ipv4Net::new("10.128.64.1".parse().unwrap(), 24).unwrap()
}

fn default_guest_username() -> String {
    "codepot".to_owned()
}

fn default_guest_password() -> String {
    Alphanumeric.sample_string(&mut rand::thread_rng(), 16)
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
    Run(Run),
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

    /// username for the user account inside the guest.
    #[argh(option, default = "default_guest_username()")]
    username: String,

    /// password for the user account inside the guest.
    #[argh(option, default = "default_guest_password()")]
    password: String,
}

#[derive(FromArgs, PartialEq, Debug)]
/// Start the server.
#[argh(subcommand, name = "run")]
struct Run {}

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
            username,
            password,
        }) => {
            let rootfs_size = rootfs_size * 1024 * 1024;
            init_images(
                &kernel_image_path,
                &rootfs_image_path,
                rootfs_size,
                username,
                password,
            )
            .context("Could not initialize images")?;

            if !config_path.try_exists()? {
                let (interfaces, host_address) =
                    init_networking(max_parallel_vm_count, &host_interface, net)
                        .context("Could not setup networking")?;
                Config::new(
                    max_parallel_vm_count,
                    net,
                    host_interface,
                    host_address,
                    interfaces,
                )
                .write(&config_path)
                .with_context(|| format!("Could not write config to {}", config_path.display()))?;
            } else {
                warn!("Config already present at {}, skipping network setup (note that this could lead to inconsistencies, best run `codepot deinit` and `codepot init` to get consistent network and image configuration)", config_path.display());
            }
        }
        Subcommand::Run(Run {}) => {
            for p in &[&kernel_image_path, &rootfs_image_path, &config_path] {
                ensure!(p.try_exists()?, "Not inited yet, please run `codepot init` to create necessary images and setup networking");
            }
            let config = Config::read(&config_path)
                .with_context(|| format!("Could not read config from {}", config_path.display()))?;

            let iface = &config.interfaces[0];
            let configurator = MachineConfigurator::new(
                kernel_image_path,
                rootfs_image_path,
                2,
                512,
                config.host_address.addr(),
                &iface.if_name,
                &iface.mac_address,
                iface.ip_address,
                "foo",
            );
            configurator.store()?;
        }
    }

    Ok(())
}
