use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const APIFY_BASE_URL: &str = "https://api.apify.com/v2";
const INSTAGRAM_SCRAPER_ACTOR: &str = "apify~instagram-scraper";

/// A single Instagram post returned by Apify.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstagramPost {
    #[serde(default)]
    pub owner_username: String,
    #[serde(default)]
    pub owner_full_name: String,
    #[serde(default)]
    pub caption: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub short_code: String,
    #[serde(default)]
    pub likes_count: u64,
    #[serde(default)]
    pub comments_count: u64,
    #[serde(default)]
    pub hashtags: Vec<String>,
    #[serde(default)]
    pub location_name: Option<String>,
    #[serde(default)]
    pub display_url: Option<String>,
    #[serde(default)]
    pub is_video: bool,
}

/// Scrape recent posts from a list of Instagram profile URLs via Apify.
///
/// Uses the sync endpoint which waits for results (up to 300s timeout).
pub async fn scrape_profiles(
    http: &reqwest::Client,
    api_key: &str,
    profile_urls: &[String],
    results_limit: u32,
) -> Result<Vec<InstagramPost>> {
    let input = serde_json::json!({
        "directUrls": profile_urls,
        "resultsType": "posts",
        "resultsLimit": results_limit,
    });

    let url = format!(
        "{}/acts/{}/run-sync-get-dataset-items",
        APIFY_BASE_URL, INSTAGRAM_SCRAPER_ACTOR
    );

    let resp = http
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&input)
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await
        .context("failed to reach Apify API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Apify returned status {}: {}", status, &body[..body.len().min(200)]);
    }

    let posts: Vec<InstagramPost> = resp
        .json()
        .await
        .context("failed to parse Apify response")?;

    Ok(posts)
}

/// Download an image from a URL and return the bytes.
pub async fn download_image(http: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = http
        .get(url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("failed to download image")?
        .error_for_status()
        .context("image download returned error status")?;

    let bytes = resp.bytes().await.context("failed to read image bytes")?;
    Ok(bytes.to_vec())
}
