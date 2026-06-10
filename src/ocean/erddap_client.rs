use std::sync::Arc;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::cache::CacheStore;

const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;
const MAX_TABLEDAP_ROWS: usize = 50_000;
const MAX_GRIDDAP_ROWS: usize = 250_000;

#[derive(Debug, Clone, Deserialize)]
pub struct ErddapResponse {
    pub table: ErddapTable,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErddapTable {
    #[serde(rename = "columnNames")]
    pub column_names: Vec<String>,
    #[serde(rename = "columnTypes")]
    #[allow(dead_code)]
    pub column_types: Vec<String>,
    #[serde(rename = "columnUnits")]
    #[allow(dead_code)]
    pub column_units: Vec<Option<String>>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

impl ErddapTable {
    pub fn col_index(&self, name: &str) -> Option<usize> {
        self.column_names.iter().position(|c| c == name)
    }

    #[allow(dead_code)]
    pub fn get_f64(&self, row: usize, col: usize) -> Option<f64> {
        self.rows.get(row)?.get(col)?.as_f64()
    }

    #[allow(dead_code)]
    pub fn get_str(&self, row: usize, col: usize) -> Option<&str> {
        self.rows.get(row)?.get(col)?.as_str()
    }
}

pub struct TabledapQuery {
    pub variables: Vec<String>,
    pub constraints: Vec<String>,
    pub order_by_mean: Option<String>,
    pub order_by: Option<String>,
    /// Variables whose `<name>_qc_agg` companion should be constrained to 1.
    /// The client auto-appends `<var>_qc_agg` to the variable list and adds
    /// `&<var>_qc_agg=1` as a constraint.
    pub qc_vars: Vec<String>,
}

impl TabledapQuery {
    pub fn new(variables: Vec<String>) -> Self {
        Self {
            variables,
            constraints: Vec::new(),
            order_by_mean: None,
            order_by: None,
            qc_vars: Vec::new(),
        }
    }

    pub fn constraint(mut self, expr: impl Into<String>) -> Self {
        self.constraints.push(expr.into());
        self
    }

    pub fn with_qc(mut self, vars: Vec<String>) -> Self {
        self.qc_vars = vars;
        self
    }

    pub fn order_by(mut self, expr: impl Into<String>) -> Self {
        self.order_by = Some(expr.into());
        self
    }

    pub fn order_by_mean(mut self, expr: impl Into<String>) -> Self {
        self.order_by_mean = Some(expr.into());
        self
    }
}

#[derive(Clone)]
pub struct ErddapClient {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl ErddapClient {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    /// Shared cache handle, so typed-snapshot fetchers can cache their parsed
    /// result under the same TTL the single-tool string paths use.
    pub fn cache(&self) -> &Arc<CacheStore> {
        &self.cache
    }

    pub async fn tabledap(
        &self,
        server: &str,
        dataset_id: &str,
        query: TabledapQuery,
    ) -> Result<ErddapResponse> {
        if query.variables.is_empty() {
            bail!("TabledapQuery: variables must not be empty (invariant: always pin variables)");
        }

        let has_time_constraint = query.constraints.iter().any(|c| c.starts_with("time"));
        if !has_time_constraint {
            bail!(
                "TabledapQuery: at least one time constraint required (invariant: always pin time)"
            );
        }

        let mut all_vars = query.variables.clone();
        let mut all_constraints = query.constraints.clone();

        for var in &query.qc_vars {
            let qc_var = format!("{}_qc_agg", var);
            if !all_vars.contains(&qc_var) {
                all_vars.push(qc_var.clone());
            }
            all_constraints.push(format!("{}_qc_agg=1", var));
        }

        let var_str = all_vars.join(",");
        let constraint_str = all_constraints
            .iter()
            .map(|c| format!("&{}", c))
            .collect::<String>();

        let mut url = format!(
            "{}/tabledap/{}.json?{}{}",
            server.trim_end_matches('/'),
            dataset_id,
            var_str,
            constraint_str,
        );

        if let Some(ref obm) = query.order_by_mean {
            url.push_str(&format!("&orderByMean(\"{}\")", obm));
        }
        if let Some(ref ob) = query.order_by {
            url.push_str(&format!("&orderBy(\"{}\")", ob));
        }

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("ERDDAP request failed: {}", e))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("no matching results") || body.contains("outside of the variable") {
                bail!(
                    "ERDDAP query returned no data ({}). The dataset may have a lag. \
                     Try a wider time range. Server message: {}",
                    status,
                    extract_erddap_message(&body),
                );
            }
            bail!(
                "ERDDAP returned HTTP {} for dataset '{}': {}",
                status,
                dataset_id,
                extract_erddap_message(&body),
            );
        }

        let bytes = resp.bytes().await?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            bail!(
                "ERDDAP response too large ({:.1} MB, limit {:.0} MB). \
                 Narrow your time range or add spatial constraints.",
                bytes.len() as f64 / 1_048_576.0,
                MAX_RESPONSE_BYTES as f64 / 1_048_576.0,
            );
        }

        let response: ErddapResponse = serde_json::from_slice(&bytes).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse ERDDAP JSON from '{}': {}",
                dataset_id,
                e
            )
        })?;

        if response.table.rows.len() > MAX_TABLEDAP_ROWS {
            bail!(
                "ERDDAP response has {} rows (limit {}). Narrow your query.",
                response.table.rows.len(),
                MAX_TABLEDAP_ROWS,
            );
        }

        Ok(response)
    }

    /// Griddap query. Each element of `selectors` is a complete
    /// `varname[dim1][dim2]…` string. Multiple selectors are comma-joined per
    /// ERDDAP griddap syntax. Auto-translates negative longitudes to 0-360 when
    /// `lon_0_360` is true.
    pub async fn griddap(
        &self,
        server: &str,
        dataset_id: &str,
        selectors: &[String],
    ) -> Result<ErddapResponse> {
        if selectors.is_empty() {
            bail!("GriddapQuery: at least one selector required");
        }

        let selector_str = selectors.join(",");
        let url = format!(
            "{}/griddap/{}.json?{}",
            server.trim_end_matches('/'),
            dataset_id,
            selector_str,
        );

        let resp = self.http.get(&url).send().await
            .map_err(|e| anyhow::anyhow!("ERDDAP griddap request failed: {}", e))?;

        let status = resp.status();

        if status == reqwest::StatusCode::FOUND || status == reqwest::StatusCode::MOVED_PERMANENTLY {
            let location = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");
            let redirect_resp = self.http.get(location).send().await
                .map_err(|e| anyhow::anyhow!("ERDDAP redirect failed: {}", e))?;
            return self.parse_griddap_response(redirect_resp, dataset_id).await;
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "ERDDAP griddap returned HTTP {} for '{}': {}",
                status,
                dataset_id,
                extract_erddap_message(&body),
            );
        }

        self.parse_griddap_response(resp, dataset_id).await
    }

    async fn parse_griddap_response(
        &self,
        resp: reqwest::Response,
        dataset_id: &str,
    ) -> Result<ErddapResponse> {
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "ERDDAP griddap returned HTTP {} for '{}': {}",
                status,
                dataset_id,
                extract_erddap_message(&body),
            );
        }

        let bytes = resp.bytes().await?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            bail!(
                "ERDDAP griddap response too large ({:.1} MB, limit {:.0} MB). Use stride.",
                bytes.len() as f64 / 1_048_576.0,
                MAX_RESPONSE_BYTES as f64 / 1_048_576.0,
            );
        }

        let response: ErddapResponse = serde_json::from_slice(&bytes).map_err(|e| {
            anyhow::anyhow!("failed to parse ERDDAP griddap JSON from '{}': {}", dataset_id, e)
        })?;

        if response.table.rows.len() > MAX_GRIDDAP_ROWS {
            bail!(
                "ERDDAP griddap response has {} rows (limit {}). Use stride or narrow bbox.",
                response.table.rows.len(),
                MAX_GRIDDAP_ROWS,
            );
        }

        Ok(response)
    }

    #[allow(dead_code)]
    pub async fn tabledap_cached(
        &self,
        server: &str,
        dataset_id: &str,
        query: TabledapQuery,
        cache_key: &str,
        ttl_secs: u64,
    ) -> Result<ErddapResponse> {
        let server = server.to_string();
        let dataset_id = dataset_id.to_string();
        self.cache
            .get_or_fetch(cache_key, ttl_secs, move || {
                let client = self.clone();
                let server = server.clone();
                let dataset_id = dataset_id.clone();
                async move { client.tabledap(&server, &dataset_id, query).await }
            })
            .await
    }
}

/// Build a griddap selector string like `varname[(time_start):(time_end)][(lat_start):(lat_end)][(lon_start):(lon_end)]`.
/// For a single point, set start == end. Use `last` for the most recent timestep.
pub fn grid_selector(
    var: &str,
    time: &str,
    lat_range: (f64, f64),
    lon_range: (f64, f64),
) -> String {
    format!(
        "{}[{}][({:.4}):({:.4})][({:.4}):({:.4})]",
        var, time, lat_range.0, lat_range.1, lon_range.0, lon_range.1,
    )
}

/// Like `grid_selector` but with stride.
pub fn grid_selector_stride(
    var: &str,
    time: &str,
    lat_range: (f64, f64),
    lat_stride: usize,
    lon_range: (f64, f64),
    lon_stride: usize,
) -> String {
    format!(
        "{}[{}][({:.4}):{}:({:.4})][({:.4}):{}:({:.4})]",
        var, time, lat_range.0, lat_stride, lat_range.1, lon_range.0, lon_stride, lon_range.1,
    )
}

/// Like `grid_selector` but with an extra altitude dimension (for datasets
/// like VIIRS chl-a that have a degenerate altitude axis).
#[allow(dead_code)]
pub fn grid_selector_with_alt(
    var: &str,
    time: &str,
    alt: f64,
    lat_range: (f64, f64),
    lon_range: (f64, f64),
) -> String {
    format!(
        "{}[{}][({})][({:.4}):({:.4})][({:.4}):({:.4})]",
        var, time, alt, lat_range.0, lat_range.1, lon_range.0, lon_range.1,
    )
}

/// Convert Western-hemisphere longitude (-180..0) to 0-360 convention.
pub fn lon_to_360(lon: f64) -> f64 {
    if lon < 0.0 { lon + 360.0 } else { lon }
}

fn extract_erddap_message(body: &str) -> String {
    if let Some(start) = body.find("message=\"") {
        let rest = &body[start + 9..];
        if let Some(end) = rest.find('"') {
            return rest[..end].to_string();
        }
    }
    if let Some(start) = body.find("message=") {
        let rest = &body[start + 8..];
        if let Some(end) = rest.find(';') {
            return rest[..end].trim_matches('"').to_string();
        }
    }
    body.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erddap_table_accessors() {
        let table = ErddapTable {
            column_names: vec!["time".into(), "z".into(), "temp".into()],
            column_types: vec!["String".into(), "double".into(), "double".into()],
            column_units: vec![Some("UTC".into()), Some("m".into()), Some("°C".into())],
            rows: vec![vec![
                serde_json::json!("2026-04-28T00:00:00Z"),
                serde_json::json!(-1.0),
                serde_json::json!(13.5),
            ]],
        };

        assert_eq!(table.col_index("time"), Some(0));
        assert_eq!(table.col_index("z"), Some(1));
        assert_eq!(table.col_index("missing"), None);
        assert_eq!(table.get_str(0, 0), Some("2026-04-28T00:00:00Z"));
        assert_eq!(table.get_f64(0, 1), Some(-1.0));
        assert_eq!(table.get_f64(0, 2), Some(13.5));
    }

    #[test]
    fn extract_message() {
        let body = r#"Error {
    code=404;
    message="Not Found: Your query produced no matching results.";
}"#;
        assert!(extract_erddap_message(body).contains("Not Found"));
    }
}
