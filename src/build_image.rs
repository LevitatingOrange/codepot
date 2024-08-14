//! Build a root file image capable of running the compilers for different programming languages for the firecracker VM.

use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

use color_eyre::eyre::{bail, ensure, Context, Result};
use rand::distributions::{Alphanumeric, DistString};
use tempfile::TempDir;
use tracing::{debug, error, info};

/// Build up the file image by using `buildah` to build up an alpine container with the necessary tools installed.
///
/// Note that the drop implementation is blocking, so building an image should best not be done from an async context.
#[derive(Debug)]
pub struct EphemeralContainer {
    container_id: String,
    username: String,
    password: String,
}

impl EphemeralContainer {
    const BUILDAH_PATH: &'static str = "buildah";

    pub fn username(&self) -> &str {
        &self.username
    }
    pub fn password(&self) -> &str {
        &self.password
    }

    /// Start building the container
    fn new() -> Result<Self> {
        // Hardcoded at the moment
        const BASE_IMAGE: &str = "alpine:3.20";
        const USERNAME: &str = "alpine";

        let output = Command::new(Self::BUILDAH_PATH)
            .arg("from")
            .arg(BASE_IMAGE)
            .output()
            .context("Could not create ephemeral container")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Could not create ephemeral container: {}", stderr.trim());
        }

        let container_id = String::from_utf8(output.stdout)
            .context("Could not create ephemeral container")?
            .trim()
            .to_owned();

        ensure!(
            container_id.is_ascii() && !container_id.contains(['\n', '\t', '\r']),
            "Could not create ephemeral container: Invalid output from buildah: {container_id}"
        );

        let password = Alphanumeric.sample_string(&mut rand::thread_rng(), 16);

        debug!("Created ephemeral container with id {container_id}");

        Ok(Self {
            container_id,
            username: USERNAME.to_owned(),
            password,
        })
    }

    /// Run a single command in the working container.
    fn run(&self, cmd: impl AsRef<OsStr>) -> Result<()> {
        let output = Command::new(Self::BUILDAH_PATH)
            .arg("run")
            .arg(&self.container_id)
            .arg("--")
            .arg("sh")
            .arg("-c")
            .arg(&cmd)
            .output()
            .with_context(|| {
                format!(
                    "Could not run \"{}\" in container",
                    cmd.as_ref().to_string_lossy()
                )
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Could not create ephemeral container: {}", stderr.trim());
        }

        Ok(())
    }

    /// Setup the container by installing necessary packages and tools
    fn setup(&self) -> Result<()> {
        // TODO: Dropbear, https://gruchalski.com/posts/2021-02-13-launching-alpine-linux-on-firecracker-like-a-boss/

        // Install necessary packages
        debug!("Installing packages");
        self.run("apk update")?;
        self.run("apk add openrc sudo util-linux")?;

        // Setup user account
        debug!("Setting up user account");
        self.run(format!("mkdir -p /home/{0}/", self.username))?;
        self.run(format!("addgroup -S {0}", self.username))?;
        self.run(format!(
            "adduser -S {0} -G {0} -h /home/{0} -s /bin/sh",
            self.username
        ))?;
        self.run(format!(
            "echo \"{0}:{1}\" | chpasswd",
            self.username, self.password
        ))?;
        self.run(format!(
            "echo \"%{0} ALL=(ALL) NOPASSWD: ALL\" > /etc/sudoers.d/{0}",
            self.username
        ))?;

        // Setup auto-login for the serial console
        debug!("Setting up auto-login");
        self.run(
            "ln -s agetty /etc/init.d/agetty.ttyS0 \
                 && echo ttyS0 > /etc/securetty",
        )
        .context("Could not setup auto-login")?;

        // Setup necessary system jobs.
        debug!("Setting up system jobs");
        self.run(
            "rc-update add agetty.ttyS0 default \
                 && rc-update add devfs boot \
                 && rc-update add procfs boot \
                 && rc-update add sysfs boot \
                 && rc-update add local default",
        )
        .context("Could not setup system jobs")?;

        // Setup rc
        debug!("Setting up RC");
        self.run(
            "mkdir /run/openrc \
                 && touch /run/openrc/softlevel",
        )
        .context("Could not setup RC")?;

        Ok(())
    }

    /// Build the ephemeral container.
    pub fn build() -> Result<Self> {
        info!("Building ephemeral container");
        let this = Self::new()?;
        this.setup()?;
        Ok(this)
    }

    /// Build an image of the given size (in bytes) from the container and put it at the specified path.
    pub fn to_image(self, image_path: impl AsRef<Path>, image_size: u64) -> Result<()> {
        let mut image = File::create_new(&image_path).context("Could not create image file")?;
        io::copy(&mut io::repeat(0).take(image_size), &mut image)?;
        image.flush()?;

        let mkfs_output = Command::new("mkfs.ext4")
            .arg(image_path.as_ref())
            .output()?;
        if !mkfs_output.status.success() {
            let stderr = String::from_utf8_lossy(&mkfs_output.stderr);
            bail!(
                "Could not delete ephemeral container {}: {}",
                self.container_id,
                stderr.trim()
            );
        }

        let container_mnt_output = Command::new(Self::BUILDAH_PATH)
            .arg("unshare")
            .arg("sh")
            .arg("-c")
            .arg(format!("buildah mount {}", self.container_id))
            .output()?;
        if !container_mnt_output.status.success() {
            let stderr = String::from_utf8_lossy(&container_mnt_output.stderr);
            bail!(
                "Could not mount ephemeral container {}: {}",
                self.container_id,
                stderr.trim()
            );
        }

        let container_mnt_path = PathBuf::from(
            String::from_utf8(container_mnt_output.stdout)
                .context("Could not mount ephemeral containers' rootfs")?
                .trim()
                .to_owned(),
        );
        ensure!(
            container_mnt_path.try_exists()?,
            "Could not mount ephemeral containers' rootfs"
        );
        debug!(
            "Ephemeral containers rootfs mounted at {}",
            container_mnt_path.display()
        );

        let image_mnt_dir = TempDir::new()?;
        // TODO: Use unshare to not require sudo
        let image_mnt_output = Command::new("sudo")
            .arg("mount")
            .arg(image_path.as_ref())
            .arg(image_mnt_dir.path())
            .output()?;
        if !image_mnt_output.status.success() {
            let stderr = String::from_utf8_lossy(&image_mnt_output.stderr);
            bail!("Could not mount image: {}", stderr.trim());
        }
        debug!("Image mounted at {}", image_mnt_dir.path().display());

        let cp_output = Command::new("sudo")
            .arg("cp")
            .arg("-r")
            .arg(&container_mnt_path)
            .arg(image_mnt_dir.path())
            .output()?;

        if !cp_output.status.success() {
            let stderr = String::from_utf8_lossy(&cp_output.stderr);
            bail!("Could not cp image contents: {}", stderr.trim());
        }

        info!(
            "Created image at {} with size {image_size}",
            image_path.as_ref().display()
        );
        Ok(())
    }
}

// impl Drop for EphemeralContainer {
//     fn drop(&mut self) {
//         let output = Command::new(Self::BUILDAH_PATH)
//             .arg("rm")
//             .arg(&self.container_id)
//             .output();
//         match output {
//             Ok(output) => {
//                 if !output.status.success() {
//                     let stderr = String::from_utf8_lossy(&output.stderr);
//                     error!(
//                         "Could not delete ephemeral container {}: {}",
//                         self.container_id,
//                         stderr.trim()
//                     );
//                 }
//             }
//             Err(err) => error!(
//                 "Could not delete ephemeral container {}: {}",
//                 self.container_id, err
//             ),
//         }
//     }
// }
