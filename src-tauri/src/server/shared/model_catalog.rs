//! The Claude model catalog, read from a vendored copy of
//! `@ikenga/contract`'s generated `schemas/models.json`.
//!
//! The contract package owns the table (ids, tier aliases, context windows,
//! verified prices, role defaults). The shell can't import TypeScript from
//! Rust, and the crate must build without a sibling `contract/` checkout, so
//! `model_catalog.json` beside this file is a byte-for-byte copy embedded at
//! compile time. The frontend imports the same file. `vendored_copy_matches_contract`
//! fails when the copy drifts from a contract checkout that has the file;
//! refresh it with `cp ../contract/schemas/models.json src-tauri/src/server/shared/model_catalog.json`.
//!
//! Headless (no Tauri types) so the daemon build and its tests compile it too.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

/// The vendored catalog document, verbatim.
pub const MODEL_CATALOG_JSON: &str = include_str!("model_catalog.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPricing {
    pub in_per_mtok: Option<f64>,
    pub out_per_mtok: Option<f64>,
    pub cache_read_per_mtok: Option<f64>,
    /// 5-minute cache write.
    pub cache_write_per_mtok: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub id: String,
    pub alias: String,
    pub family: String,
    pub tier: u32,
    pub context_tokens: u64,
    pub pricing: ModelPricing,
    pub pricing_verified_at: String,
    pub source: String,
    #[serde(default)]
    pub default_for: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalog {
    pub version: u32,
    pub default_model: String,
    /// Launch role (`chi` | `pane` | `plan`) → model id.
    pub roles: BTreeMap<String, String>,
    /// Tier alias (`opus` | `sonnet` | `haiku` | `fable`) → model id.
    pub aliases: BTreeMap<String, String>,
    pub models: Vec<ModelEntry>,
}

static CATALOG: LazyLock<ModelCatalog> = LazyLock::new(|| {
    // The file is embedded and covered by `vendored_catalog_parses`, so a
    // parse failure here is a build-time mistake, not a runtime condition.
    serde_json::from_str(MODEL_CATALOG_JSON).expect("vendored model_catalog.json parses")
});

/// The parsed catalog.
pub fn catalog() -> &'static ModelCatalog {
    &CATALOG
}

/// Default model id for a launch role (`chi`, `pane`, `plan`). `None` for an
/// unknown role, so the caller falls back to Claude Code's own default.
pub fn default_model_for_role(role: &str) -> Option<&'static str> {
    catalog().roles.get(role).map(String::as_str)
}

/// Catalog row by exact id or tier alias.
pub fn find_model(id_or_alias: &str) -> Option<&'static ModelEntry> {
    let c = catalog();
    c.models
        .iter()
        .find(|m| m.id == id_or_alias)
        .or_else(|| c.models.iter().find(|m| m.alias == id_or_alias))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn vendored_catalog_parses() {
        let c = catalog();
        assert_eq!(c.version, 1);
        assert!(!c.models.is_empty());
        for m in &c.models {
            assert!(
                m.pricing.in_per_mtok.is_some(),
                "{} input price unverified",
                m.id
            );
            assert!(
                m.pricing.out_per_mtok.is_some(),
                "{} output price unverified",
                m.id
            );
            assert!(!m.pricing_verified_at.is_empty());
        }
    }

    #[test]
    fn role_defaults_match_the_locked_decision() {
        assert_eq!(default_model_for_role("chi"), Some("claude-sonnet-5-5"));
        assert_eq!(default_model_for_role("pane"), Some("claude-sonnet-5-5"));
        assert_eq!(default_model_for_role("plan"), Some("claude-opus-5-5"));
        assert_eq!(default_model_for_role("review"), None);
        assert_eq!(catalog().default_model, "claude-sonnet-5-5");
    }

    #[test]
    fn roles_and_aliases_point_at_rows() {
        let c = catalog();
        for id in c.roles.values().chain(c.aliases.values()) {
            assert!(find_model(id).is_some(), "{id} is not a catalog row");
        }
        assert_eq!(
            find_model("fable").map(|m| m.id.as_str()),
            Some("claude-fable-5-1")
        );
        assert!(find_model("claude-fable-5-1")
            .unwrap()
            .default_for
            .is_empty());
    }

    /// Same sibling-checkout convention as `pkg::manifest_v5_parity`:
    /// `$IKENGA_CONTRACT_DIR`, else `../../contract`. Skips when the checkout
    /// or its `schemas/models.json` is absent (a contract release that
    /// predates the catalog), fails when both exist and differ.
    #[test]
    fn vendored_copy_matches_contract() {
        let root = std::env::var_os("IKENGA_CONTRACT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract"))
            });
        let path = root.join("schemas/models.json");
        let Ok(upstream) = std::fs::read_to_string(&path) else {
            eprintln!(
                "model_catalog: skipping parity — {} not found",
                path.display()
            );
            return;
        };
        assert_eq!(
            upstream.replace("\r\n", "\n"),
            MODEL_CATALOG_JSON.replace("\r\n", "\n"),
            "src-tauri/src/server/shared/model_catalog.json is stale: copy {}",
            path.display()
        );
    }
}
