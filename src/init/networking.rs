use std::collections::HashSet;

use color_eyre::{
    eyre::{ensure, Context, OptionExt},
    Result,
};
use ipnet::Ipv4Net;
use rand::distributions::{Alphanumeric, DistString};
use tracing::{debug, info};

use crate::{config::InterfaceConfig, util::run_sudo};

const BRIDGE_NAME: &'static str = "codepot0";

fn random_if_name() -> String {
    format!(
        "vethcdpt{}",
        Alphanumeric.sample_string(&mut rand::thread_rng(), 6)
    )
}

// TODO: We need to figure out how to do networking for multiple vms:
// - MAC addresses and IPs need to be setup
// - Multiple tuns need to be setup

fn setup_tap_interface(if_name: &str) -> Result<()> {
    // Remove interface...
    run_sudo(format!("ip link del {if_name} 2> /dev/null || true"))?;

    // and create it again to be idempotent.
    run_sudo(format!("ip tuntap add {if_name} mode tap"))?;
    run_sudo(format!("ip link set dev {if_name} master {BRIDGE_NAME}"))?;
    run_sudo(format!("ip link set dev {if_name} up"))?;

    Ok(())
}

/// Configure host interface and ip table rules to do NAT.
fn setup_host_interface(host_if_name: &str, host_address: Ipv4Net) -> Result<()> {
    // Remove bridge
    run_sudo(format!("ip link del {BRIDGE_NAME} 2> /dev/null || true"))?;

    // Add bridge again to be idempotent.
    run_sudo(format!("ip link add name {BRIDGE_NAME} type bridge"))?;
    run_sudo(format!("ip addr add {host_address} dev {BRIDGE_NAME}"))?;
    run_sudo(format!("ip link set dev {BRIDGE_NAME} up"))?;

    // Remove exsting rules...
    run_sudo("iptables -D FORWARD -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT || true")?;
    run_sudo(format!(
        "iptables -t nat -D POSTROUTING -o {host_if_name} -j MASQUERADE || true"
    ))?;
    run_sudo(format!(
        "iptables -D FORWARD -i {BRIDGE_NAME} -j ACCEPT || true"
    ))?;

    // and apply them again to be idempotent.
    run_sudo("iptables -A FORWARD -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT")?;
    run_sudo(format!(
        "iptables -t nat -A POSTROUTING -o {host_if_name} -j MASQUERADE"
    ))?;
    run_sudo(format!("iptables -A FORWARD -i {BRIDGE_NAME} -j ACCEPT"))?;

    Ok(())
}

/// Initialize networking, returning the list of created interfaces and associated static IP addresses.
pub fn init_networking(
    max_parallel_vm_count: usize,
    host_if_name: &str,
    net: Ipv4Net,
) -> Result<(Vec<InterfaceConfig>, Ipv4Net)> {
    info!("Setting up networking");
    ensure!(
        max_parallel_vm_count + 1 <= net.hosts().count(),
        "More VMs than hostmask allows"
    );

    let mut ip_addresses = net.hosts();
    let host_address = Ipv4Net::new(
        ip_addresses
            .next()
            .ok_or_eyre("invalid hostmask specified")?,
        net.prefix_len(),
    )
    .unwrap();
    let mac_addresses = std::iter::once("06:00:AC:10:00:02".to_owned()); // TODO

    if max_parallel_vm_count > 1 {
        todo!();
    }

    // Make sure that we have `max_parallel_vm_count` unique interface names
    let ifs: Vec<_> = loop {
        let s: HashSet<_> = std::iter::repeat_with(|| random_if_name())
            .take(max_parallel_vm_count)
            .collect();
        if s.len() == max_parallel_vm_count {
            break s
                .into_iter()
                .zip(ip_addresses.map(|s| Ipv4Net::new(s, net.prefix_len()).unwrap()))
                .zip(mac_addresses)
                .map(|((n, a), b)| InterfaceConfig::new(n, a, b))
                .collect();
        }
    };

    // enable forwarding
    run_sudo(format!("echo 1 > /proc/sys/net/ipv4/ip_forward"))?;

    debug!("Setting up host interface {host_if_name}");
    setup_host_interface(host_if_name, host_address).context("could not setup host interface")?;
    for if_conf in &ifs {
        debug!("Setting up tap interface {}", if_conf.if_name);
        setup_tap_interface(&if_conf.if_name).context("could not setup tap interface")?;
    }

    Ok((ifs, host_address))
}

pub fn deinit_networking() -> Result<()> {
    todo!()
}
