use std::{
    fs::File,
    io::{BufWriter, Write},
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use color_eyre::Result;
use ipnet::Ipv4Net;
use serde::Serialize;
use tempfile::tempfile;
use tracing::debug;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct BootArgs(String);

impl From<String> for BootArgs {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl BootArgs {
    pub const SSH_KEY_KEY: &'static str = "ssh_key";
    pub const STATIC_IP_KEY: &'static str = "static_ip";
    pub const GATEWAY_IP_KEY: &'static str = "gateway_ip";

    pub fn new() -> Self {
        Self::default()
    }

    /// Add another argument to the boot args.
    pub fn arg(&mut self, key: &str, value: &str) -> &mut Self {
        const PATTERN: [char; 2] = [' ', '='];
        self.0.push(' ');
        if key.contains(PATTERN) {
            self.0.push('"');
        }
        self.0.push_str(key);
        if key.contains(PATTERN) {
            self.0.push('"');
        }
        self.0.push('=');
        if value.contains(PATTERN) {
            self.0.push('"');
        }
        self.0.push_str(value);
        if value.contains(PATTERN) {
            self.0.push('"');
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BootSourceConfig {
    kernel_image_path: PathBuf,
    boot_args: BootArgs,
    initrd_path: Option<PathBuf>,
}

/// The engine file type, either Sync or Async (through io_uring).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
enum FileEngineType {
    /// Use an Async engine, based on io_uring.
    Async,
    /// Use a Sync engine, based on blocking system calls.
    #[default]
    Sync,
}

/// Use this structure to set up the Block Device before booting the kernel. Taken from https://github.com/firecracker-microvm/firecracker/blob/a364da806f8093e8d8ab1a8287be4a0efd4e4658/src/vmm/src/vmm_config/drive.rs#L29C1-L65C2.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct BlockDeviceConfig {
    /// Unique identifier of the drive.
    drive_id: String,
    /// Part-UUID. Represents the unique id of the boot partition of this device. It is
    /// optional and it will be used only if the `is_root_device` field is true.
    partuuid: Option<String>,
    /// If set to true, it makes the current device the root block device.
    /// Setting this flag to true will mount the block device in the
    /// guest under /dev/vda unless the partuuid is present.
    is_root_device: bool,
    // VirtioBlock specific fields
    /// If set to true, the drive is opened in read-only mode. Otherwise, the
    /// drive is opened as read-write.
    is_read_only: Option<bool>,
    /// Path of the drive.
    path_on_host: Option<PathBuf>,
    // /// Rate Limiter for I/O operations.
    // rate_limiter: Option<RateLimiterConfig>,
    /// The type of IO engine used by the device.
    #[serde(rename = "io_engine")]
    file_engine_type: Option<FileEngineType>,

    // VhostUserBlock specific fields
    /// Path to the vhost-user socket.
    socket: Option<String>,
}

/// Configuration of the microvm. Taken from https://github.com/firecracker-microvm/firecracker/blob/a364da806f8093e8d8ab1a8287be4a0efd4e4658/src/vmm/src/vmm_config/machine_config.rs#L175.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct MachineConfig {
    /// Number of vcpu to start.
    vcpu_count: u8,
    /// The memory size in MiB.
    mem_size_mib: usize,
    /// Enables or disabled SMT.
    smt: bool,
    /// Enables or disables dirty page tracking. Enabling allows incremental snapshots.
    track_dirty_pages: bool,
}

/// This struct represents the strongly typed equivalent of the json body from net iface
/// related requests. Taken from https://github.com/firecracker-microvm/firecracker/blob/a364da806f8093e8d8ab1a8287be4a0efd4e4658/src/vmm/src/vmm_config/net.rs#L19.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct NetworkInterfaceConfig {
    /// ID of the guest network interface.
    iface_id: String,
    /// Host level path for the guest network interface.
    host_dev_name: String,
    /// Guest MAC address.
    guest_mac: Option<String>,
    // /// Rate Limiter for received packages.
    // rx_rate_limiter: Option<RateLimiterConfig>,
    // /// Rate Limiter for transmitted packages.
    // tx_rate_limiter: Option<RateLimiterConfig>,
}

/// Used for configuring a vmm from one single json passed to the Firecracker process. Taken from https://github.com/firecracker-microvm/firecracker/blob/a364da806f8093e8d8ab1a8287be4a0efd4e4658/src/vmm/src/resources.rs#L63C1-L88C2.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct VmmConfig {
    #[serde(rename = "drives")]
    block_devices: Vec<BlockDeviceConfig>,
    #[serde(rename = "boot-source")]
    boot_source: BootSourceConfig,
    #[serde(rename = "cpu-config")]
    cpu_config: Option<PathBuf>,
    // #[serde(rename = "logger")]
    // logger: Option<crate::logger::LoggerConfig>,
    #[serde(rename = "machine-config")]
    machine_config: Option<MachineConfig>,
    // #[serde(rename = "metrics")]
    // metrics: Option<MetricsConfig>,
    // #[serde(rename = "mmds-config")]
    // mmds_config: Option<MmdsConfig>,
    #[serde(rename = "network-interfaces", default)]
    net_devices: Vec<NetworkInterfaceConfig>,
    // #[serde(rename = "vsock")]
    // vsock_device: Option<VsockDeviceConfig>,
    // #[serde(rename = "entropy")]
    // entropy_device: Option<EntropyDeviceConfig>,
}

pub struct MachineConfigurator(VmmConfig);

impl MachineConfigurator {
    /// Construct a new configurator from the given config values.
    pub fn new(
        kernel_image_path: impl AsRef<Path>,
        rootfs_image_path: impl AsRef<Path>,
        vcpu_count: u8,
        mem_size_mib: usize,
        host_address: Ipv4Addr,
        host_dev_name: &str,
        guest_mac: &str,
        ip_address: Ipv4Net,
        pub_ssh_key: &str,
    ) -> Self {
        let mut boot_args = BootArgs::from("console=ttyS0 reboot=k panic=1 pci=off".to_owned());
        boot_args
            .arg(BootArgs::SSH_KEY_KEY, pub_ssh_key)
            .arg(BootArgs::STATIC_IP_KEY, &ip_address.to_string())
            .arg(BootArgs::GATEWAY_IP_KEY, &host_address.to_string());

        Self(VmmConfig {
            block_devices: vec![BlockDeviceConfig {
                drive_id: "rootfs".to_owned(),
                partuuid: None,
                is_root_device: true,
                is_read_only: Some(false),
                path_on_host: Some(rootfs_image_path.as_ref().to_owned()),
                file_engine_type: Some(FileEngineType::Sync),
                socket: None,
            }],
            boot_source: BootSourceConfig {
                kernel_image_path: kernel_image_path.as_ref().to_owned(),
                boot_args,
                initrd_path: None,
            },
            cpu_config: None,
            machine_config: Some(MachineConfig {
                vcpu_count,
                mem_size_mib,
                smt: false,
                track_dirty_pages: false, // Needed for snapshotting
            }),
            net_devices: vec![NetworkInterfaceConfig {
                iface_id: "eth0".to_owned(),
                host_dev_name: host_dev_name.to_owned(),
                guest_mac: Some(guest_mac.to_owned()),
            }],
        })
    }

    /// Write the config out so that firecracker can consume it. Note that the file will be destroyed when the returned
    /// handle is dropped, so it should be held until firecracker started up.
    pub fn store(self) -> Result<File> {
        let mut file = tempfile()?;
        // Note: writing to a write is often slower than just storing the whole config (which is not that big) on the
        // heap and writing it out in one go.
        let contents = serde_json::to_string(&self.0)?;
        debug!(
            "Writing machine config {} to temporary config file",
            contents
        );
        file.write_all(&contents.as_bytes())?;
        Ok(file)
    }
}
