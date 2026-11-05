//! Generate the nft scripts. Everything lives in one table named lagswitch.
//!
//! Both directions drop by socket cgroup. Dropping in the output hook makes the
//! app's UDP send() fail with EPERM, so the game sees its network fail right away,
//! as it does in Whole PC mode. Fake lag queues the app's packets to the daemon
//! instead. Loopback is left alone in every case.

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

/// The netfilter queue fake lag listens on.
pub const QUEUE: u16 = 4242;

/// What the freeze part of the table does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Freeze<'a> {
    Off,
    Apps(&'a [String], Direction),
    /// Whole PC mode's inbound half. Sends are stopped by a routing rule instead.
    WholePc,
}

fn chain(s: &mut String, name: &str, hook: &str, rules: &[String]) {
    if rules.is_empty() {
        return;
    }
    s.push_str(&format!("  chain {name} {{\n    type filter hook {hook} priority filter; policy accept;\n"));
    for rule in rules {
        s.push_str(&format!("    {rule}\n"));
    }
    s.push_str("  }\n");
}

/// One atomic transaction that replaces our table with these freezes and
/// delays. Drops come first, so a frozen app stays frozen while it's lagged.
pub fn script(freeze: Freeze, lag: &[String], lag_inbound: bool) -> String {
    let (mut out, mut inbound) = (Vec::new(), Vec::new());
    match freeze {
        Freeze::Off => {}
        Freeze::Apps(cgroups, direction) => {
            for cgroup in cgroups {
                if direction.outbound() {
                    out.push(format!("oifname != \"lo\" {} counter drop", matcher(cgroup)));
                }
                if direction.inbound() {
                    inbound.push(format!("iifname != \"lo\" {} counter drop", matcher(cgroup)));
                }
            }
        }
        Freeze::WholePc => inbound.push("iifname != \"lo\" counter drop".to_string()),
    }
    for cgroup in lag {
        out.push(format!("oifname != \"lo\" {} counter queue flags bypass to {QUEUE}", matcher(cgroup)));
        if lag_inbound {
            inbound.push(format!("iifname != \"lo\" {} counter queue flags bypass to {QUEUE}", matcher(cgroup)));
        }
    }
    let mut s = release_script();
    if out.is_empty() && inbound.is_empty() {
        return s;
    }
    s.push_str(&format!("table inet {TABLE} {{\n"));
    chain(&mut s, "out", "output", &out);
    chain(&mut s, "in", "input", &inbound);
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const CG: &str = "user.slice/app.slice/app-flatpak-org.foo-1.scope";

    fn freeze(direction: Direction) -> String {
        script(Freeze::Apps(&[CG.to_string()], direction), &[], true)
    }

    #[test]
    fn level_counts_path_segments() {
        assert!(matcher(CG).starts_with("socket cgroupv2 level 3 \"user.slice/"));
    }

    #[test]
    fn both_directions_drop_and_spare_loopback() {
        let s = freeze(Direction::Both);
        assert!(s.contains("hook output"));
        assert!(s.contains("oifname != \"lo\""));
        assert!(s.contains("hook input"));
        assert!(s.contains("iifname != \"lo\""));
        assert_eq!(s.matches("counter drop").count(), 2);
    }

    #[test]
    fn single_direction_has_a_single_chain() {
        assert!(!freeze(Direction::Out).contains("hook input"));
        assert!(!freeze(Direction::In).contains("hook output"));
    }

    #[test]
    fn replaces_old_table_in_the_same_transaction() {
        assert!(freeze(Direction::Both).starts_with(&release_script()));
        assert!(script(Freeze::WholePc, &[], true).starts_with(&release_script()));
    }

    #[test]
    fn every_cgroup_gets_a_rule() {
        let cgroups = ["a/b.scope".to_string(), "a/c.scope".to_string()];
        let s = script(Freeze::Apps(&cgroups, Direction::In), &[], true);
        assert!(s.contains("\"a/b.scope\"") && s.contains("\"a/c.scope\""));
    }

    #[test]
    fn whole_pc_drops_only_inbound_and_spares_loopback() {
        let s = script(Freeze::WholePc, &[], true);
        assert!(s.contains("hook input"));
        assert!(s.contains("iifname != \"lo\" counter drop"));
        assert!(!s.contains("hook output"));
        assert!(!s.contains("socket cgroupv2"));
    }

    #[test]
    fn nothing_active_only_removes_the_table() {
        assert_eq!(script(Freeze::Off, &[], true), release_script());
    }

    #[test]
    fn lag_queues_both_ways_and_can_be_bypassed() {
        let s = script(Freeze::Off, &[CG.to_string()], true);
        assert_eq!(s.matches(&format!("queue flags bypass to {QUEUE}")).count(), 2);
        let choke = script(Freeze::Off, &[CG.to_string()], false);
        assert_eq!(choke.matches("queue").count(), 1, "choke only queues outgoing");
        assert!(!choke.contains("hook input"));
        assert!(s.contains("oifname != \"lo\"") && s.contains("iifname != \"lo\""));
        assert!(!s.contains("drop"));
    }

    #[test]
    fn freeze_drops_before_lag_queues() {
        let lag = [CG.to_string()];
        let s = script(Freeze::Apps(&lag, Direction::Both), &lag, true);
        let out = &s[s.find("chain out").unwrap_or(0)..s.find("chain in").unwrap_or(s.len())];
        assert!(out.find("drop") < out.find("queue"), "{out}");
        let whole = script(Freeze::WholePc, &lag, true);
        let inbound = &whole[whole.find("chain in").unwrap_or(0)..];
        assert!(inbound.find("drop") < inbound.find("queue"), "{inbound}");
    }

    #[test]
    fn parses_direction() {
        assert_eq!(Direction::parse("both"), Ok(Direction::Both));
        assert!(Direction::parse("sideways").is_err());
    }
}
