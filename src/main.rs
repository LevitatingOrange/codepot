use std::path::{Path, PathBuf};

use argh::FromArgs;
use build_image::EphemeralContainer;
use color_eyre::{eyre::ensure, Result};

mod build_image;

fn default_vm_assets_path() -> PathBuf {
    Path::new("vm/").to_owned()
}

fn default_rootfs_size_mb() -> u64 {
    800
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

    match args.subcommand {
        Subcommand::Init(Init { rootfs_size }) => {
            let rootfs_size = rootfs_size * 1024 * 1024;
            ensure!(
                !rootfs_image_path.try_exists()?,
                "Rootfs image already exists at {}",
                rootfs_image_path.display()
            );

            let container = EphemeralContainer::build()?;

            println!(
                "Default user is {}, password is {}",
                container.username(),
                container.password()
            );
            container.to_image(&rootfs_image_path, rootfs_size)?;
        }
    }

    Ok(())
}
