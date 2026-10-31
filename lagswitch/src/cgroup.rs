//! Find the cgroup v2 scopes that belong to a process or Flatpak app.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

/// The cgroup v2 path in the contents of /proc/<pid>/cgroup, without the leading slash.
pub fn parse_proc_cgroup(contents: &str) -> Option<String> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(|path| path.trim().trim_start_matches('/').to_string())
}

/// The Flatpak app ID in a path ending in app-flatpak-<appid>-<n>.scope.
pub fn flatpak_app_id(cgroup: &str) -> Option<String> {
    let scope = cgroup.rsplit('/').next()?;
    let rest = scope.strip_prefix("app-flatpak-")?.strip_suffix(".scope")?;
    let (id, instance) = rest.rsplit_once('-')?;
    if id.is_empty() || instance.is_empty() || !instance.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(id.replace("\\x2d", "-"))
}

/// Refuse any cgroup shared with unrelated processes, or one that can't go in a rule safely.
pub fn check_target(cgroup: &str) -> Result<(), String> {
    let last = cgroup.rsplit('/').next().unwrap_or("");
    if cgroup.is_empty()
        || last.ends_with(".slice")
        || last.ends_with(".service") && last.starts_with("user@")
        || last == "init.scope"
        || (last.starts_with("session-") && last.ends_with(".scope"))
    {
        return Err(format!(
            "cgroup \"/{cgroup}\" is shared with other programs, refusing to freeze it"
        ));
    }
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._-@:\\".contains(c);
    if !cgroup.chars().all(safe) {
        return Err(format!("cgroup path has unexpected characters: {cgroup:?}"));
    }
    Ok(())
}

/// The daemon only freezes the user who talks to it, never system services or other users.
pub fn check_owner(cgroup: &str, uid: u32) -> Result<(), String> {
    if cgroup.starts_with(&format!("user.slice/user-{uid}.slice/")) {
        Ok(())
    } else {
        Err(format!("cgroup \"/{cgroup}\" doesn't belong to user {uid}"))
    }
}

/// Each running process that reports a cgroup v2 path, as (pid, path).
fn running() -> Result<Vec<(u32, String)>, String> {
    let dir = fs::read_dir("/proc").map_err(|e| format!("can't read /proc: {e}"))?;
    let mut found = Vec::new();
    for entry in dir.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        // Processes can exit while we scan.
        if let Ok(contents) = fs::read_to_string(entry.path().join("cgroup"))
            && let Some(path) = parse_proc_cgroup(&contents)
        {
            found.push((pid, path));
        }
    }
    Ok(found)
}

/// Running Flatpak apps as (app ID, scope path), one row per instance.
pub fn flatpak_scopes() -> Result<BTreeSet<(String, String)>, String> {
    Ok(running()?
        .into_iter()
        .filter_map(|(_, path)| {
            let top = flatpak_scope_of(&path)?;
            Some((flatpak_app_id(&top)?, top))
        })
        .collect())
}

/// Trim a path to its app-flatpak scope, in case a process sits in a child cgroup.
fn flatpak_scope_of(path: &str) -> Option<String> {
    let mut end = 0;
    for part in path.split('/') {
        end += part.len();
        if part.starts_with("app-flatpak-") && part.ends_with(".scope") {
            return Some(path[..end].to_string());
        }
        end += 1;
    }
    None
}

/// Pick the scopes for an app name: an exact app ID, or a part of exactly one.
pub fn match_app(name: &str, apps: &BTreeSet<(String, String)>) -> Result<Vec<String>, String> {
    let exact: Vec<String> =
        apps.iter().filter(|(id, _)| id == name).map(|(_, p)| p.clone()).collect();
    if !exact.is_empty() {
        return Ok(exact);
    }
    let lower = name.to_lowercase();
    let ids: BTreeSet<&String> =
        apps.iter().map(|(id, _)| id).filter(|id| id.to_lowercase().contains(&lower)).collect();
    match ids.len() {
        0 => Err(format!("no running Flatpak app matches \"{name}\"")),
        1 => Ok(apps
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .map(|(_, p)| p.clone())
            .collect()),
        _ => {
            let list: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
            Err(format!("\"{name}\" matches several apps: {}", list.join(", ")))
        }
    }
}

/// One row of the picker: everything in one cgroup, which is what gets frozen together.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub pid: u32,
    pub app_id: Option<String>,
    pub cgroup: String,
    /// How many processes would freeze together.
    pub processes: usize,
    pub freezable: bool,
}

fn comm(pid: u32) -> Option<String> {
    let text = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(text.trim_end().to_string())
}

/// Group (pid, command name, cgroup) rows into picker entries for one user.
pub fn group(rows: &[(u32, String, String)], uid: u32) -> Vec<Entry> {
    let mut groups: BTreeMap<&str, Vec<&(u32, String, String)>> = BTreeMap::new();
    for row in rows {
        if check_owner(&row.2, uid).is_ok() {
            groups.entry(&row.2).or_default().push(row);
        }
    }
    let mut entries: Vec<Entry> = groups
        .into_iter()
        .filter_map(|(path, members)| {
            // The lowest PID started first, so it best names the group.
            let first = members.iter().min_by_key(|row| row.0)?;
            let scope = flatpak_scope_of(path);
            let app_id = scope.as_deref().and_then(flatpak_app_id);
            Some(Entry {
                name: app_id.clone().unwrap_or_else(|| first.1.clone()),
                pid: first.0,
                app_id,
                cgroup: path.to_string(),
                processes: members.len(),
                freezable: check_target(scope.as_deref().unwrap_or(path)).is_ok(),
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        (a.app_id.is_none(), a.name.to_lowercase()).cmp(&(b.app_id.is_none(), b.name.to_lowercase()))
    });
    entries
}

/// Everything this user could pick right now, Flatpak apps first.
pub fn entries(uid: u32) -> Result<Vec<Entry>, String> {
    let rows: Vec<(u32, String, String)> = running()?
        .into_iter()
        .filter_map(|(pid, path)| Some((pid, comm(pid)?, path)))
        .collect();
    Ok(group(&rows, uid))
}

/// Keep entries whose name, app ID or PID contains the filter.
pub fn filter(entries: Vec<Entry>, text: &str) -> Vec<Entry> {
    let text = text.to_lowercase();
    entries
        .into_iter()
        .filter(|e| {
            e.name.to_lowercase().contains(&text)
                || e.app_id.as_deref().is_some_and(|id| id.to_lowercase().contains(&text))
                || e.pid.to_string().contains(&text)
        })
        .collect()
}

/// The cgroups for an app name: a running Flatpak first, then an exact command name.
pub fn resolve_app(name: &str, uid: u32) -> Result<Vec<String>, String> {
    let flatpak = match_app(name, &flatpak_scopes()?);
    if !flatpak.as_ref().is_err_and(|e| e.starts_with("no running")) {
        return flatpak;
    }
    let found: BTreeSet<String> = entries(uid)?
        .into_iter()
        .filter(|e| e.app_id.is_none() && e.name.eq_ignore_ascii_case(name))
        .map(|e| e.cgroup)
        .collect();
    if found.is_empty() {
        return Err(format!("no running Flatpak app or process matches \"{name}\""));
    }
    Ok(found.into_iter().collect())
}

/// The cgroup of one process.
pub fn of_pid(pid: u32) -> Result<String, String> {
    let contents = fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .map_err(|_| format!("process {pid} not found"))?;
    parse_proc_cgroup(&contents).ok_or_else(|| format!("cgroup not found for process {pid}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCOPE: &str =
        "user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-org.foo.Bar-123.scope";

    #[test]
    fn parses_v2_line_among_v1_lines() {
        let text = format!("1:net_cls:/\n0::/{SCOPE}\n");
        assert_eq!(parse_proc_cgroup(&text).as_deref(), Some(SCOPE));
        assert_eq!(parse_proc_cgroup("1:net_cls:/\n"), None);
        assert_eq!(parse_proc_cgroup("0::/\n").as_deref(), Some(""));
    }

    #[test]
    fn reads_flatpak_ids() {
        assert_eq!(flatpak_app_id(SCOPE).as_deref(), Some("org.foo.Bar"));
        assert_eq!(flatpak_app_id("a/app-flatpak-org.a-b.C-9.scope").as_deref(), Some("org.a-b.C"));
        assert_eq!(flatpak_app_id("a/app-flatpak-org.a\\x2db-9.scope").as_deref(), Some("org.a-b"));
        assert_eq!(flatpak_app_id("a/app-com.foo-9.scope"), None);
        assert_eq!(flatpak_app_id("a/app-flatpak-org.foo-x.scope"), None);
    }

    #[test]
    fn trims_child_cgroups_to_the_scope() {
        assert_eq!(flatpak_scope_of(&format!("{SCOPE}/child")).as_deref(), Some(SCOPE));
        assert_eq!(flatpak_scope_of(SCOPE).as_deref(), Some(SCOPE));
        assert_eq!(flatpak_scope_of("user.slice/other.scope"), None);
    }

    #[test]
    fn refuses_shared_cgroups() {
        assert!(check_target(SCOPE).is_ok());
        for shared in [
            "",
            "user.slice",
            "user.slice/user-1000.slice",
            "user.slice/user-1000.slice/session-2.scope",
            "user.slice/user-1000.slice/user@1000.service",
            "user.slice/user-1000.slice/user@1000.service/init.scope",
            "user.slice/user-1000.slice/user@1000.service/app.slice",
        ] {
            assert!(check_target(shared).is_err(), "{shared}");
        }
        assert!(check_target("a/b\" drop; #.scope").is_err());
        assert!(check_target("a/b\nc.scope").is_err());
    }

    #[test]
    fn only_the_owner_can_be_frozen() {
        assert!(check_owner(SCOPE, 1000).is_ok());
        assert!(check_owner(SCOPE, 1001).is_err());
        assert!(check_owner("system.slice/sshd.service", 1000).is_err());
        assert!(check_owner("user.slice/user-10000.slice/x.scope", 1000).is_err());
    }

    fn row(pid: u32, name: &str, path: &str) -> (u32, String, String) {
        (pid, name.to_string(), path.to_string())
    }

    #[test]
    fn groups_processes_by_cgroup() {
        let rows = [
            row(30, "child", SCOPE),
            row(20, "bwrap", SCOPE),
            row(5, "zsh", "user.slice/user-1000.slice/session-1.scope"),
            row(7, "steam", "user.slice/user-1000.slice/user@1000.service/app.slice/steam.scope"),
            row(1, "systemd", "init.scope"),
            row(9, "other", "user.slice/user-1001.slice/x.scope"),
        ];
        let entries = group(&rows, 1000);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["org.foo.Bar", "steam", "zsh"]);
        assert_eq!(entries[0].processes, 2);
        assert_eq!(entries[0].pid, 20);
        assert_eq!(entries[0].app_id.as_deref(), Some("org.foo.Bar"));
        assert!(entries[0].freezable && entries[1].freezable);
        assert!(!entries[2].freezable, "a session scope holds the whole login");
    }

    #[test]
    fn filters_entries() {
        let entries = group(&[row(20, "bwrap", SCOPE), row(77, "steam", "user.slice/user-1000.slice/a.scope")], 1000);
        assert_eq!(filter(entries.clone(), "FOO").len(), 1);
        assert_eq!(filter(entries.clone(), "steam").len(), 1);
        assert_eq!(filter(entries.clone(), "77").len(), 1);
        assert_eq!(filter(entries, "nothing").len(), 0);
    }

    #[test]
    fn matches_app_names() {
        let apps: BTreeSet<(String, String)> = [
            ("org.foo.Bar", "s/app-flatpak-org.foo.Bar-1.scope"),
            ("org.foo.Bar", "s/app-flatpak-org.foo.Bar-2.scope"),
            ("org.foo.Baz", "s/app-flatpak-org.foo.Baz-3.scope"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        assert_eq!(match_app("org.foo.Bar", &apps).map(|v| v.len()), Ok(2));
        assert_eq!(match_app("BAZ", &apps).map(|v| v.len()), Ok(1));
        assert!(match_app("foo", &apps).unwrap_err().contains("several"));
        assert!(match_app("nope", &apps).unwrap_err().contains("no running"));
    }
}
