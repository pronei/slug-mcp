pub mod apify;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

// ─── Default accounts (fallback when no instagram.toml exists) ───

const DEFAULT_ACCOUNTS: &[(&str, &str)] = &[
    ("uaboradining", "dining"),
    ("ucaborarecreation", "recreation"),
    ("ucaboraevents", "events"),
    ("ucsantacruz", "campus"),
    ("ucscstudentlife", "student_life"),
    ("ucscslugs", "athletics"),
    ("mchenrylibrary", "library"),
    ("ucscrecreation", "recreation"),
];

// ─── Config file models ───

#[derive(Debug, Deserialize)]
struct InstagramConfig {
    #[serde(default)]
    accounts: Vec<AccountEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountEntry {
    pub username: String,
    pub category: String,
}

// ─── MCP request types ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchCampusSocialRequest {
    /// Text to search for in post captions.
    pub query: Option<String>,
    /// Filter by category (e.g., "dining", "recreation", "events").
    pub category: Option<String>,
    /// Filter by specific Instagram username.
    pub account: Option<String>,
    /// Max results to return (default 10, max 25).
    pub limit: Option<u32>,
}

// ─── Cached post model (includes local image path) ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPost {
    pub username: String,
    pub full_name: String,
    pub category: String,
    pub caption: String,
    pub timestamp: String,
    pub url: String,
    pub short_code: String,
    pub likes: u64,
    pub comments: u64,
    pub hashtags: Vec<String>,
    pub location: Option<String>,
    pub image_path: Option<String>,
    pub is_video: bool,
}

// ─── Service ───

pub struct InstagramService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
    api_key: Option<String>,
    config_path: PathBuf,
    images_dir: PathBuf,
}

impl InstagramService {
    pub fn new(
        http: reqwest::Client,
        cache: Arc<CacheStore>,
        api_key: Option<String>,
        config_path: PathBuf,
        images_dir: PathBuf,
    ) -> Self {
        Self {
            http,
            cache,
            api_key,
            config_path,
            images_dir,
        }
    }

    /// Search cached Instagram posts. Returns formatted text output.
    pub async fn search(
        &self,
        query: Option<&str>,
        category: Option<&str>,
        account: Option<&str>,
        limit: u32,
    ) -> Result<(String, Vec<CachedPost>)> {
        let posts = self.load_cached_posts().await;

        if posts.is_empty() {
            if self.api_key.is_none() {
                return Ok((
                    "No cached Instagram data and Apify API key not configured.\n\
                     Set the `SLUG_MCP_APIFY_KEY` environment variable to enable campus social media scraping."
                        .to_string(),
                    vec![],
                ));
            }
            return Ok((
                "No cached Instagram data yet. Call `refresh_campus_social` to fetch latest posts."
                    .to_string(),
                vec![],
            ));
        }

        let mut filtered: Vec<&CachedPost> = posts.iter().collect();

        if let Some(cat) = category {
            let cat_lower = cat.to_lowercase();
            filtered.retain(|p| p.category.to_lowercase() == cat_lower);
        }

        if let Some(acct) = account {
            let acct_lower = acct.to_lowercase();
            filtered.retain(|p| p.username.to_lowercase() == acct_lower);
        }

        if let Some(q) = query {
            let q_lower = q.to_lowercase();
            let terms: Vec<&str> = q_lower.split_whitespace().collect();
            filtered.retain(|p| {
                let text = format!(
                    "{} {} {}",
                    p.caption.to_lowercase(),
                    p.hashtags.join(" ").to_lowercase(),
                    p.location.as_deref().unwrap_or("").to_lowercase()
                );
                terms.iter().all(|t| text.contains(t))
            });
        }

        filtered.truncate(limit as usize);

        if filtered.is_empty() {
            return Ok(("No matching posts found.".to_string(), vec![]));
        }

        let mut out = format!("Found {} matching posts:\n", filtered.len());
        for post in &filtered {
            out.push_str(&format!(
                "\n**@{}** — {}\n",
                post.username,
                format_relative_time(&post.timestamp),
            ));
            let caption = if post.caption.len() > 300 {
                format!("{}...", &post.caption[..297])
            } else {
                post.caption.clone()
            };
            if !caption.is_empty() {
                out.push_str(&format!("{}\n", caption));
            }
            if !post.hashtags.is_empty() {
                out.push_str(&format!(
                    "Tags: {}\n",
                    post.hashtags.iter().take(5).map(|h| format!("#{}", h)).collect::<Vec<_>>().join(" ")
                ));
            }
            out.push_str(&format!(
                "{} likes, {} comments | {}\n",
                post.likes, post.comments, post.url
            ));
        }

        let result_posts: Vec<CachedPost> = filtered.into_iter().cloned().collect();
        Ok((out, result_posts))
    }

    /// Fetch fresh posts from Apify and update the cache.
    pub async fn refresh(&self) -> Result<String> {
        let api_key = match &self.api_key {
            Some(key) => key.clone(),
            None => {
                return Ok(
                    "Apify API key not configured. Set the `SLUG_MCP_APIFY_KEY` environment variable.\n\
                     Get a free API token at https://console.apify.com/account/integrations".to_string()
                );
            }
        };

        let accounts = self.load_accounts();
        if accounts.is_empty() {
            return Ok("No Instagram accounts configured.".to_string());
        }

        let profile_urls: Vec<String> = accounts
            .iter()
            .map(|a| format!("https://www.instagram.com/{}/", a.username))
            .collect();

        let posts = apify::scrape_profiles(&self.http, &api_key, &profile_urls, 5)
            .await
            .context("Apify scrape failed")?;

        // Build account category lookup
        let category_map: std::collections::HashMap<String, String> = accounts
            .iter()
            .map(|a| (a.username.to_lowercase(), a.category.clone()))
            .collect();

        // Ensure images directory exists
        std::fs::create_dir_all(&self.images_dir).ok();

        // Convert to cached posts and download images
        let mut cached_posts: Vec<CachedPost> = Vec::new();
        for post in &posts {
            let category = category_map
                .get(&post.owner_username.to_lowercase())
                .cloned()
                .unwrap_or_else(|| "other".to_string());

            let image_path = if let Some(url) = &post.display_url {
                self.download_and_cache_image(&post.short_code, url).await
            } else {
                None
            };

            cached_posts.push(CachedPost {
                username: post.owner_username.clone(),
                full_name: post.owner_full_name.clone(),
                category,
                caption: post.caption.clone(),
                timestamp: post.timestamp.clone(),
                url: post.url.clone(),
                short_code: post.short_code.clone(),
                likes: post.likes_count,
                comments: post.comments_count,
                hashtags: post.hashtags.clone(),
                location: post.location_name.clone(),
                image_path,
                is_video: post.is_video,
            });
        }

        // Cache per-username and all-posts
        let mut by_username: std::collections::HashMap<String, Vec<&CachedPost>> =
            std::collections::HashMap::new();
        for post in &cached_posts {
            by_username
                .entry(post.username.to_lowercase())
                .or_default()
                .push(post);
        }

        for (username, user_posts) in &by_username {
            let key = format!("instagram:posts:{}", username);
            if let Ok(json) = serde_json::to_string(user_posts) {
                self.cache.set(&key, &json, 86400).await; // 24h TTL
            }
        }

        // Also cache the full list
        if let Ok(json) = serde_json::to_string(&cached_posts) {
            self.cache.set("instagram:all_posts", &json, 86400).await;
        }

        self.cleanup_old_images().await;

        Ok(format!(
            "Fetched {} posts from {} accounts ({}).",
            cached_posts.len(),
            accounts.len(),
            accounts
                .iter()
                .map(|a| format!("@{}", a.username))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// Get the local file path for a post's cached image (if it exists).
    // ─── Private helpers ───

    fn load_accounts(&self) -> Vec<AccountEntry> {
        if let Ok(content) = std::fs::read_to_string(&self.config_path) {
            if let Ok(config) = toml::from_str::<InstagramConfig>(&content) {
                if !config.accounts.is_empty() {
                    return config.accounts;
                }
            }
        }

        // Fall back to defaults
        DEFAULT_ACCOUNTS
            .iter()
            .map(|(username, category)| AccountEntry {
                username: username.to_string(),
                category: category.to_string(),
            })
            .collect()
    }

    async fn load_cached_posts(&self) -> Vec<CachedPost> {
        if let Some(json) = self.cache.get("instagram:all_posts").await {
            if let Ok(posts) = serde_json::from_str::<Vec<CachedPost>>(&json) {
                return posts;
            }
        }
        vec![]
    }

    async fn download_and_cache_image(&self, short_code: &str, url: &str) -> Option<String> {
        let path = self.images_dir.join(format!("{}.jpg", short_code));

        // Skip if already downloaded
        if path.exists() {
            return Some(path.to_string_lossy().to_string());
        }

        match apify::download_image(&self.http, url).await {
            Ok(bytes) => {
                if std::fs::write(&path, &bytes).is_ok() {
                    Some(path.to_string_lossy().to_string())
                } else {
                    None
                }
            }
            Err(_) => None,
        }
    }

    async fn cleanup_old_images(&self) {
        let Ok(entries) = std::fs::read_dir(&self.images_dir) else {
            return;
        };

        let seven_days_ago = std::time::SystemTime::now()
            - std::time::Duration::from_secs(7 * 24 * 3600);

        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                if let Ok(modified) = metadata.modified() {
                    if modified < seven_days_ago {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

fn format_relative_time(timestamp: &str) -> String {
    if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(timestamp) {
        let now = chrono::Utc::now();
        let diff = now.signed_duration_since(ts);

        if diff.num_hours() < 1 {
            format!("{}m ago", diff.num_minutes().max(1))
        } else if diff.num_hours() < 24 {
            format!("{}h ago", diff.num_hours())
        } else if diff.num_days() < 7 {
            format!("{}d ago", diff.num_days())
        } else {
            ts.format("%b %-d").to_string()
        }
    } else {
        timestamp.to_string()
    }
}
