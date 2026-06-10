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

    Ok(body
        .results
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
        .collect())
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
