//! ~/.config/lagswitch/config.toml, a flat file of `key = value` lines.

use std::fs;
use std::path::PathBuf;

use crate::rules::Direction;
use crate::timer;

pub const DEFAULT_MS: u64 = 1500;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub default_ms: u64,
    pub direction: Direction,
    pub max_cap_ms: u64,
    pub last_app: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_ms: DEFAULT_MS,
            direction: Direction::Both,
            max_cap_ms: timer::DEFAULT_CAP_MS,
            last_app: None,
        }
    }
}

fn path() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or("can't find the config folder, HOME isn't set")?;
    Ok(base.join("lagswitch/config.toml"))
}

fn text_value(value: &str) -> &str {
    value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value)
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut config = Self::default();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = || format!("config line {}", index + 1);
            let (key, value) = line.split_once('=').ok_or_else(|| format!("{}: expected key = value", at()))?;
            let value = value.trim();
            let whole = || value.parse::<u64>().map_err(|_| format!("{}: \"{value}\" isn't a whole number", at()));
            match key.trim() {
                "default_ms" => config.default_ms = whole()?,
                "max_cap_ms" => config.max_cap_ms = whole()?,
                "direction" => {
                    config.direction = Direction::parse(text_value(value)).map_err(|e| format!("{}: {e}", at()))?;
                }
                "last_app" => config.last_app = Some(text_value(value).to_string()),
                other => return Err(format!("{}: unknown setting \"{other}\"", at())),
            }
        }
        Ok(config)
    }

    pub fn render(&self) -> String {
        let mut text = format!(
            "default_ms = {}\ndirection = \"{}\"\nmax_cap_ms = {}\n",
            self.default_ms,
            self.direction.name(),
            self.max_cap_ms
        );
        if let Some(app) = &self.last_app {
            text.push_str(&format!("last_app = \"{app}\"\n"));
        }
        text
    }

    /// Defaults if the file doesn't exist yet.
    pub fn load() -> Result<Self, String> {
        let path = path()?;
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("can't read {}: {e}", path.display())),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = path()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        }
        fs::write(&path, self.render()).map_err(|e| format!("can't write {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Config::parse(""), Ok(Config::default()));
        assert_eq!(Config::default().max_cap_ms, 30_000);
    }

    #[test]
    fn survives_a_round_trip() {
        let config = Config {
            default_ms: 5000,
            direction: Direction::Out,
            max_cap_ms: 10_000,
            last_app: Some("org.foo.Bar".to_string()),
        };
        assert_eq!(Config::parse(&config.render()), Ok(config));
    }

    #[test]
    fn reads_comments_and_blank_lines() {
        let config = Config::parse("# freeze time\n\n default_ms = 200 \n").expect("parse");
        assert_eq!(config.default_ms, 200);
    }

    #[test]
    fn reports_bad_lines() {
        assert!(Config::parse("default_ms = soon").unwrap_err().contains("line 1"));
        assert!(Config::parse("\ndirection = \"up\"").unwrap_err().contains("line 2"));
        assert!(Config::parse("colour = 3").unwrap_err().contains("unknown"));
        assert!(Config::parse("nonsense").unwrap_err().contains("key = value"));
    }
}
