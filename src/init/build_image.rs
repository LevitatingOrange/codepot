//! Build a root file image capable of running the compilers for different programming languages for the firecracker VM.

use std::{
    cell::OnceCell,
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, BufWriter, Read, Write},
    ops::DerefMut,
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

use color_eyre::eyre::{bail, ensure, Context, Result};
use rand::distributions::{Alphanumeric, DistString};
use reqwest::Url;
use scopeguard::guard;
use tempfile::TempDir;
use tracing::{debug, error, info, warn};

use crate::util::run_sudo;

const LATEST_KERNEL_IMAGE: &'static str =
    "spec.ccfc.min/firecracker-ci/v1.9/x86_64/vmlinux-5.10.219-no-acpi";
static KERNEL_IMAGE_DOWNLOAD_URL: LazyLock<Url> = LazyLock::new(|| {
    let mut url = Url::parse("https://s3.amazonaws.com/").unwrap();
    url.set_path(LATEST_KERNEL_IMAGE);
    url
});

/// Build up the file image by using `buildah` to build up an alpine container with the necessary tools installed.
///
/// Note that the drop implementation is blocking, so building an image should not be done from an async context.
#[derive(Debug)]
struct EphemeralContainer {
    container_id: String,
    username: String,
    password: String,
    uid: u32,
    gid: u32,
}

impl EphemeralContainer {
    const BUILDAH_PATH: &'static str = "buildah";

    fn username(&self) -> &str {
        &self.username
    }
    fn password(&self) -> &str {
        &self.password
    }

    /// Start building the container
    fn new() -> Result<Self> {
        // Hardcoded at the moment
        const BASE_IMAGE: &str = "alpine:3.20";
        const USERNAME: &str = "alpine";
        const UID: u32 = 1000;
        const GID: u32 = 1000;

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
            uid: UID,
            gid: GID,
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
        const PACKAGES: [&'static str; 5] = ["openrc", "sudo", "util-linux", "dropbear", "clang"];

        // TODO: Dropbear, https://gruchalski.com/posts/2021-02-13-launching-alpine-linux-on-firecracker-like-a-boss/

        // Install necessary packages
        debug!("Installing packages");
        self.run("apk update")?;
        self.run(format!(
            "apk add{}",
            PACKAGES.iter().fold(String::new(), |mut acc, s| {
                acc.push(' ');
                acc.push_str(s);
                acc
            })
        ))?;

        // Setup user account
        debug!("Setting up user account");
        self.run(format!("mkdir -p /home/{0}/", self.username))?;
        self.run(format!("addgroup -g {0} -S {1}", self.gid, self.username))?;
        self.run(format!(
            "adduser -u {0} -S {1} -G {1} -h /home/{1} -s /bin/sh",
            self.uid, self.username
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
                 && rc-update add local default \
                 && rc-update add dropbear",
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
    fn build() -> Result<Self> {
        info!("Building ephemeral container");
        let this = Self::new()?;
        this.setup()?;
        Ok(this)
    }

    /// Build an image of the given size (in bytes) from the container and put it at the specified path.
    fn to_image(self, image_path: impl AsRef<Path>, image_size: u64) -> Result<()> {
        info!("Creating image");
        let defused = OnceCell::new();

        let image = File::create_new(&image_path).context("Could not create image file")?;
        let mut image = guard(image, |image| {
            drop(image);
            if defused.get().is_none() {
                debug!("Removing image because creation was not successful");
                let _ = std::fs::remove_file(&image_path);
            }
        });

        io::copy(&mut io::repeat(0).take(image_size), image.deref_mut())?;
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

        let temp_dir = TempDir::new()?;
        let mount_dir =
            Path::new("/mnt").join(Alphanumeric.sample_string(&mut rand::thread_rng(), 8));

        let mut unshare_arg: OsString = OsString::from(r"cp -r $MNT_PATH/* ");
        unshare_arg.push(temp_dir.path());

        let cp_output = Command::new(Self::BUILDAH_PATH)
            .arg("unshare")
            .arg("--mount")
            .arg(format!("MNT_PATH={}", self.container_id))
            .arg("sh")
            .arg("-c")
            .arg(unshare_arg)
            .output()?;

        if !cp_output.status.success() {
            let stderr = String::from_utf8_lossy(&cp_output.stderr);
            bail!(
                "Could not mount ephemeral container {}: {}",
                self.container_id,
                stderr.trim()
            );
        }

        if !cp_output.status.success() {
            let stderr = String::from_utf8_lossy(&cp_output.stderr);
            bail!("Could not cp container contents: {}", stderr.trim());
        }

        // TODO: find a way to do this without `sudo`
        let mut cp_arg = OsString::from("mkdir ");
        cp_arg.push(&mount_dir);
        cp_arg.push(" && mount ");
        cp_arg.push(image_path.as_ref());
        cp_arg.push(" ");
        cp_arg.push(&mount_dir);
        cp_arg.push(" && cp -r ");
        cp_arg.push(temp_dir.path());
        cp_arg.push("/* ");
        cp_arg.push(&mount_dir);
        cp_arg.push(" && chown root:root ");
        cp_arg.push(&mount_dir);
        cp_arg.push(format!("/* && chown {}:{} ", self.uid, self.gid));
        cp_arg.push(mount_dir.join(format!("home/{}", self.username)));
        cp_arg.push(" && umount ");
        cp_arg.push(&mount_dir);

        debug!("Running sudo command: {}", cp_arg.to_string_lossy());

        run_sudo(cp_arg).context("Could not copy files from container to image")?;

        info!(
            "Created image at {} with size {image_size}",
            image_path.as_ref().display()
        );

        defused.get_or_init(|| ());

        Ok(())
    }
}

impl Drop for EphemeralContainer {
    fn drop(&mut self) {
        let output = Command::new(Self::BUILDAH_PATH)
            .arg("rm")
            .arg(&self.container_id)
            .output();
        match output {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    error!(
                        "Could not delete ephemeral container {}: {}",
                        self.container_id,
                        stderr.trim()
                    );
                }
            }
            Err(err) => error!(
                "Could not delete ephemeral container {}: {}",
                self.container_id, err
            ),
        }
    }
}

/// Create and download necessary kernel and rootfs images.
pub fn init_images(
    kernel_image_path: &Path,
    rootfs_image_path: &Path,
    rootfs_size: u64,
) -> Result<()> {
    if rootfs_image_path.try_exists()? {
        warn!(
            "RootFS image already exists at {}, not building it",
            rootfs_image_path.display()
        );
    } else {
        let container = EphemeralContainer::build()?;

        println!(
            "Default user is {}, password is {}",
            container.username(),
            container.password()
        );
        container.to_image(&rootfs_image_path, rootfs_size)?;
    }

    if kernel_image_path.try_exists()? {
        warn!(
            "Kernel image already exists at {}, not downloading it",
            kernel_image_path.display()
        );
    } else {
        info!(
            "Downloading image from {} and putting it into {}",
            *KERNEL_IMAGE_DOWNLOAD_URL,
            kernel_image_path.display()
        );
        let image_contents = reqwest::blocking::get(KERNEL_IMAGE_DOWNLOAD_URL.clone())
            .context("Could not download kernel image")?;

        let mut file = BufWriter::new(File::create(kernel_image_path)?);
        std::io::copy(&mut image_contents.bytes()?.as_ref(), &mut file)?;
    }
    Ok(())
}
