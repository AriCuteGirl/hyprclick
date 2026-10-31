//! Generate the nft scripts. Everything lives in two tables named lagswitch.
//!
//! Inbound drops in the input hook. Outbound can't drop in the output hook,
//! because then UDP send() fails with EPERM and the app can tell something is
//! wrong. So output only marks the packets and a netdev egress chain drops them,
//! where send() still reports success. Loopback is left alone in both directions.

pub const TABLE: &str = "lagswitch";
pub const MARK: u32 = 0x1a65;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    In,
    Out,
    Both,
}

impl Direction {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "in" => Ok(Self::In),
            "out" => Ok(Self::Out),
            "both" => Ok(Self::Both),
            _ => Err(format!("direction must be in, out or both, not \"{text}\"")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
            Self::Both => "both",
        }
    }

    fn inbound(self) -> bool {
        self != Self::Out
    }

    fn outbound(self) -> bool {
        self != Self::In
    }
}

/// Removes our tables. `destroy` doesn't fail when they are already gone.
pub fn release_script() -> String {
    format!("destroy table inet {TABLE}\ndestroy table netdev {TABLE}\n")
}

fn matcher(cgroup: &str) -> String {
    let level = cgroup.split('/').count();
    format!("socket cgroupv2 level {level} \"{cgroup}\"")
}

/// One atomic transaction that replaces our tables with a freeze of these cgroups.
pub fn freeze_script(cgroups: &[String], direction: Direction, devices: &[String]) -> String {
    let mut s = release_script();
    s.push_str(&format!("table inet {TABLE} {{\n"));
    if direction.outbound() {
        // Last priority, so nothing rewrites the mark after us.
        s.push_str("  chain out {\n    type filter hook output priority 300; policy accept;\n");
        for cgroup in cgroups {
            s.push_str(&format!("    {} meta mark set {MARK:#x}\n", matcher(cgroup)));
        }
        s.push_str("  }\n");
    }
    if direction.inbound() {
        s.push_str("  chain in {\n    type filter hook input priority filter; policy accept;\n");
        for cgroup in cgroups {
            s.push_str(&format!("    iifname != \"lo\" {} counter drop\n", matcher(cgroup)));
        }
        s.push_str("  }\n");
    }
    s.push_str("}\n");
    if direction.outbound() {
        let names: Vec<String> = devices.iter().map(|d| format!("\"{d}\"")).collect();
        s.push_str(&format!(
            "table netdev {TABLE} {{\n  chain out {{\n    type filter hook egress devices = {{ {} }} priority filter; policy accept;\n    meta mark {MARK:#x} counter drop\n  }}\n}}\n",
            names.join(", ")
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const CG: &str = "user.slice/app.slice/app-flatpak-org.foo-1.scope";

    fn script(direction: Direction) -> String {
        freeze_script(&[CG.to_string()], direction, &["enp7s0".to_string(), "wlan0".to_string()])
    }

    #[test]
    fn level_counts_path_segments() {
        assert!(matcher(CG).starts_with("socket cgroupv2 level 3 \"user.slice/"));
    }

    #[test]
    fn both_directions_have_all_three_parts() {
        let s = script(Direction::Both);
        assert!(s.contains("hook output priority 300"));
        assert!(s.contains("meta mark set 0x1a65"));
        assert!(s.contains("hook input"));
        assert!(s.contains("iifname != \"lo\""));
        assert!(s.contains("hook egress devices = { \"enp7s0\", \"wlan0\" }"));
        assert!(s.contains("meta mark 0x1a65 counter drop"));
    }

    #[test]
    fn outbound_never_drops_in_the_output_hook() {
        let s = script(Direction::Out);
        let output = s.split("hook output").nth(1).unwrap_or("").split('}').next().unwrap_or("");
        assert!(!output.contains("drop"));
        assert!(!s.contains("hook input"));
    }

    #[test]
    fn inbound_only_skips_marking() {
        let s = script(Direction::In);
        assert!(s.contains("hook input"));
        assert!(!s.contains("mark"));
        assert!(!s.contains("netdev lagswitch {"));
    }

    #[test]
    fn replaces_old_tables_in_the_same_transaction() {
        assert!(script(Direction::Both).starts_with(&release_script()));
    }

    #[test]
    fn every_cgroup_gets_a_rule() {
        let cgroups = ["a/b.scope".to_string(), "a/c.scope".to_string()];
        let s = freeze_script(&cgroups, Direction::In, &[]);
        assert!(s.contains("\"a/b.scope\"") && s.contains("\"a/c.scope\""));
    }

    #[test]
    fn parses_direction() {
        assert_eq!(Direction::parse("both"), Ok(Direction::Both));
        assert!(Direction::parse("sideways").is_err());
    }
}
