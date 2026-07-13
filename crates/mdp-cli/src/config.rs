//! Optional `config.toml` for persistent defaults (design section 6).
//!
//! Resolution precedence is CLI flag > config file > built-in default. The
//! config lives in the platform config dir: `~/.config/mdp/config.toml` on Unix
//! (Linux and macOS) and `%APPDATA%\mdp\config.toml` on Windows. `MDP_CONFIG_DIR`
//! overrides the location (used by tests, and handy for isolated setups).
//!
//! A missing config is simply "no defaults"; a malformed one is a warning, not a
//! failure, so a broken config never blocks a preview.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::Scheme;

/// Persistent defaults parsed from `config.toml`. Unknown keys are rejected so a
/// typo surfaces (as a warning) rather than being silently ignored.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Standing default for `--theme`: a path to a theme CSS file.
    pub theme: Option<PathBuf>,
    /// Standing default color scheme.
    pub scheme: Option<Scheme>,
}

impl Config {
    /// Load config from the platform config dir, or defaults if it is absent,
    /// unreadable, or malformed.
    pub fn load() -> Config {
        match config_dir() {
            Some(dir) => Config::load_from_dir(&dir),
            None => Config::default(),
        }
    }

    /// Load `config.toml` from `dir`. A missing/unreadable file yields defaults;
    /// a malformed file yields defaults plus a warning on stderr.
    fn load_from_dir(dir: &Path) -> Config {
        let path = dir.join("config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => return Config::default(),
        };
        match toml::from_str(&text) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("mdp: ignoring malformed {}: {e}", path.display());
                Config::default()
            }
        }
    }
}

/// Resolve the config directory from the real process environment.
fn config_dir() -> Option<PathBuf> {
    config_dir_from(|key| std::env::var_os(key))
}

/// Pure config-dir resolver with an injected environment accessor, so the
/// platform logic is testable without mutating the process environment.
fn config_dir_from(get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(dir) = get("MDP_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if cfg!(windows) {
        get("APPDATA")
            .filter(|v| !v.is_empty())
            .map(|appdata| PathBuf::from(appdata).join("mdp"))
    } else {
        if let Some(xdg) = get("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(xdg).join("mdp"));
        }
        get("HOME")
            .filter(|v| !v.is_empty())
            .map(|home| PathBuf::from(home).join(".config").join("mdp"))
    }
}

/// Effective theme: CLI flag wins, then config, then none.
pub fn resolve_theme(flag: Option<PathBuf>, config: Option<PathBuf>) -> Option<PathBuf> {
    flag.or(config)
}

/// Effective color scheme: CLI flag wins, then config, then `Auto`.
pub fn resolve_scheme(flag: Option<Scheme>, config: Option<Scheme>) -> Scheme {
    flag.or(config).unwrap_or(Scheme::Auto)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(*v))
        }
    }

    #[test]
    fn config_dir_prefers_explicit_override() {
        let get = env_from(&[
            ("MDP_CONFIG_DIR", "/custom/dir"),
            ("XDG_CONFIG_HOME", "/xdg"),
            ("HOME", "/home/u"),
            ("APPDATA", "C:\\ad"),
        ]);
        assert_eq!(config_dir_from(get), Some(PathBuf::from("/custom/dir")));
    }

    #[cfg(not(windows))]
    #[test]
    fn config_dir_uses_xdg_then_home_on_unix() {
        let xdg = env_from(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")]);
        assert_eq!(config_dir_from(xdg), Some(PathBuf::from("/xdg/mdp")));

        let home_only = env_from(&[("HOME", "/home/u")]);
        assert_eq!(
            config_dir_from(home_only),
            Some(PathBuf::from("/home/u/.config/mdp"))
        );

        let empty = env_from(&[]);
        assert_eq!(config_dir_from(empty), None);
    }

    #[test]
    fn empty_override_is_ignored() {
        // An empty MDP_CONFIG_DIR must not short-circuit to an empty path.
        let get = env_from(&[("MDP_CONFIG_DIR", ""), ("HOME", "/home/u")]);
        let resolved = config_dir_from(get);
        assert_ne!(resolved, Some(PathBuf::new()));
    }

    #[test]
    fn parses_theme_and_scheme() {
        let cfg: Config =
            toml::from_str("theme = \"/themes/night.css\"\nscheme = \"dark\"\n").unwrap();
        assert_eq!(cfg.theme, Some(PathBuf::from("/themes/night.css")));
        assert!(matches!(cfg.scheme, Some(Scheme::Dark)));
    }

    #[test]
    fn empty_config_is_all_none() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn unknown_key_is_rejected() {
        // deny_unknown_fields turns a typo into an error the loader can warn on.
        assert!(toml::from_str::<Config>("thmee = \"x.css\"\n").is_err());
        // ...while the correctly-spelled key still parses.
        assert!(toml::from_str::<Config>("theme = \"x.css\"\n").is_ok());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        // No config.toml written.
        assert_eq!(Config::load_from_dir(dir.path()), Config::default());
    }

    #[test]
    fn present_file_is_loaded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "theme = \"/t.css\"\nscheme = \"light\"\n",
        )
        .unwrap();
        let cfg = Config::load_from_dir(dir.path());
        assert_eq!(cfg.theme, Some(PathBuf::from("/t.css")));
        assert!(matches!(cfg.scheme, Some(Scheme::Light)));
    }

    #[test]
    fn malformed_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "this is not = valid = toml").unwrap();
        assert_eq!(Config::load_from_dir(dir.path()), Config::default());
    }

    #[test]
    fn theme_precedence_flag_beats_config() {
        let flag = Some(PathBuf::from("/flag.css"));
        let config = Some(PathBuf::from("/config.css"));
        assert_eq!(resolve_theme(flag, config), Some(PathBuf::from("/flag.css")));
        assert_eq!(
            resolve_theme(None, Some(PathBuf::from("/config.css"))),
            Some(PathBuf::from("/config.css"))
        );
        assert_eq!(resolve_theme(None, None), None);
    }

    #[test]
    fn scheme_precedence_flag_beats_config_beats_default() {
        assert!(matches!(
            resolve_scheme(Some(Scheme::Light), Some(Scheme::Dark)),
            Scheme::Light
        ));
        assert!(matches!(
            resolve_scheme(None, Some(Scheme::Dark)),
            Scheme::Dark
        ));
        assert!(matches!(resolve_scheme(None, None), Scheme::Auto));
    }
}
