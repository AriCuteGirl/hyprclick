//! Whole PC mode: turn NetworkManager's networking off, then back on.
//!
//! NetworkManager remembers "off" across reboots, so a marker file records that
//! we did it. It lives in the state directory and survives a crash or power cut,
//! so the daemon can always restore networking, and never touches networking
//! the user turned off themselves.

use std::fs;
use std::io::ErrorKind;
use std::process::{Command, Stdio};

use crate::nft;

const MARKER: &str = "/var/lib/lagswitch/network-off";

fn nmcli(args: &[&str]) -> Result<String, String> {
    let output = Command::new("nmcli").args(args).output().map_err(|e| match e.kind() {
        ErrorKind::NotFound => "nmcli not found, Whole PC mode needs NetworkManager".to_string(),
        _ => format!("can't start nmcli: {e}"),
    })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!("nmcli failed: {}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

/// Ethernet and Wi-Fi devices in `nmcli -t -f DEVICE,TYPE,STATE device` output
/// that are connected, so the same ones can be reconnected afterwards.
pub fn connected_devices(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split(':');
            let (device, kind, state) = (parts.next()?, parts.next()?, parts.next()?);
            let wanted = matches!(kind, "ethernet" | "wifi") && state == "connected";
            wanted.then(|| device.to_string())
        })
        .collect()
}

pub fn off() -> Result<(), String> {
    let devices = connected_devices(&nmcli(&["-t", "-f", "DEVICE,TYPE,STATE", "device"])?);
    // Marker first: there must be no moment where networking is off unrecorded.
    fs::write(MARKER, devices.join("\n")).map_err(|e| format!("can't write {MARKER}: {e}"))?;
    if let Err(e) = nmcli(&["-w", "5", "networking", "off"]) {
        let _ = fs::remove_file(MARKER);
        return Err(e);
    }
    Ok(())
}

/// Turns networking back on if we turned it off. Does nothing otherwise.
pub fn restore() -> Result<(), String> {
    let devices = match fs::read_to_string(MARKER) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("can't read {MARKER}: {e}")),
    };
    nmcli(&["-w", "5", "networking", "on"])?;
    for device in devices.lines().filter(|d| !d.is_empty()) {
        // Don't wait: connecting can take many seconds and the daemon has a timer to run.
        if let Ok(mut child) = Command::new("nmcli")
            .args(["-w", "0", "device", "connect", device])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            std::thread::spawn(move || child.wait());
        }
    }
    fs::remove_file(MARKER).map_err(|e| format!("can't remove {MARKER}: {e}"))
}

/// Ends any freeze of either kind. Both steps run even if the first one fails.
pub fn release_all() -> Result<(), String> {
    let network = restore();
    let rules = nft::release();
    network.and(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_connected_ethernet_and_wifi_count() {
        let output = "enp7s0:ethernet:connected\nwlan0:wifi:connected\nlo:loopback:connected (externally)\n\
                      br-1:bridge:connected (externally)\np2p-dev-wlan0:wifi-p2p:disconnected\n\
                      eth1:ethernet:unavailable\ndocker0:bridge:unmanaged\n";
        assert_eq!(connected_devices(output), ["enp7s0", "wlan0"]);
        assert!(connected_devices("").is_empty());
    }
}
