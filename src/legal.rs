//! Whether you have accepted the current terms. Nothing plays and nothing
//! talks to the network until you have: the station, the tunnel and the
//! stream server all wait for it.
//!
//! The version is the `(terms version YYYY-MM-DD)` line in TERMS.md, read
//! when the console is built, so a new TERMS.md asks again on its own.

use std::path::{Path, PathBuf};

pub const TERMS_VERSION: &str = env!("DEFALT_TERMS_VERSION");

pub struct Acceptance {
    path: PathBuf,
    accepted: bool,
}

impl Acceptance {
    /// Accepted only if the saved record is for this exact terms version.
    pub fn load(root: &Path) -> Self {
        let path = root.join("cache").join("legal-acceptance.json");
        let accepted = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|saved| saved["terms_version"].as_str() == Some(TERMS_VERSION));
        Acceptance { path, accepted }
    }

    pub fn accepted(&self) -> bool {
        self.accepted
    }

    /// Accepted from here on, and remembered. A record that can't be saved
    /// still counts for this session; you'd just be asked again next launch.
    pub fn accept(&mut self) -> Result<(), String> {
        self.accepted = true;
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let record = serde_json::json!({
            "terms_version": TERMS_VERSION,
            "accepted_at": at,
            "app_version": env!("CARGO_PKG_VERSION"),
        });
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&self.path, format!("{record:#}\n")).map_err(|e| e.to_string())
    }

    /// Screenshot runs pose the panel behind the notice, not the notice,
    /// unless they ask for it. Neither case writes anything.
    pub fn pose(&mut self, show_notice: bool) {
        self.accepted = !show_notice;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("defalt-legal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn the_version_is_the_one_terms_md_names() {
        let terms = include_str!("../TERMS.md");
        assert!(terms.contains(&format!("(terms version {TERMS_VERSION})")), "{TERMS_VERSION}");
    }

    #[test]
    fn acceptance_is_remembered_for_this_version_only() {
        let root = root("remembered");
        let mut first = Acceptance::load(&root);
        assert!(!first.accepted(), "nothing saved yet");
        first.accept().unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("cache/legal-acceptance.json")).unwrap()).unwrap();
        assert_eq!(saved["terms_version"], TERMS_VERSION);
        assert!(saved["accepted_at"].as_u64().unwrap() > 1_700_000_000);
        assert!(Acceptance::load(&root).accepted(), "asked again after accepting");

        std::fs::write(root.join("cache/legal-acceptance.json"),
                       r#"{"terms_version":"2026-01-01","accepted_at":1}"#).unwrap();
        assert!(!Acceptance::load(&root).accepted(), "older terms must be accepted again");
        std::fs::write(root.join("cache/legal-acceptance.json"), "not json").unwrap();
        assert!(!Acceptance::load(&root).accepted());
        let _ = std::fs::remove_dir_all(&root);
    }
}
