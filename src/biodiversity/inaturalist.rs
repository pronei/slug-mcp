//! iNaturalist v1 observation fetch + markdown formatter.
//!
//! Public API: <https://api.inaturalist.org/v1/>. No API key required.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::util::now_pacific;

use super::Observation;

#[allow(clippy::too_many_arguments)]
pub(super) async fn fetch_inaturalist(
    http: &reqwest::Client,
    query: Option<&str>,
    lat: f64,
    lon: f64,
    radius_km: f64,
    days: u32,
    iconic: Option<&str>,
    limit: u32,
) -> Result<Vec<Observation>> {
    let mut params: Vec<(&str, String)> = vec![
        ("lat", format!("{}", lat)),
        ("lng", format!("{}", lon)),
        ("radius", format!("{}", radius_km)),
        ("per_page", format!("{}", limit)),
        ("order", "desc".to_string()),
        ("order_by", "observed_on".to_string()),
        (
            "d1",
            chrono::Utc::now()
                .checked_sub_signed(chrono::Duration::days(days as i64))
                .unwrap()
                .format("%Y-%m-%d")
                .to_string(),
        ),
    ];
    if let Some(q) = query {
        params.push(("q", q.to_string()));
    }
    if let Some(t) = iconic {
        params.push(("iconic_taxa", t.to_string()));
    }

    let resp = http
        .get("https://api.inaturalist.org/v1/observations")
        .query(&params)
        .send()
        .await
        .context("iNaturalist HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("iNaturalist returned HTTP {}", resp.status());
    }
    let body: InatResponse = resp.json().await.context("parsing iNaturalist JSON")?;

    Ok(to_observations(body))
}

fn to_observations(resp: InatResponse) -> Vec<Observation> {
    resp.results
        .into_iter()
        .map(|r| Observation {
            common_name: r
                .taxon
                .as_ref()
                .and_then(|t| t.preferred_common_name.clone()),
            scientific_name: r.taxon.as_ref().map(|t| t.name.clone()),
            observed_on: r.observed_on,
            location: r.place_guess,
            observer: r.user.map(|u| u.login),
            url: Some(r.uri),
            iconic_taxon: r.taxon.and_then(|t| t.iconic_taxon_name),
            count: None,
        })
        .collect()
}

#[derive(Deserialize)]
struct InatResponse {
    #[serde(default)]
    results: Vec<InatObservation>,
}
#[derive(Deserialize)]
struct InatObservation {
    uri: String,
    observed_on: Option<String>,
    place_guess: Option<String>,
    user: Option<InatUser>,
    taxon: Option<InatTaxon>,
}
#[derive(Deserialize)]
struct InatUser {
    login: String,
}
#[derive(Deserialize)]
struct InatTaxon {
    name: String,
    preferred_common_name: Option<String>,
    iconic_taxon_name: Option<String>,
}

pub(super) fn format_species(
    obs: &[Observation],
    lat: f64,
    lon: f64,
    radius_km: f64,
    days: u32,
) -> String {
    if obs.is_empty() {
        return format!(
            "No iNaturalist observations found within {:.0} km of ({:.3}, {:.3}) in the last {} days.\n",
            radius_km, lat, lon, days
        );
    }
    let mut out = format!(
        "# iNaturalist observations — ({:.3}, {:.3}) · {:.0} km radius · last {} days\n\n",
        lat, lon, radius_km, days
    );
    out.push_str(&format!("_{} observations._\n\n", obs.len()));

    for o in obs {
        let name = match (&o.common_name, &o.scientific_name) {
            (Some(c), Some(s)) => format!("**{}** (_{}_)", c, s),
            (Some(c), None) => format!("**{}**", c),
            (None, Some(s)) => format!("_{}_", s),
            (None, None) => "**unknown**".to_string(),
        };
        out.push_str(&format!("- {}", name));
        if let Some(t) = &o.iconic_taxon {
            out.push_str(&format!(" · {}", t));
        }
        if let Some(d) = &o.observed_on {
            out.push_str(&format!(" · {}", d));
        }
        if let Some(l) = &o.location {
            out.push_str(&format!(" · {}", l));
        }
        if let Some(u) = &o.observer {
            out.push_str(&format!(" · @{}", u));
        }
        if let Some(url) = &o.url {
            out.push_str(&format!(" · [iNat]({})", url));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "\n_Source: iNaturalist v1 API. Last updated: {}_\n",
        now_pacific().format("%-I:%M %p")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const INAT_FIXTURE: &str = include_str!("fixtures/inat_observations.json");

    #[test]
    fn parse_live_fixture_and_map() {
        // Trimmed live capture (2026-07-07): genus-level record has a null
        // preferred_common_name; one record omits place_guess/photos entirely.
        let resp: InatResponse = serde_json::from_str(INAT_FIXTURE).unwrap();
        let obs = to_observations(resp);
        assert_eq!(obs.len(), 3);

        // Null common name → scientific name only.
        assert!(obs[0].common_name.is_none());
        assert_eq!(obs[0].scientific_name.as_deref(), Some("Anthopleura"));
        assert_eq!(obs[0].iconic_taxon.as_deref(), Some("Animalia"));
        assert_eq!(obs[0].observer.as_deref(), Some("marusya12"));
        assert_eq!(
            obs[0].url.as_deref(),
            Some("https://www.inaturalist.org/observations/378981231")
        );

        // Missing place_guess key → location None; parse still succeeds.
        assert!(obs[1].location.is_none());
        assert_eq!(obs[1].common_name.as_deref(), Some("Oleander Aphid"));

        assert_eq!(obs[2].common_name.as_deref(), Some("Acorn Woodpecker"));
        assert_eq!(
            obs[2].location.as_deref(),
            Some("Red Hill Rd, Santa Cruz, CA, US")
        );
    }

    #[test]
    fn format_species_renders_null_common_name_as_scientific() {
        let resp: InatResponse = serde_json::from_str(INAT_FIXTURE).unwrap();
        let obs = to_observations(resp);
        let out = format_species(&obs, 36.9741, -122.0308, 25.0, 30);
        assert!(out.contains("_Anthopleura_"));
        assert!(out.contains("**Oleander Aphid**"));
        assert!(out.contains("@sbatory"));
        assert!(out.contains("3 observations."));
    }

    #[test]
    fn format_species_empty_message() {
        let out = format_species(&[], 36.9741, -122.0308, 25.0, 30);
        assert!(out.contains("No iNaturalist observations found"));
    }

    #[test]
    fn missing_results_key_defaults_empty() {
        // Unknown envelope without `results` (e.g. an error body that still
        // returns 200) degrades to zero observations, not a parse failure.
        let resp: InatResponse =
            serde_json::from_str(r#"{"error": "Internal Server Error", "status": 500}"#).unwrap();
        assert!(to_observations(resp).is_empty());
    }

    #[test]
    fn truncated_json_errors_gracefully() {
        let cut = &INAT_FIXTURE[..INAT_FIXTURE.len() / 2];
        assert!(serde_json::from_str::<InatResponse>(cut).is_err());
    }

    #[test]
    fn observation_missing_uri_errors() {
        // uri is the one required field — its absence is malformed data and
        // fails the parse with a clear serde message.
        let body = r#"{"results": [{"observed_on": "2026-07-06"}]}"#;
        let err = serde_json::from_str::<InatResponse>(body)
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(err.contains("uri"), "got: {err}");
    }
}
