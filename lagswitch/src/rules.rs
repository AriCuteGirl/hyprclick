//! Generate the nft scripts. Everything lives in one table named lagswitch.
//!
//! Both directions drop by socket cgroup. Dropping in the output hook makes the
//! app's UDP send() fail with EPERM, so the game sees its network fail right away,
//! as it does in Whole PC mode. Loopback is left alone in both directions.

pub const TABLE: &str = "lagswitch";

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

/// Removes our table. `destroy` doesn't fail when it's already gone.
pub fn release_script() -> String {
    format!("destroy table inet {TABLE}\n")
}

fn matcher(cgroup: &str) -> String {
    let level = cgroup.split('/').count();
    format!("socket cgroupv2 level {level} \"{cgroup}\"")
}

/// One atomic transaction that replaces our table with a freeze of these cgroups.
pub fn freeze_script(cgroups: &[String], direction: Direction) -> String {
    let mut s = release_script();
    s.push_str(&format!("table inet {TABLE} {{\n"));
    if direction.outbound() {
        s.push_str("  chain out {\n    type filter hook output priority filter; policy accept;\n");
        for cgroup in cgroups {
            s.push_str(&format!("    oifname != \"lo\" {} counter drop\n", matcher(cgroup)));
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
    s
}

/// Whole PC mode's inbound half. Sends are stopped by a routing rule instead.
pub fn whole_pc_script() -> String {
    let mut s = release_script();
    s.push_str(&format!(
        "table inet {TABLE} {{\n  chain in {{\n    type filter hook input priority filter; policy accept;\n    iifname != \"lo\" counter drop\n  }}\n}}\n"
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const CG: &str = "user.slice/app.slice/app-flatpak-org.foo-1.scope";

    fn script(direction: Direction) -> String {
        freeze_script(&[CG.to_string()], direction)
    }

    #[test]
    fn level_counts_path_segments() {
        assert!(matcher(CG).starts_with("socket cgroupv2 level 3 \"user.slice/"));
    }

    #[test]
    fn both_directions_drop_and_spare_loopback() {
        let s = script(Direction::Both);
        assert!(s.contains("hook output"));
        assert!(s.contains("oifname != \"lo\""));
        assert!(s.contains("hook input"));
        assert!(s.contains("iifname != \"lo\""));
        assert_eq!(s.matches("counter drop").count(), 2);
    }

    #[test]
    fn single_direction_has_a_single_chain() {
        assert!(!script(Direction::Out).contains("hook input"));
        assert!(!script(Direction::In).contains("hook output"));
    }

    #[test]
    fn replaces_old_table_in_the_same_transaction() {
        assert!(script(Direction::Both).starts_with(&release_script()));
        assert!(whole_pc_script().starts_with(&release_script()));
    }

    #[test]
    fn every_cgroup_gets_a_rule() {
        let cgroups = ["a/b.scope".to_string(), "a/c.scope".to_string()];
        let s = freeze_script(&cgroups, Direction::In);
        assert!(s.contains("\"a/b.scope\"") && s.contains("\"a/c.scope\""));
    }

    #[test]
    fn whole_pc_drops_only_inbound_and_spares_loopback() {
        let s = whole_pc_script();
        assert!(s.contains("hook input"));
        assert!(s.contains("iifname != \"lo\" counter drop"));
        assert!(!s.contains("hook output"));
        assert!(!s.contains("socket cgroupv2"));
    }

    #[test]
    fn parses_direction() {
        assert_eq!(Direction::parse("both"), Ok(Direction::Both));
        assert!(Direction::parse("sideways").is_err());
    }
}
