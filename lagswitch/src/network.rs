//! Whole PC mode: everything off the machine fails at once, as if the network
//! were gone, while interfaces, addresses and connections stay up. Ending it is
//! instant, because there's nothing to reconnect.
//!
//! An `unreachable` routing rule makes every send fail with ENETUNREACH. It sits
//! just after the local table, so loopback and the PC's own addresses still work.
//! Routing rules don't touch incoming packets, so a nft rule drops those.

use std::io::ErrorKind;
use std::process::{Command, Stdio};

use crate::nft;

const PRIORITY: &str = "1";
const FAMILIES: [&str; 2] = ["-4", "-6"];

fn ip(family: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new("ip")
        .arg(family)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            ErrorKind::NotFound => "ip not found, install the iproute2 package".to_string(),
            _ => format!("can't start ip: {e}"),
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!("ip rule failed: {}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

/// Is one of our rules in this `ip rule show priority` output?
pub fn has_our_rule(output: &str) -> bool {
    output.lines().any(|line| line.trim_end().ends_with("from all unreachable"))
}

fn rule_present(family: &str) -> bool {
    ip(family, &["rule", "show", "priority", PRIORITY]).is_ok_and(|text| has_our_rule(&text))
}

pub fn off() -> Result<(), String> {
    nft::whole_pc()?;
    if !rule_present("-4")
        && let Err(e) = ip("-4", &["rule", "add", "priority", PRIORITY, "unreachable"])
    {
        let _ = nft::release();
        return Err(e);
    }
    // Machines without IPv6 have nothing to block here.
    if !rule_present("-6") {
        let _ = ip("-6", &["rule", "add", "priority", PRIORITY, "unreachable"]);
    }
    Ok(())
}

/// Removes our routing rules, and is safe to call when there are none.
pub fn restore() -> Result<(), String> {
    for family in FAMILIES {
        for _ in 0..8 {
            if !rule_present(family) {
                break;
            }
            ip(family, &["rule", "del", "priority", PRIORITY, "unreachable"])?;
        }
        if rule_present(family) {
            return Err(format!("couldn't remove the ip {family} rule"));
        }
    }
    Ok(())
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
    fn finds_only_our_rule() {
        assert!(has_our_rule("1:\tfrom all unreachable\n"));
        assert!(!has_our_rule(""));
        assert!(!has_our_rule("5250:\tfrom all fwmark 0x80000/0xff0000 unreachable\n"));
        assert!(!has_our_rule("1:\tfrom all lookup main\n"));
    }
}
