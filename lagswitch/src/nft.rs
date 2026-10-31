//! Load scripts into nft and list the devices they apply to.

use std::fs;
use std::io::{ErrorKind, Write};
use std::process::{Command, Stdio};

use crate::rules::{self, Direction};

/// Network devices the egress chain hooks. Loopback stays out so local IPC keeps working.
pub fn devices() -> Result<Vec<String>, String> {
    let dir = fs::read_dir("/sys/class/net").map_err(|e| format!("can't list devices: {e}"))?;
    let mut names: Vec<String> = dir
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name != "lo")
        .collect();
    names.sort();
    Ok(names)
}

fn load(script: &str) -> Result<(), String> {
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            ErrorKind::NotFound => "nft not found, install the nftables package".to_string(),
            _ => format!("can't start nft: {e}"),
        })?;
    let mut stdin = child.stdin.take().ok_or("can't write to nft")?;
    stdin.write_all(script.as_bytes()).map_err(|e| format!("can't write to nft: {e}"))?;
    drop(stdin);
    let output = child.wait_with_output().map_err(|e| format!("nft failed: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("nft failed: {}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

pub fn freeze(cgroups: &[String], direction: Direction) -> Result<(), String> {
    let result = load(&rules::freeze_script(cgroups, direction, &devices()?));
    match result {
        // A device (a container's veth, say) vanished after we listed it.
        Err(e) if e.contains("No such") => load(&rules::freeze_script(cgroups, direction, &devices()?)),
        other => other,
    }
}

pub fn release() -> Result<(), String> {
    load(&rules::release_script())
}
