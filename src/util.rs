use std::{ffi::OsStr, process::Command};

use color_eyre::{eyre::bail, Result};

pub fn run_sudo(command: impl AsRef<OsStr>) -> Result<()> {
    let output = Command::new("sudo")
        .arg("sh")
        .arg("-c")
        .arg(command)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Command exited with non zero exit code {}: {}",
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}
