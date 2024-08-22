//! Build a root file image capable of running the compilers for different programming languages for the firecracker VM.

use std::{
    cell::OnceCell,
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, BufWriter, Read, Write},
    ops::DerefMut,
    path::Path,
    process::Command,
    sync::LazyLock,
};

use color_eyre::eyre::{bail, ensure, Context, Result};
use rand::distributions::{Alphanumeric, DistString};
use reqwest::Url;
use scopeguard::guard;
use tempfile::{NamedTempFile, TempDir};
use tracing::{debug, error, info, warn};

use crate::util::run_sudo;

const LATEST_KERNEL_IMAGE: &'static str =
    "spec.ccfc.min/firecracker-ci/v1.9/x86_64/vmlinux-5.10.219-no-acpi";
static KERNEL_IMAGE_DOWNLOAD_URL: LazyLock<Url> = LazyLock::new(|| {
    let mut url = Url::parse("https://s3.amazonaws.com/").unwrap();
    url.set_path(LATEST_KERNEL_IMAGE);
    url
});

const GET_CMDLINE_KEY_SCRIPT: &'static str = include_str!("../../vm_utils/get_cmdline_key");
const IFUPDOWN_EXECUTOR_SCRIPT: &'static str = include_str!("../../vm_utils/cmdline_static");
const INTERFACES_CONFIG: &'static str = include_str!("../../vm_utils/interfaces");
const MOTD: &'static str = include_str!("../../vm_utils/motd");

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
    const RUSTUP_URL: &'static str =
        "https://static.rust-lang.org/rustup/archive/1.27.1/x86_64-unknown-linux-musl/rustup-init";
    const RUSTUP_SHA256: &'static str =
        "1455d1df3825c5f24ba06d9dd1c7052908272a2cae9aa749ea49d67acbe22b47";
    const RUST_VERSION: &'static str = "1.80.1";
    const INITRD_PATH: &'static str = "/initrd";
    const BUILD_DIR: &'static str = "/build";

    fn username(&self) -> &str {
        &self.username
    }
    fn password(&self) -> &str {
        &self.password
    }

    /// Start building the container
    fn new(username: String, password: String) -> Result<Self> {
        // Hardcoded at the moment
        const BASE_IMAGE: &str = "alpine:3.20";
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

        debug!("Created ephemeral container with id {container_id}");

        Ok(Self {
            container_id,
            username,
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
    /// Run a single command chrooted.
    fn chroot_in(&self, cmd: impl AsRef<OsStr>, dir: impl AsRef<Path>) -> Result<()> {
        let output = Command::new(Self::BUILDAH_PATH)
            .arg("run")
            .arg(&self.container_id)
            .arg("--")
            .arg("chroot")
            .arg(&dir.as_ref())
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

    /// Run a single command in the working container inside a directory.
    fn run_in(&self, cmd: impl AsRef<OsStr>, dir: impl AsRef<Path>) -> Result<()> {
        let output = Command::new(Self::BUILDAH_PATH)
            .arg("run")
            .arg("--workingdir")
            .arg(dir.as_ref())
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

    /// Copy a file into the container.
    fn copy(
        &self,
        from_host: impl AsRef<Path>,
        to_container: impl AsRef<Path>,
        permissions: &str,
    ) -> Result<()> {
        let output = Command::new(Self::BUILDAH_PATH)
            .arg("copy")
            .arg("--chmod")
            .arg(permissions)
            .arg(&self.container_id)
            .arg(from_host.as_ref())
            .arg(to_container.as_ref())
            .output()
            .with_context(|| {
                format!(
                    "Could not copy from host \"{}\" to \"{}\" in container",
                    from_host.as_ref().display(),
                    to_container.as_ref().display()
                )
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Could not create ephemeral container: {}", stderr.trim());
        }

        Ok(())
    }

    fn add_file_contents(
        &self,
        path: impl AsRef<Path>,
        contents: &str,
        permissions: &str,
    ) -> Result<()> {
        let mut temp = NamedTempFile::new()?;
        temp.write_all(&contents.as_bytes())?;
        temp.flush()?;
        debug!(
            "Adding file in {} with permissions {}",
            path.as_ref().display(),
            permissions
        );
        self.copy(temp.path(), &path, permissions)
            .context("Could not add file contents")?;
        Ok(())
    }

    fn add_file_contents_to_build(
        &self,
        path: impl AsRef<Path>,
        contents: &str,
        permissions: &str,
    ) -> Result<()> {
        self.add_file_contents(Path::new(Self::BUILD_DIR).join(path), contents, permissions)
    }

    fn install_rust(&self) -> Result<()> {
        debug!("Installing rust");
        self.run(format!("wget {}", Self::RUSTUP_URL))?;
        self.run(format!(
            "echo '{} *rustup-init' | sha256sum -c && chmod +x ./rustup-init",
            Self::RUSTUP_SHA256
        ))?;
        self.run(format!("RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo \
                              ./rustup-init -y --no-modify-path --profile minimal --default-toolchain {} --default-host x86_64-unknown-linux-musl", Self::RUST_VERSION))?;
        self.run_in(
            "cp -r /usr/local/rustup ./usr/local/rustup && \
                     cp -r /usr/local/cargo ./usr/local/cargo",
            Self::BUILD_DIR,
        )?;
        self.run_in(
            "echo '$PATH=\"$PATH:/usr/local/cargo/bin\"' >> ./etc/profile",
            Self::BUILD_DIR,
        )?;

        Ok(())
    }

    /// Setup the container by installing necessary packages and tools
    fn setup(&self) -> Result<()> {
        const GUEST_PACKAGES: [&'static str; 7] = [
            "alpine-base",
            "openrc",
            "util-linux",
            "dropbear",
            "grep",
            "doas",
            "rust",
        ];

        debug!("Creating root dir");
        self.run(format!("mkdir {}", Self::BUILD_DIR))?;

        // Install necessary packages
        debug!("Installing packages");
        self.run("apk update")?;
        // Add package to builder
        self.run("apk add dropbear ca-certificates gcc")?;
        self.run(format!(
            "apk -X http://dl-5.alpinelinux.org/alpine/latest-stable/main -U --allow-untrusted --root {} --initdb add{}",
            Self::BUILD_DIR,
            GUEST_PACKAGES.iter().fold(String::new(), |mut acc, s| {
                acc.push(' ');
                acc.push_str(s);
                acc
            })
        ))?;

        self.run_in(
            "cp /etc/apk/repositories ./etc/apk/repositories",
            Self::BUILD_DIR,
        )?;

        // Setup user account
        debug!("Setting up user account");
        self.run(format!(
            "mkdir -p {0}/home/{1}/",
            Self::BUILD_DIR,
            self.username
        ))?;

        self.chroot_in(
            format!(
                "addgroup -g {2} -S {0} && \
                                adduser -u {1} -S {0} -G {0} -G wheel -h /home/{0} -s /bin/sh && \
                                echo \"{0}:{3}\" | chpasswd",
                self.username, self.uid, self.gid, self.password
            ),
            Self::BUILD_DIR,
        )?;
        self.run_in(
            "echo 'permit nopass :wheel' > ./etc/doas.d/doas.conf",
            Self::BUILD_DIR,
        )?;

        // Setup auto-login for the serial console
        debug!("Setting up getty");
        self.run_in(
            format!(
                "ln -s agetty ./etc/init.d/agetty.ttyS0 \
             && echo ttyS0 > ./etc/securetty \
             && ln -sf /etc/init.d/agetty.ttyS0 ./etc/runlevels/default/agetty.ttyS0"
            ),
            Self::BUILD_DIR,
        )
        .context("Could not setup getty")?;

        // Setup necessary system jobs.
        debug!("Setting up VM startup");
        self.run_in(
            "ln -sf /etc/init.d/devfs  ./etc/runlevels/boot/devfs && \
             ln -sf /etc/init.d/procfs     ./etc/runlevels/boot/procfs && \
             ln -sf /etc/init.d/sysfs      ./etc/runlevels/boot/sysfs && \
             ln -sf networking             ./etc/init.d/net.eth0 && \
             ln -sf /etc/init.d/networking ./etc/runlevels/default/networking && \
             ln -sf /etc/init.d/net.eth0   ./etc/runlevels/default/net.eth0 && \
             ln -sf dropbearr              ./etc/init.d/dropbear.eth0",
            Self::BUILD_DIR,
        )
        .context("Could not setup system jobs")?;

        // Setup rc
        debug!("Setting up RC");
        self.run_in(
            "mkdir ./run/openrc \
             && touch ./run/openrc/softlevel",
            Self::BUILD_DIR,
        )
        .context("Could not setup RC")?;

        debug!("Copying files...");
        self.add_file_contents_to_build(
            "usr/local/bin/get_cmdline_key",
            GET_CMDLINE_KEY_SCRIPT,
            "755",
        )
        .context("Could not add cmdline get script")?;
        self.add_file_contents_to_build(
            "usr/libexec/ifupdown-ng/cmdline_static",
            IFUPDOWN_EXECUTOR_SCRIPT,
            "755",
        )
        .context("Could not add ifupdown executor script")?;
        self.add_file_contents_to_build("etc/network/interfaces", INTERFACES_CONFIG, "644")
            .context("Could not add interfaces config")?;
        self.add_file_contents_to_build("etc/motd", MOTD, "644")
            .context("Could not add motd")?;
        self.run_in(
            "echo 'DROPBEAR_OPTS=\"-w -j\"' > ./etc/conf.d/dropbear",
            Self::BUILD_DIR,
        )?; // '-s' to disable password logins

        //self.install_rust()?;

        Ok(())
    }

    /// Build the ephemeral container.
    fn build(username: String, password: String) -> Result<Self> {
        info!("Building ephemeral container");
        let this = Self::new(username, password)?;
        this.setup()?;
        Ok(this)
    }

    /// Build an image of the given size (in bytes) from the container and put it at the specified path.
    fn build_initrd(self, initrd_path: impl AsRef<Path>) -> Result<()> {
        info!("Creating image");

        // TODO: custom init (see https://github.com/marcov/firecracker-initrd/blob/master/container/build-initrd-in-ctr.sh)
        self.run_in(format!("ln -sf /sbin/init ./init"), Self::BUILD_DIR)?;
        self.run_in(
            format!(
                "find . -print0 | cpio --null --create --verbose --format=newc | tee > {}",
                Self::INITRD_PATH
            ),
            Self::BUILD_DIR,
        )?;

        info!("Copying initrd to host");

        let mut unshare_arg: OsString =
            OsString::from(format!("cp -r $MNT_PATH/{} ", Self::INITRD_PATH));
        unshare_arg.push(&initrd_path.as_ref());

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
    initrd_path: &Path,
    username: String,
    password: String,
) -> Result<()> {
    if initrd_path.try_exists()? {
        warn!(
            "initrd already exists at {}, not building it",
            initrd_path.display()
        );
    } else {
        let container = EphemeralContainer::build(username, password)?;

        println!(
            "Default user is {}, password is {}",
            container.username(),
            container.password()
        );
        container.build_initrd(&initrd_path)?;
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
