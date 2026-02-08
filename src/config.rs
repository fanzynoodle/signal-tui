use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Config {
    pub scrollback_dir: PathBuf,
    pub scrollback_load_limit: usize,
    pub save_scrollback: bool,
    pub notify: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfigFile {
    scrollback_dir: Option<String>,
    scrollback_load_limit: Option<usize>,
    save_scrollback: Option<bool>,
    notify: Option<bool>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            scrollback_dir: None,
            scrollback_load_limit: Some(500),
            save_scrollback: Some(true),
            notify: Some(true),
        }
    }
}

pub fn load_or_create(config_path_override: Option<PathBuf>) -> Result<Config> {
    let config_path = if let Some(p) = config_path_override {
        p
    } else {
        config_path()?
    };
    if !config_path.exists() {
        if let Some(p) = config_path.parent() {
            fs::create_dir_all(p).with_context(|| format!("create config dir {p:?}"))?;
        }
        let sample = default_config_text(&default_scrollback_dir()?);
        fs::write(&config_path, sample).with_context(|| format!("write {config_path:?}"))?;
    }

    let raw = fs::read_to_string(&config_path).with_context(|| format!("read {config_path:?}"))?;
    let cf: ConfigFile = toml::from_str(&raw).with_context(|| format!("parse {config_path:?}"))?;

    let scrollback_dir = if let Some(s) = cf.scrollback_dir {
        expand_path(s)?
    } else {
        default_scrollback_dir()?
    };

    if scrollback_dir.as_os_str().is_empty() {
        bail!("scrollback_dir resolved to empty path");
    }
    fs::create_dir_all(&scrollback_dir)
        .with_context(|| format!("create scrollback dir {scrollback_dir:?}"))?;

    Ok(Config {
        scrollback_dir,
        scrollback_load_limit: cf.scrollback_load_limit.unwrap_or(500).clamp(50, 100_000),
        save_scrollback: cf.save_scrollback.unwrap_or(true),
        notify: cf.notify.unwrap_or(true),
    })
}

fn config_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("SIGNAL_TUI_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    let base = if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else {
        home_dir()?.join(".config")
    };
    Ok(base.join("signal-tui").join("config.toml"))
}

fn default_scrollback_dir() -> Result<PathBuf> {
    let base = if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
        PathBuf::from(xdg)
    } else {
        home_dir()?.join(".local").join("state")
    };
    Ok(base.join("signal-tui").join("scrollback"))
}

fn home_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("$HOME not set")?;
    Ok(PathBuf::from(home))
}

fn expand_path(s: String) -> Result<PathBuf> {
    let s = s.trim().to_string();
    if s.is_empty() {
        bail!("empty path in config");
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }
    Ok(PathBuf::from(s))
}

fn default_config_text(default_scrollback: &Path) -> String {
    // Keep it simple and explicit; users can edit.
    format!(
        r#"# signal-tui config
#
# Location of the config file:
#   $XDG_CONFIG_HOME/signal-tui/config.toml (default: ~/.config/signal-tui/config.toml)
#
# Location of scrollback (saved chat history, JSONL per chat):
#   $XDG_STATE_HOME/signal-tui/scrollback (default: ~/.local/state/signal-tui/scrollback)

scrollback_dir = "{p}"
scrollback_load_limit = 500
save_scrollback = true
notify = true
"#,
        p = default_scrollback.display()
    )
}
