use std::{fs::File, io::Write, path::Path};

use color_eyre::Result;

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use tracing::debug;

#[derive(Debug, Serialize, Deserialize)]
pub struct InterfaceConfig {
    pub if_name: String,
    pub ip_address: Ipv4Net,
    pub mac_address: String,
}

impl InterfaceConfig {
    pub fn new(if_name: String, ip_address: Ipv4Net, mac_address: String) -> Self {
        Self {
            if_name,
            ip_address,
            mac_address,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    // pub guest_default_username: String,
    // pub guest_default_password: String,
    pub max_parallel_vm_count: usize,
    pub net: Ipv4Net,
    pub host_ifname: String,
    /// Address of the bridge on the host
    pub host_address: Ipv4Net,
    pub interfaces: Vec<InterfaceConfig>,
}

impl Config {
    pub fn new(
        max_parallel_vm_count: usize,
        net: Ipv4Net,
        host_ifname: String,
        host_address: Ipv4Net,
        interfaces: Vec<InterfaceConfig>,
    ) -> Self {
        Self {
            max_parallel_vm_count,
            net,
            host_ifname,
            host_address,
            interfaces,
        }
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let contents = std::fs::read_to_string(path.as_ref())?;
        debug!("Read config {contents} from {}", path.as_ref().display());
        Ok(serde_json::from_str(&contents)?)
    }

    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        let contents = serde_json::to_string(self)?;
        let mut file = File::create_new(path.as_ref())?;
        debug!("Writing config {contents} to {}", path.as_ref().display());
        file.write_all(&contents.as_bytes())?;
        Ok(())
    }
}
