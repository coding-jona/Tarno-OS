//! In-Memory API-Key-Vault.
//!
//! Liest eine root-only Datei (`KEY=VALUE` pro Zeile, `#`-Kommentare erlaubt)
//! einmalig beim Start und hält die Werte danach ausschließlich im
//! Prozessspeicher. Kein erneuter Festplattenzugriff nach dem Start.
//! Begründung: docs/month3-tarno-layer.md#api-key-vault.

use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Vault {
    keys: HashMap<String, String>,
}

impl Vault {
    pub fn load_from_file(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::parse(&content))
    }

    pub fn parse(content: &str) -> Self {
        let mut keys = HashMap::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                keys.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        Vault { keys }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.keys.get(name).map(|s| s.as_str())
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_keys() {
        let vault = Vault::parse("MOJANG_API_KEY=abc123\nOTHER=xyz\n");
        assert_eq!(vault.get("MOJANG_API_KEY"), Some("abc123"));
        assert_eq!(vault.get("OTHER"), Some("xyz"));
        assert_eq!(vault.len(), 2);
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let vault = Vault::parse("# comment\n\nKEY=value\n   \n# another\n");
        assert_eq!(vault.len(), 1);
        assert_eq!(vault.get("KEY"), Some("value"));
    }

    #[test]
    fn trims_whitespace_around_key_and_value() {
        let vault = Vault::parse("  KEY  =  value with spaces  \n");
        assert_eq!(vault.get("KEY"), Some("value with spaces"));
    }

    #[test]
    fn unknown_key_returns_none() {
        let vault = Vault::parse("KEY=value\n");
        assert_eq!(vault.get("MISSING"), None);
    }

    #[test]
    fn empty_content_is_empty_vault() {
        let vault = Vault::parse("");
        assert!(vault.is_empty());
    }

    #[test]
    fn load_from_missing_file_returns_err() {
        let result = Vault::load_from_file(Path::new("/nonexistent/path/tarnod-test-vault"));
        assert!(result.is_err());
    }
}
