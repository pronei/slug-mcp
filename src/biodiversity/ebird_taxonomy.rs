//! Local eBird taxonomy index for free-text → 6-letter species code resolution.
//!
//! eBird has no name-search endpoint, so we build the index ourselves:
//! 1. `GET /product/spplist/US-CA?fmt=json` → ~700 California species codes
//! 2. `GET /ref/taxonomy/ebird?species=<csv>&fmt=json` → taxonomy rows for those codes
//! 3. Build a lowercase string → species_code map indexing common name, scientific
//!    name, banding codes (e.g. "BHGR" for Black-headed Grosbeak), and 4-letter
//!    eBird name codes.
//!
//! The index is wrapped in a `tokio::sync::OnceCell` on the service so that the
//! first species-typed query pays the cold-start cost (~600 ms) and subsequent
//! lookups are HashMap-fast. Re-fetched on process restart.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct TaxonEntry {
    pub species_code: String,
    pub com_name: String,
    pub sci_name: String,
}

pub struct TaxonomyIndex {
    /// Lowercased common name, scientific name, banding code, or 4-letter name code → species_code.
    by_lower_name: HashMap<String, String>,
    /// All taxonomy entries kept for substring/fuzzy fallback. Order preserved
    /// from the taxonomy endpoint (which sorts by `taxonOrder`).
    all_entries: Vec<TaxonEntry>,
    /// species_code → display entry. Used to render "did you mean" lists.
    by_code: HashMap<String, TaxonEntry>,
}

#[derive(Debug)]
pub enum SpeciesLookup {
    /// Resolved to a single species code.
    Exact(String),
    /// Multiple plausible candidates — caller should ask the user to disambiguate.
    Ambiguous(Vec<TaxonEntry>),
    /// No clear match; up to 3 fuzzy suggestions to surface.
    NotFound(Vec<TaxonEntry>),
}

impl TaxonomyIndex {
    /// Resolve a free-text query (common name, scientific name, banding code,
    /// 4-letter name code, or a literal 6-char species code) to a species code.
    pub fn lookup(&self, query: &str) -> SpeciesLookup {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return SpeciesLookup::NotFound(Vec::new());
        }

        // Fast path: literal 6-char alphanumeric species code (e.g. "norcar").
        if q.len() == 6
            && q.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            if let Some(entry) = self.by_code.get(&q) {
                return SpeciesLookup::Exact(entry.species_code.clone());
            }
            // Unknown code, but the format is right — pass through; eBird will 404 if invalid.
            return SpeciesLookup::Exact(q);
        }

        // Exact lowercase match against any indexed string.
        if let Some(code) = self.by_lower_name.get(&q) {
            return SpeciesLookup::Exact(code.clone());
        }

        // Substring scan on common name (most useful field for users).
        let hits: Vec<&TaxonEntry> = self
            .all_entries
            .iter()
            .filter(|e| {
                e.com_name.to_lowercase().contains(&q) || e.sci_name.to_lowercase().contains(&q)
            })
            .collect();

        match hits.len() {
            0 => SpeciesLookup::NotFound(self.fuzzy_suggestions(&q, 3)),
            1 => SpeciesLookup::Exact(hits[0].species_code.clone()),
            2 | 3 => SpeciesLookup::Ambiguous(hits.into_iter().cloned().collect()),
            _ => {
                // Too many substring hits — narrow to those whose common name
                // *starts with* the query, otherwise return the first 3 as ambiguous.
                let prefix: Vec<&TaxonEntry> = hits
                    .iter()
                    .copied()
                    .filter(|e| e.com_name.to_lowercase().starts_with(&q))
                    .collect();
                if prefix.len() == 1 {
                    SpeciesLookup::Exact(prefix[0].species_code.clone())
                } else if !prefix.is_empty() && prefix.len() <= 3 {
                    SpeciesLookup::Ambiguous(prefix.into_iter().cloned().collect())
                } else {
                    SpeciesLookup::Ambiguous(hits.into_iter().take(3).cloned().collect())
                }
            }
        }
    }

    /// Rough "did you mean" suggestions for a query that didn't match anything.
    /// Ranks by shared-character count then length-similarity to the query.
    fn fuzzy_suggestions(&self, q: &str, n: usize) -> Vec<TaxonEntry> {
        let q_chars: std::collections::HashSet<char> = q.chars().collect();
        let mut scored: Vec<(usize, isize, &TaxonEntry)> = self
            .all_entries
            .iter()
            .map(|e| {
                let lower = e.com_name.to_lowercase();
                let shared = lower.chars().filter(|c| q_chars.contains(c)).count();
                let len_diff = (lower.chars().count() as isize - q.chars().count() as isize).abs();
                (shared, -len_diff, e)
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        scored
            .into_iter()
            .take(n)
            .map(|(_, _, e)| e.clone())
            .collect()
    }
}

/// Render a list of taxonomy entries as markdown bullets — used when a lookup
/// is `Ambiguous` or `NotFound` so the caller can show "did you mean..."
pub fn format_candidates(entries: &[TaxonEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        out.push_str(&format!(
            "- **{}** (_{}_) — code `{}`\n",
            e.com_name, e.sci_name, e.species_code
        ));
    }
    out
}

#[derive(Deserialize)]
struct TaxonRow {
    #[serde(rename = "speciesCode")]
    species_code: String,
    #[serde(rename = "comName")]
    com_name: String,
    #[serde(rename = "sciName")]
    sci_name: String,
    #[serde(rename = "bandingCodes", default)]
    banding_codes: Vec<String>,
    #[serde(rename = "comNameCodes", default)]
    com_name_codes: Vec<String>,
}

/// Build the taxonomy index by fetching the California species list, then the
/// taxonomy rows for just those codes. Caller is responsible for caching the
/// result (we don't write to `CacheStore` from here — keeps this function pure
/// and easier to test).
pub async fn build_index(http: &reqwest::Client, key: &str) -> Result<TaxonomyIndex> {
    // 1. California species list (~700 codes). Returns a flat array of strings.
    let spplist: Vec<String> = http
        .get("https://api.ebird.org/v2/product/spplist/US-CA")
        .header("X-eBirdApiToken", key)
        .send()
        .await
        .context("eBird spplist HTTP request failed")?
        .error_for_status()
        .context("eBird spplist non-success status")?
        .json()
        .await
        .context("parsing eBird spplist JSON")?;

    if spplist.is_empty() {
        anyhow::bail!("eBird spplist returned no species codes");
    }

    // 2. Taxonomy rows for those codes. The `species=` param is a CSV.
    let csv = spplist.join(",");
    let rows: Vec<TaxonRow> = http
        .get("https://api.ebird.org/v2/ref/taxonomy/ebird")
        .header("X-eBirdApiToken", key)
        .query(&[("fmt", "json"), ("species", csv.as_str())])
        .send()
        .await
        .context("eBird taxonomy HTTP request failed")?
        .error_for_status()
        .context("eBird taxonomy non-success status")?
        .json()
        .await
        .context("parsing eBird taxonomy JSON")?;

    // 3. Build the index.
    let mut by_lower_name: HashMap<String, String> = HashMap::with_capacity(rows.len() * 3);
    let mut by_code: HashMap<String, TaxonEntry> = HashMap::with_capacity(rows.len());
    let mut all_entries: Vec<TaxonEntry> = Vec::with_capacity(rows.len());

    for r in rows {
        let entry = TaxonEntry {
            species_code: r.species_code.clone(),
            com_name: r.com_name.clone(),
            sci_name: r.sci_name.clone(),
        };
        by_lower_name.insert(r.com_name.to_lowercase(), r.species_code.clone());
        by_lower_name.insert(r.sci_name.to_lowercase(), r.species_code.clone());
        for b in r.banding_codes {
            by_lower_name.insert(b.to_lowercase(), r.species_code.clone());
        }
        for c in r.com_name_codes {
            by_lower_name.insert(c.to_lowercase(), r.species_code.clone());
        }
        by_code.insert(r.species_code.clone(), entry.clone());
        all_entries.push(entry);
    }

    Ok(TaxonomyIndex {
        by_lower_name,
        all_entries,
        by_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> TaxonomyIndex {
        let entries = vec![
            TaxonEntry {
                species_code: "brnpel".to_string(),
                com_name: "Brown Pelican".to_string(),
                sci_name: "Pelecanus occidentalis".to_string(),
            },
            TaxonEntry {
                species_code: "snoplo5".to_string(),
                com_name: "Snowy Plover".to_string(),
                sci_name: "Charadrius nivosus".to_string(),
            },
            TaxonEntry {
                species_code: "houspa".to_string(),
                com_name: "House Sparrow".to_string(),
                sci_name: "Passer domesticus".to_string(),
            },
            TaxonEntry {
                species_code: "sonspa".to_string(),
                com_name: "Song Sparrow".to_string(),
                sci_name: "Melospiza melodia".to_string(),
            },
            TaxonEntry {
                species_code: "calcon".to_string(),
                com_name: "California Condor".to_string(),
                sci_name: "Gymnogyps californianus".to_string(),
            },
        ];
        let mut by_lower_name = HashMap::new();
        let mut by_code = HashMap::new();
        for e in &entries {
            by_lower_name.insert(e.com_name.to_lowercase(), e.species_code.clone());
            by_lower_name.insert(e.sci_name.to_lowercase(), e.species_code.clone());
            by_code.insert(e.species_code.clone(), e.clone());
        }
        TaxonomyIndex {
            by_lower_name,
            all_entries: entries,
            by_code,
        }
    }

    #[test]
    fn six_char_code_pass_through() {
        let idx = fixture();
        match idx.lookup("brnpel") {
            SpeciesLookup::Exact(c) => assert_eq!(c, "brnpel"),
            other => panic!("expected Exact, got {:?}", other),
        }
    }

    #[test]
    fn exact_common_name() {
        let idx = fixture();
        match idx.lookup("Brown Pelican") {
            SpeciesLookup::Exact(c) => assert_eq!(c, "brnpel"),
            other => panic!("expected Exact, got {:?}", other),
        }
    }

    #[test]
    fn substring_unique() {
        let idx = fixture();
        match idx.lookup("pelican") {
            SpeciesLookup::Exact(c) => assert_eq!(c, "brnpel"),
            other => panic!("expected Exact, got {:?}", other),
        }
    }

    #[test]
    fn substring_ambiguous_returns_candidates() {
        let idx = fixture();
        match idx.lookup("sparrow") {
            SpeciesLookup::Ambiguous(v) => assert_eq!(v.len(), 2),
            other => panic!("expected Ambiguous, got {:?}", other),
        }
    }

    #[test]
    fn unknown_returns_suggestions() {
        let idx = fixture();
        match idx.lookup("zzz no such bird zzz") {
            SpeciesLookup::NotFound(v) => assert!(!v.is_empty(), "should give suggestions"),
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn case_insensitive() {
        let idx = fixture();
        match idx.lookup("BROWN PELICAN") {
            SpeciesLookup::Exact(c) => assert_eq!(c, "brnpel"),
            other => panic!("expected Exact, got {:?}", other),
        }
    }

    #[test]
    fn format_candidates_renders() {
        let idx = fixture();
        let out = format_candidates(&idx.all_entries[..2]);
        assert!(out.contains("Brown Pelican"));
        assert!(out.contains("brnpel"));
        assert!(out.contains("Pelecanus"));
    }
}
