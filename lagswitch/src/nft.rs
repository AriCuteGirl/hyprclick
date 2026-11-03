//! Load scripts into nft.

use std::io::{ErrorKind, Write};
use std::process::{Command, Stdio};

use crate::rules::{self, Freeze};

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

/// Replace our table with these freezes and delays.
pub fn apply(freeze: Freeze, lag: &[String]) -> Result<(), String> {
    load(&rules::script(freeze, lag))
}

pub fn release() -> Result<(), String> {
    load(&rules::release_script())
}
