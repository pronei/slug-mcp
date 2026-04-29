//! Slug-themed progress notifications + UCSC SVG preamble for slow tools.
//!
//! Wraps every tool dispatch in [`slug_wrap`]. If the tool hasn't completed
//! within `FIRST_PING`, the wrapper begins emitting `notifications/progress`
//! every `REPEAT` with rotating slug-themed messages until the tool returns.
//! When elapsed exceeds `SLOW_THRESHOLD`, [`maybe_prepend_slug`] adds a tiny
//! UCSC blue + gold banana-slug SVG to the result preamble.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine};
use rmcp::model::{
    CallToolResult, Content, ProgressNotificationParam, ProgressToken, RawContent,
};
use rmcp::service::Peer;
use rmcp::RoleServer;

const FIRST_PING: Duration = Duration::from_millis(500);
const REPEAT: Duration = Duration::from_millis(1500);
const SLOW_THRESHOLD: Duration = Duration::from_millis(500);

/// Rotating slug-step descriptions. `{tool}` is substituted with the tool name.
const STEPS: &[&str] = &[
    "🐌 sniffing around for `{tool}`…",
    "🐌 still crawling — `{tool}` is taking a moment…",
    "🐌 munching through data for `{tool}`…",
    "🐌 almost there with `{tool}`…",
];

/// Animated SVG: SMIL `<animate>` morphs the body path through 5 keyframes
/// (contraction wave tail → head) on a 1.6s loop. Renderers without SMIL
/// support (e.g. terminal clients) fall back to the initial frame, which is
/// still a valid static slug.
const SLUG_SVG: &str = include_str!("../assets/slug-crawl.svg");

fn slug_data_uri() -> &'static str {
    static URI: OnceLock<String> = OnceLock::new();
    URI.get_or_init(|| {
        format!(
            "data:image/svg+xml;base64,{}",
            STANDARD.encode(SLUG_SVG.as_bytes())
        )
    })
}

/// Run `fut` while emitting slug progress notifications if it runs long.
///
/// Returns `(result, elapsed)`. The watchdog only fires when `progress_token`
/// is `Some` — clients that don't request progress incur zero notification
/// traffic.
pub async fn slug_wrap<F, T>(
    peer: &Peer<RoleServer>,
    progress_token: Option<ProgressToken>,
    tool: &str,
    fut: F,
) -> (T, Duration)
where
    F: std::future::Future<Output = T>,
{
    let start = Instant::now();
    let watcher = async {
        let Some(token) = progress_token else {
            std::future::pending::<()>().await;
            unreachable!()
        };
        tokio::time::sleep(FIRST_PING).await;
        for i in 0u64.. {
            let msg = STEPS[i as usize % STEPS.len()].replace("{tool}", tool);
            let _ = peer
                .notify_progress(ProgressNotificationParam {
                    progress_token: token.clone(),
                    progress: i as f64,
                    total: None,
                    message: Some(msg),
                })
                .await;
            tokio::time::sleep(REPEAT).await;
        }
        unreachable!()
    };

    tokio::pin!(fut);
    tokio::pin!(watcher);
    tokio::select! {
        biased;
        out = &mut fut => (out, start.elapsed()),
        _ = &mut watcher => unreachable!(),
    }
}

/// Prepend a UCSC slug SVG + elapsed-time caption to the first text content
/// block, but only if the call exceeded [`SLOW_THRESHOLD`].
pub fn maybe_prepend_slug(mut result: CallToolResult, elapsed: Duration) -> CallToolResult {
    if elapsed < SLOW_THRESHOLD {
        return result;
    }
    let preamble = format!(
        "![🐌]({})  *the slug crawled for {} ms*\n\n",
        slug_data_uri(),
        elapsed.as_millis()
    );
    if let Some(first) = result.content.iter_mut().find_map(|c| match &mut c.raw {
        RawContent::Text(t) => Some(t),
        _ => None,
    }) {
        first.text = format!("{preamble}{}", first.text);
    } else {
        result.content.insert(0, Content::text(preamble));
    }
    result
}
