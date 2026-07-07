use std::fmt::Write;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use scraper::Html;
#[cfg(feature = "auth")]
use scraper::Selector;

use crate::util::{FuzzyMatcher, selectors};

selectors! {
    SEL_ANCHOR_NAME => "a[name]",
    SEL_LINK => "a[href]",
    SEL_IMG => "img",
    SEL_TD => "td",
    SEL_LONG_MENU => "td.longmenugridheader, div.longmenucolmenucat, div.longmenucoldispname",
    SEL_RECIPE => "div.labelrecipe",
    SEL_INGREDIENTS => "span.labelingredientsvalue",
    SEL_ALLERGENS => "span.labelallergensvalue",
    SEL_SHORT_MENU => "div.shortmenumeals, div.shortmenucats, div.shortmenurecipes",
    SEL_SCHEMA => "div[itemtype]",
    SEL_NAME => "meta[itemprop=\"name\"]",
    SEL_HOURS => "meta[itemprop=\"openingHours\"]",
    SEL_SPEC => "div[itemprop=\"openingHoursSpecification\"]",
    SEL_TIME => "time",
}

// --- Dining hall definitions ---

/// Distinguishes full-service residential dining halls (which serve multi-meal
/// menus we scrape) from cafes (which don't). The pre-warmer and "all halls"
/// queries default to `Full` so cafes don't pollute the menu output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HallKind {
    Full,
    Cafe,
}

pub struct DiningHall {
    pub name: &'static str,
    pub short_names: &'static [&'static str],
    pub location_num: &'static str,
    pub location_name: &'static str,
    pub kind: HallKind,
}

pub static DINING_HALLS: &[DiningHall] = &[
    DiningHall {
        name: "John R. Lewis & College Nine Dining Hall",
        short_names: &["lewis", "college nine", "c9", "nine"],
        location_num: "40",
        location_name: "John+R.+Lewis+%26+College+Nine+Dining+Hall",
        kind: HallKind::Full,
    },
    DiningHall {
        name: "Cowell & Stevenson Dining Hall",
        short_names: &["cowell", "stevenson"],
        location_num: "05",
        location_name: "Cowell+%26+Stevenson+Dining+Hall",
        kind: HallKind::Full,
    },
    DiningHall {
        name: "Crown & Merrill Dining Hall",
        short_names: &["crown", "merrill"],
        location_num: "20",
        location_name: "Crown+%26+Merrill+Dining+Hall",
        kind: HallKind::Full,
    },
    DiningHall {
        name: "Porter & Kresge Dining Hall",
        short_names: &["porter", "kresge"],
        location_num: "25",
        location_name: "Porter+%26+Kresge+Dining+Hall",
        kind: HallKind::Full,
    },
    DiningHall {
        name: "Rachel Carson & Oakes Dining Hall",
        short_names: &["carson", "oakes", "rco"],
        location_num: "30",
        location_name: "Rachel+Carson+%26+Oakes+Dining+Hall",
        kind: HallKind::Full,
    },
    DiningHall {
        name: "Banana Joe's",
        short_names: &["banana joe", "banana"],
        location_num: "21",
        location_name: "Banana+Joe%27s",
        kind: HallKind::Cafe,
    },
    DiningHall {
        name: "Oakes Cafe",
        short_names: &["oakes cafe"],
        location_num: "23",
        location_name: "Oakes+Cafe",
        kind: HallKind::Cafe,
    },
    DiningHall {
        name: "Global Village Cafe",
        short_names: &["global village", "global"],
        location_num: "46",
        location_name: "Global+Village+Cafe",
        kind: HallKind::Cafe,
    },
    DiningHall {
        name: "Owl's Nest Cafe",
        short_names: &["owl", "owls nest"],
        location_num: "24",
        location_name: "Owl%27s+Nest+Cafe",
        kind: HallKind::Cafe,
    },
    DiningHall {
        name: "Perk Coffee Bar",
        short_names: &["perk"],
        location_num: "22",
        location_name: "Perk+Coffee+Bar",
        kind: HallKind::Cafe,
    },
];

const SHORTMENU_BASE: &str = "https://nutrition.sa.ucsc.edu/shortmenu.aspx";
const LONGMENU_BASE: &str = "https://nutrition.sa.ucsc.edu/longmenu.aspx";
const USER_AGENT: &str = "Mozilla/5.0 (compatible; SlugMCP/0.1)";

pub fn find_hall(query: &str) -> Option<&'static DiningHall> {
    let q_matcher = FuzzyMatcher::new([query]).case_insensitive();
    DINING_HALLS.iter().find(|hall| {
        let labels: Vec<&str> = std::iter::once(hall.name)
            .chain(hall.short_names.iter().copied())
            .collect();
        // Forward: the user query contains one of this hall's labels (e.g.
        // "Cowell Stevenson Dining" contains "cowell").
        let label_matcher = FuzzyMatcher::new(labels.iter().copied()).case_insensitive();
        if label_matcher.matches(query) {
            return true;
        }
        // Reverse: a label contains the user query (e.g. query "rc" inside
        // short_name "rco", or query "cow" inside "cowell").
        labels.iter().any(|label| q_matcher.matches(label))
    })
}

static HALL_NAMES: LazyLock<String> = LazyLock::new(|| {
    DINING_HALLS
        .iter()
        .map(|h| h.name)
        .collect::<Vec<_>>()
        .join(", ")
});

pub fn hall_names() -> &'static str {
    &HALL_NAMES
}

/// Build menu URL. Accepts date in `M/D/YYYY` format (the nutrition site's native format).
/// The caller (DiningService) is responsible for converting ISO dates before calling this.
fn menu_url(base: &str, hall: &DiningHall, date: Option<&str>, meal_name: Option<&str>) -> String {
    let mut url = format!(
        "{}?sName=UC+Santa+Cruz+Dining&locationNum={}&locationName={}&naFlag=1\
         &WeeksMenus=UCSC+-+This+Week%27s+Menus&myaction=read",
        base, hall.location_num, hall.location_name
    );
    if let Some(d) = date {
        url.push_str("&dtdate=");
        url.push_str(&urlencoding::encode(d));
    }
    if let Some(m) = meal_name {
        url.push_str("&mealName=");
        url.push_str(&urlencoding::encode(m));
    }
    url
}

// --- Menu data types ---

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MenuItem {
    pub name: String,
    pub dietary_tags: Vec<String>,
    pub recipe_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Meal {
    pub name: String,
    pub categories: Vec<Category>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Category {
    pub name: String,
    pub items: Vec<MenuItem>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiningMenu {
    pub hall_name: String,
    pub date: Option<String>,
    pub meals: Vec<Meal>,
}

fn icon_to_tag(icon_filename: &str) -> Option<&'static str> {
    match icon_filename {
        "veggie" => Some("vegetarian"),
        "vegan" => Some("vegan"),
        "gluten" => Some("gluten_free"),
        "halal" => Some("halal"),
        "eggs" => Some("contains_eggs"),
        "milk" => Some("contains_dairy"),
        "nuts" => Some("contains_nuts"),
        "treenut" => Some("contains_tree_nuts"),
        "soy" => Some("contains_soy"),
        "wheat" => Some("contains_wheat"),
        "fish" => Some("contains_fish"),
        "shellfish" => Some("contains_shellfish"),
        "pork" => Some("contains_pork"),
        "beef" => Some("contains_beef"),
        "sesame" => Some("contains_sesame"),
        "alcohol" => Some("contains_alcohol"),
        _ => None,
    }
}

impl DiningMenu {
    pub fn format(&self) -> String {
        let mut out = match self.date.as_ref() {
            Some(d) => format!("## {} ({})\n\n", self.hall_name, d),
            None => format!("## {}\n\n", self.hall_name),
        };
        if self.meals.is_empty() {
            out.push_str("No menu items available.\n");
            return out;
        }
        for meal in &self.meals {
            let _ = writeln!(out, "### {}", meal.name);
            for cat in &meal.categories {
                let _ = writeln!(out, "**{}**", cat.name);
                for item in &cat.items {
                    let _ = write!(out, "- {}", item.name);
                    if !item.dietary_tags.is_empty() {
                        let _ = write!(out, " [{}]", item.dietary_tags.join(", "));
                    }
                    if let Some(id) = item.recipe_id.as_ref() {
                        let _ = write!(out, " (recipe: {})", id);
                    }
                    out.push('\n');
                }
                out.push('\n');
            }
        }
        out
    }
}

// --- Menu scraper ---

/// The nutrition site requires certain cookies to be present or it returns 500.
/// The cookies can be empty-valued; they just need to exist in the request.
const NUTRITION_COOKIES: &str =
    "WebInaCartLocation=; WebInaCartDates=; WebInaCartMeals=; WebInaCartRecipes=; WebInaCartQtys=";

/// Fetch a nutrition site page with required cookies and User-Agent.
async fn fetch_with_cookies(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = client
        .get(url)
        .header("Cookie", NUTRITION_COOKIES)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .context("Failed to fetch nutrition page")?;

    let status = resp.status();
    let html = resp
        .text()
        .await
        .context("Failed to read nutrition page body")?;

    if !status.is_success() || html.contains("Runtime Error") || html.contains("Server Error") {
        anyhow::bail!("Nutrition site returned error (status {})", status);
    }

    Ok(html)
}

pub async fn scrape_menu(
    client: &reqwest::Client,
    hall: &DiningHall,
    date: Option<&str>,
) -> Result<DiningMenu> {
    // Primary: fetch shortmenu (all meals on one page, no recipe IDs)
    let short_url = menu_url(SHORTMENU_BASE, hall, date, None);
    let html = fetch_with_cookies(client, &short_url).await?;
    let mut menu = parse_shortmenu(&html, hall.name);
    menu.date = date.map(|s| s.to_string());

    // Best-effort: enrich with recipe IDs from longmenu (one page per meal).
    // Fetch all meal pages concurrently so a cold menu isn't N serialized GETs.
    let meal_names: Vec<String> = menu.meals.iter().map(|m| m.name.clone()).collect();
    let long_fetches = meal_names.iter().map(|meal_name| {
        let long_url = menu_url(LONGMENU_BASE, hall, date, Some(meal_name));
        async move { (meal_name, fetch_with_cookies(client, &long_url).await) }
    });
    let long_results = futures_util::future::join_all(long_fetches).await;
    for (meal_name, result) in long_results {
        match result {
            Ok(long_html) => {
                let long_menu = parse_longmenu(&long_html, hall.name);
                enrich_recipe_ids(&mut menu, &long_menu);
            }
            Err(e) => {
                tracing::warn!(
                    "Longmenu fallback failed for {} ({}): {}",
                    hall.name,
                    meal_name,
                    e
                );
            }
        }
    }

    Ok(menu)
}

fn parse_shortmenu(html: &str, hall_name: &str) -> DiningMenu {
    let document = Html::parse_document(html);

    let mut meals: Vec<Meal> = Vec::new();
    let mut current_meal: Option<Meal> = None;
    let mut current_cat: Option<Category> = None;

    for element in document.select(&SEL_SHORT_MENU) {
        let classes: Vec<&str> = element.value().classes().collect();

        if classes.contains(&"shortmenumeals") {
            // Flush previous state
            if let Some(cat) = current_cat.take()
                && let Some(meal) = current_meal.as_mut()
                && !cat.items.is_empty()
            {
                meal.categories.push(cat);
            }
            if let Some(meal) = current_meal.take()
                && !meal.categories.is_empty()
            {
                meals.push(meal);
            }

            let meal_name = element.text().collect::<String>().trim().to_string();
            current_meal = Some(Meal {
                name: meal_name,
                categories: Vec::new(),
            });
        } else if classes.contains(&"shortmenucats") {
            if let Some(cat) = current_cat.take()
                && let Some(meal) = current_meal.as_mut()
                && !cat.items.is_empty()
            {
                meal.categories.push(cat);
            }

            let name = element.text().collect::<String>();
            let name = name
                .trim()
                .trim_start_matches("--")
                .trim_end_matches("--")
                .trim()
                .to_string();

            if !name.is_empty() {
                current_cat = Some(Category {
                    name,
                    items: Vec::new(),
                });
            }
        } else if classes.contains(&"shortmenurecipes")
            && let Some(cat) = current_cat.as_mut()
        {
            let item_name = element
                .text()
                .collect::<String>()
                .trim()
                .trim_end_matches('\u{a0}') // &nbsp;
                .trim()
                .to_string();

            // Dietary icons are in sibling <td> elements in the parent <tr>
            let mut dietary_tags = Vec::new();
            // Walk up: div.shortmenurecipes → td → tr (inner table) → td → tr (outer)
            if let Some(parent_td) = element.parent()
                && let Some(parent_tr) = parent_td.parent()
            {
                // Check the outer row's sibling tds for images
                if let Some(outer_td) = parent_tr.parent()
                    && let Some(outer_tr) = outer_td.parent()
                    && let Some(outer_el) = scraper::ElementRef::wrap(outer_tr)
                {
                    for td in outer_el.select(&SEL_TD) {
                        for img in td.select(&SEL_IMG) {
                            if let Some(src) = img.value().attr("src")
                                && let Some(icon_name) = src
                                    .strip_prefix("LegendImages/")
                                    .and_then(|s| s.strip_suffix(".gif"))
                                && let Some(tag) = icon_to_tag(icon_name)
                            {
                                dietary_tags.push(tag.to_string());
                            }
                        }
                    }
                }
            }

            if !item_name.is_empty() {
                cat.items.push(MenuItem {
                    name: item_name,
                    dietary_tags,
                    recipe_id: None,
                });
            }
        }
    }

    // Flush remaining
    if let Some(cat) = current_cat
        && let Some(meal) = current_meal.as_mut()
        && !cat.items.is_empty()
    {
        meal.categories.push(cat);
    }
    if let Some(meal) = current_meal
        && !meal.categories.is_empty()
    {
        meals.push(meal);
    }

    DiningMenu {
        hall_name: hall_name.to_string(),
        date: None,
        meals,
    }
}

/// Copy recipe IDs from longmenu items into shortmenu items by matching item names.
fn enrich_recipe_ids(menu: &mut DiningMenu, long_menu: &DiningMenu) {
    // Build a map of item name → recipe_id from longmenu
    let mut recipe_map = std::collections::HashMap::new();
    for meal in &long_menu.meals {
        for cat in &meal.categories {
            for item in &cat.items {
                if let Some(ref rid) = item.recipe_id {
                    recipe_map.insert(item.name.to_lowercase(), rid.clone());
                }
            }
        }
    }

    // Apply to shortmenu items
    for meal in &mut menu.meals {
        for cat in &mut meal.categories {
            for item in &mut cat.items {
                if item.recipe_id.is_none()
                    && let Some(rid) = recipe_map.get(&item.name.to_lowercase())
                {
                    item.recipe_id = Some(rid.clone());
                }
            }
        }
    }
}

fn parse_longmenu(html: &str, hall_name: &str) -> DiningMenu {
    let document = Html::parse_document(html);

    let mut meals: Vec<Meal> = Vec::new();
    let mut current_meal: Option<Meal> = None;
    let mut current_cat: Option<Category> = None;

    for element in document.select(&SEL_LONG_MENU) {
        let classes: Vec<&str> = element.value().classes().collect();

        if classes.contains(&"longmenugridheader") {
            // Finish previous category and meal
            if let Some(cat) = current_cat.take()
                && let Some(meal) = current_meal.as_mut()
                && !cat.items.is_empty()
            {
                meal.categories.push(cat);
            }
            if let Some(meal) = current_meal.take()
                && !meal.categories.is_empty()
            {
                meals.push(meal);
            }

            // Extract meal name from <a name="MealName">
            let meal_name = element
                .select(&SEL_ANCHOR_NAME)
                .next()
                .and_then(|a| a.value().attr("name"))
                .unwrap_or("Unknown")
                .to_string();

            current_meal = Some(Meal {
                name: meal_name,
                categories: Vec::new(),
            });
        } else if classes.contains(&"longmenucolmenucat") {
            // Finish previous category
            if let Some(cat) = current_cat.take()
                && let Some(meal) = current_meal.as_mut()
                && !cat.items.is_empty()
            {
                meal.categories.push(cat);
            }

            let name = element.text().collect::<String>();
            let name = name
                .trim()
                .trim_start_matches("--")
                .trim_end_matches("--")
                .trim()
                .to_string();

            if !name.is_empty() {
                current_cat = Some(Category {
                    name,
                    items: Vec::new(),
                });
            }
        } else if classes.contains(&"longmenucoldispname")
            && let Some(cat) = current_cat.as_mut()
        {
            // Extract item name and recipe ID from <a> link
            let (item_name, recipe_id) = if let Some(a) = element.select(&SEL_LINK).next() {
                let name = a.text().collect::<String>().trim().to_string();
                let href = a.value().attr("href").unwrap_or("");
                let rid = href
                    .split("RecNumAndPort=")
                    .nth(1)
                    .map(|s| s.split('&').next().unwrap_or(s).to_string());
                (name, rid)
            } else {
                (element.text().collect::<String>().trim().to_string(), None)
            };

            // Extract dietary icons from sibling <td> elements
            let mut dietary_tags = Vec::new();
            if let Some(parent_tr) = element
                    .parent() // td
                    .and_then(|n| n.parent()) // tr (inner)
                    && let Some(parent_el) = scraper::ElementRef::wrap(parent_tr)
            {
                for td in parent_el.select(&SEL_TD) {
                    for img in td.select(&SEL_IMG) {
                        if let Some(src) = img.value().attr("src")
                            && let Some(icon_name) = src
                                .strip_prefix("LegendImages/")
                                .and_then(|s| s.strip_suffix(".gif"))
                            && let Some(tag) = icon_to_tag(icon_name)
                        {
                            dietary_tags.push(tag.to_string());
                        }
                    }
                }
            }

            if !item_name.is_empty() {
                cat.items.push(MenuItem {
                    name: item_name,
                    dietary_tags,
                    recipe_id,
                });
            }
        }
    }

    // Flush remaining category and meal
    if let Some(cat) = current_cat
        && let Some(meal) = current_meal.as_mut()
        && !cat.items.is_empty()
    {
        meal.categories.push(cat);
    }
    if let Some(meal) = current_meal
        && !meal.categories.is_empty()
    {
        meals.push(meal);
    }

    DiningMenu {
        hall_name: hall_name.to_string(),
        date: None,
        meals,
    }
}

// --- Meal balance ---

#[cfg(feature = "auth")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BalanceAccount {
    pub name: String,
    /// Raw balance string as shown (e.g. "$5.50", "0", "3 meals").
    pub balance: String,
}

#[cfg(feature = "auth")]
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MealBalance {
    /// Optional meal-plan label (e.g. "STAGING", "Carson 19/wk").
    pub plan: Option<String>,
    /// Every tender row from the GET funds overview, in display order.
    pub accounts: Vec<BalanceAccount>,
}

#[cfg(feature = "auth")]
impl MealBalance {
    pub fn format(&self) -> String {
        let mut out = String::from("# Meal Plan Balance\n\n");
        if let Some(plan) = &self.plan {
            let _ = writeln!(out, "_Current plan: {}_\n", plan);
        }
        if self.accounts.is_empty() {
            out.push_str(
                "Could not retrieve balance information. The balance page may have changed.\n",
            );
            return out;
        }
        for acct in &self.accounts {
            let _ = writeln!(out, "- **{}**: {}", acct.name, acct.balance);
        }
        out
    }
}

#[cfg(feature = "auth")]
/// Result of a balance scrape attempt.
pub struct BalanceResult {
    pub balance: MealBalance,
    /// If parsing failed, contains a snippet of what the page actually showed.
    pub debug_snippet: Option<String>,
}

#[cfg(feature = "auth")]
const FUNDS_HOME_URL: &str = "https://get.cbord.com/ucsc/full/funds_home.php";
#[cfg(feature = "auth")]
const FUNDS_OVERVIEW_PARTIAL_URL: &str =
    "https://get.cbord.com/ucsc/full/funds_overview_partial.php";

#[cfg(feature = "auth")]
pub async fn scrape_balance(client: &reqwest::Client) -> Result<BalanceResult> {
    // Balances live in the GET system at get.cbord.com/ucsc and require an
    // authenticated session (IdP cookies from browser SSO auto-approve the
    // SAML chain). funds_home.php only renders the *chrome* — the balance
    // table is fetched by a second AJAX POST to funds_overview_partial.php
    // with a per-user `userId` (UUID) and a per-session `formToken`, both
    // embedded in the funds_home page JS. reqwest doesn't run that JS, so we
    // replicate the POST ourselves.
    let home = crate::auth::saml_aware_get(client, FUNDS_HOME_URL)
        .await
        .context("failed to fetch funds home page")?;

    if !home.status.is_success() {
        anyhow::bail!("Funds home page returned status {}", home.status);
    }

    // If we got bounced to the IdP, the session is no longer valid.
    if home.final_url.host_str() == Some("login.ucsc.edu") {
        return Ok(BalanceResult {
            balance: MealBalance::default(),
            debug_snippet: Some(
                "Session expired — funds page redirected to SSO login. Run `login` again.".into(),
            ),
        });
    }

    let Some((user_id, form_token)) = extract_overview_tokens(&home.body) else {
        let snippet = crate::util::truncate(&clean_visible_text(&home.body), 1000);
        return Ok(BalanceResult {
            balance: MealBalance::default(),
            debug_snippet: Some(format!(
                "Could not find userId/formToken on funds page (markup may have changed). Page text: {snippet}"
            )),
        });
    };

    let partial = client
        .post(FUNDS_OVERVIEW_PARTIAL_URL)
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Referer", FUNDS_HOME_URL)
        .form(&[
            ("userId", user_id.as_str()),
            ("formToken", form_token.as_str()),
        ])
        .send()
        .await
        .context("failed to fetch funds overview partial")?;

    if !partial.status().is_success() {
        anyhow::bail!(
            "Funds overview partial returned status {}",
            partial.status()
        );
    }
    let body = partial
        .text()
        .await
        .context("reading funds overview partial")?;

    let balance = parse_balance_table(&body);
    if balance.accounts.is_empty() {
        let snippet = crate::util::truncate(&clean_visible_text(&body), 1000);
        return Ok(BalanceResult {
            balance,
            debug_snippet: Some(format!(
                "Funds overview returned no parseable accounts. Fragment: {snippet}"
            )),
        });
    }

    Ok(BalanceResult {
        balance,
        debug_snippet: None,
    })
}

#[cfg(feature = "auth")]
/// Pull the `userId` UUID and `formToken` the page JS passes to
/// `getOverview(...)` / the `$.post` to funds_overview_partial.php.
fn extract_overview_tokens(html: &str) -> Option<(String, String)> {
    use std::sync::LazyLock;
    static UID_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"getOverview\("([0-9a-fA-F-]{8,})"\)"#).unwrap());
    static TOKEN_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#""formToken"\s*:\s*"([^"]+)""#).unwrap());

    let uid = UID_RE.captures(html)?.get(1)?.as_str().to_string();
    let token = TOKEN_RE.captures(html)?.get(1)?.as_str().to_string();
    Some((uid, token))
}

#[cfg(feature = "auth")]
/// Parse the funds overview fragment: a table of `account_name` / `balance`
/// cells, optionally preceded by a "Current Plan: <strong>NAME</strong>" line.
fn parse_balance_table(html: &str) -> MealBalance {
    use std::sync::LazyLock;
    static ROW_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("tbody tr").expect("hardcoded selector"));
    static NAME_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("td.account_name").expect("hardcoded selector"));
    static BAL_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("td.balance").expect("hardcoded selector"));
    static PLAN_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse(".pd-1 strong, .pd-1").expect("hardcoded selector"));

    let document = Html::parse_fragment(html);

    let plan = document.select(&PLAN_SEL).next().and_then(|el| {
        let t = el.text().collect::<String>();
        let t = t.trim_start_matches("Current Plan:").trim();
        (!t.is_empty()).then(|| t.to_string())
    });

    let mut accounts = Vec::new();
    for row in document.select(&ROW_SEL) {
        let name = row
            .select(&NAME_SEL)
            .next()
            .map(|c| c.text().collect::<String>().trim().to_string());
        let balance = row
            .select(&BAL_SEL)
            .next()
            .map(|c| c.text().collect::<String>().trim().to_string());
        if let (Some(name), Some(balance)) = (name, balance)
            && !name.is_empty()
        {
            accounts.push(BalanceAccount { name, balance });
        }
    }

    MealBalance { plan, accounts }
}

#[cfg(feature = "auth")]
/// Collapse an HTML fragment to whitespace-normalized visible text (debug only).
fn clean_visible_text(html: &str) -> String {
    let text = Html::parse_fragment(html)
        .root_element()
        .text()
        .collect::<String>();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- Nutrition label scraper ---

#[derive(Debug)]
pub struct NutritionInfo {
    pub item_name: String,
    pub serving_size: String,
    pub calories: Option<String>,
    pub total_fat: Option<String>,
    pub saturated_fat: Option<String>,
    pub trans_fat: Option<String>,
    pub cholesterol: Option<String>,
    pub sodium: Option<String>,
    pub total_carbs: Option<String>,
    pub dietary_fiber: Option<String>,
    pub sugars: Option<String>,
    pub protein: Option<String>,
    pub ingredients: String,
    pub allergens: String,
}

const LABEL_URL: &str = "https://nutrition.sa.ucsc.edu/label.aspx";

pub async fn scrape_nutrition(client: &reqwest::Client, recipe_id: &str) -> Result<NutritionInfo> {
    let url = format!(
        "{}?RecNumAndPort={}",
        LABEL_URL,
        urlencoding::encode(recipe_id)
    );
    let html = fetch_with_cookies(client, &url).await?;
    Ok(parse_nutrition_label(&html))
}

fn parse_nutrition_label(html: &str) -> NutritionInfo {
    let document = Html::parse_document(html);

    let item_name = document
        .select(&SEL_RECIPE)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    let ingredients = document
        .select(&SEL_INGREDIENTS)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    let allergens = document
        .select(&SEL_ALLERGENS)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    let text = document.root_element().text().collect::<String>();

    NutritionInfo {
        item_name,
        serving_size: extract_after(&text, "Serving Size").unwrap_or_default(),
        calories: extract_after(&text, "Calories"),
        total_fat: extract_after(&text, "Total Fat"),
        saturated_fat: extract_after(&text, "Sat. Fat"),
        trans_fat: extract_after(&text, "Trans Fat"),
        cholesterol: extract_after(&text, "Cholesterol"),
        sodium: extract_after(&text, "Sodium"),
        total_carbs: extract_after(&text, "Tot. Carb."),
        dietary_fiber: extract_after(&text, "Dietary Fiber"),
        sugars: extract_after(&text, "Sugars"),
        protein: extract_after(&text, "Protein"),
        ingredients,
        allergens,
    }
}

fn extract_after(text: &str, label: &str) -> Option<String> {
    let idx = text.find(label)?;
    let after = text[idx + label.len()..].trim_start();
    let value: String = after
        .chars()
        .take_while(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn write_nutrient(out: &mut String, label: &str, value: &Option<String>) {
    match value {
        Some(v) => {
            let _ = writeln!(out, "| {} | {} |", label, v);
        }
        None => {
            let _ = writeln!(out, "| {} | N/A |", label);
        }
    }
}

impl NutritionInfo {
    pub fn format(&self) -> String {
        let mut out = format!("# {}\n\n", self.item_name);
        let _ = writeln!(out, "**Serving Size:** {}", self.serving_size);
        match self.calories.as_ref() {
            Some(c) => {
                let _ = writeln!(out, "**Calories:** {}\n", c);
            }
            None => out.push_str("**Calories:** N/A\n\n"),
        }
        out.push_str("| Nutrient | Amount |\n");
        out.push_str("|----------|--------|\n");
        write_nutrient(&mut out, "Total Fat", &self.total_fat);
        write_nutrient(&mut out, "Saturated Fat", &self.saturated_fat);
        write_nutrient(&mut out, "Trans Fat", &self.trans_fat);
        write_nutrient(&mut out, "Cholesterol", &self.cholesterol);
        write_nutrient(&mut out, "Sodium", &self.sodium);
        write_nutrient(&mut out, "Total Carbs", &self.total_carbs);
        write_nutrient(&mut out, "Dietary Fiber", &self.dietary_fiber);
        write_nutrient(&mut out, "Sugars", &self.sugars);
        write_nutrient(&mut out, "Protein", &self.protein);
        let _ = writeln!(out, "\n**Ingredients:** {}\n", self.ingredients);
        let _ = writeln!(out, "**Allergens:** {}", self.allergens);
        out
    }
}

// --- Hours scraper ---

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiningLocation {
    pub name: String,
    pub category: String,
    pub regular_hours: Vec<String>,
    pub date_hours: Vec<DateHours>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DateHours {
    pub date: String,
    pub opens: Option<String>,
    pub closes: Option<String>,
}

const HOURS_URL: &str = "https://dining.ucsc.edu/locations-hours/";

pub async fn scrape_hours(client: &reqwest::Client) -> Result<Vec<DiningLocation>> {
    let resp = client
        .get(HOURS_URL)
        .send()
        .await
        .context("Failed to fetch dining hours page")?;

    let html = resp.text().await.context("Failed to read hours page")?;
    Ok(parse_hours(&html))
}

fn parse_hours(html: &str) -> Vec<DiningLocation> {
    // Strip <details>/<summary> wrappers that cause html5ever foster-parenting
    // issues — the schema.org divs inside them get misplaced in the DOM tree.
    let cleaned = html
        .replace("<details", "<div")
        .replace("</details>", "</div>")
        .replace("<summary", "<span")
        .replace("</summary>", "</span>");
    let document = Html::parse_document(&cleaned);
    let mut locations = Vec::new();

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for schema_div in document.select(&SEL_SCHEMA) {
        let itemtype = schema_div.value().attr("itemtype").unwrap_or("");
        if !itemtype.contains("schema.org/Restaurant")
            && !itemtype.contains("schema.org/FoodEstablishment")
            && !itemtype.contains("schema.org/LocalBusiness")
        {
            continue;
        }

        let name = match schema_div.select(&SEL_NAME).next() {
            Some(el) => el.value().attr("content").unwrap_or("").to_string(),
            None => continue,
        };

        if name.is_empty() || seen.contains(&name) {
            continue;
        }
        seen.insert(name.clone());

        let category = if itemtype.contains("LocalBusiness") {
            "Market"
        } else {
            "Dining"
        }
        .to_string();

        let regular_hours: Vec<String> = schema_div
            .select(&SEL_HOURS)
            .filter_map(|el| el.value().attr("content").map(|s| s.to_string()))
            .collect();

        let mut date_hours = Vec::new();
        for spec in schema_div.select(&SEL_SPEC) {
            let times: Vec<_> = spec.select(&SEL_TIME).collect();
            if times.is_empty() {
                continue;
            }

            let date = times[0].value().attr("datetime").unwrap_or("").to_string();

            let (opens, closes) = if times.len() >= 3 {
                (
                    times[1].value().attr("datetime").map(|s| s.to_string()),
                    times[2].value().attr("datetime").map(|s| s.to_string()),
                )
            } else {
                (None, None)
            };

            if !date.is_empty() {
                date_hours.push(DateHours {
                    date,
                    opens,
                    closes,
                });
            }
        }

        locations.push(DiningLocation {
            name,
            category,
            regular_hours,
            date_hours,
        });
    }

    locations
}

impl DiningLocation {
    /// Format with a reference date for filtering upcoming special hours.
    pub fn format_with_date(&self, today: &str) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "### {} ({})", self.name, self.category);
        if self.regular_hours.is_empty() {
            out.push_str("Hours not available\n");
        } else {
            out.push_str("**Regular Hours:**\n");
            for h in &self.regular_hours {
                let _ = writeln!(out, "- {}", h);
            }
        }

        let upcoming: Vec<_> = self
            .date_hours
            .iter()
            .filter(|d| d.date.as_str() >= today)
            .take(5)
            .collect();
        if !upcoming.is_empty() {
            out.push_str("\n**Upcoming Special Hours:**\n");
            for dh in upcoming {
                match (&dh.opens, &dh.closes) {
                    (Some(o), Some(c)) => {
                        let _ = writeln!(out, "- {}: {} - {}", dh.date, o, c);
                    }
                    _ => {
                        let _ = writeln!(out, "- {}: CLOSED", dh.date);
                    }
                }
            }
        }
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "auth")]
    const FUNDS_OVERVIEW_FIXTURE: &str = include_str!("fixtures/funds_overview_partial.html");

    #[cfg(feature = "auth")]
    #[test]
    fn test_parse_balance_table() {
        let bal = parse_balance_table(FUNDS_OVERVIEW_FIXTURE);
        assert_eq!(bal.plan.as_deref(), Some("Carson 19/wk"));
        assert_eq!(bal.accounts.len(), 5);
        let by_name = |n: &str| {
            bal.accounts
                .iter()
                .find(|a| a.name == n)
                .map(|a| a.balance.as_str())
        };
        assert_eq!(by_name("Banana Bucks"), Some("$42.75"));
        assert_eq!(by_name("Slug Points"), Some("$310.50"));
        assert_eq!(by_name("Flexi Dollars"), Some("$125.00"));
        assert_eq!(by_name("Board"), Some("14"));
        assert_eq!(by_name("Donated Meal"), Some("0"));
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_balance_format_renders_accounts() {
        let bal = parse_balance_table(FUNDS_OVERVIEW_FIXTURE);
        let out = bal.format();
        assert!(out.contains("Current plan: Carson 19/wk"));
        assert!(out.contains("**Banana Bucks**: $42.75"));
        assert!(out.contains("**Slug Points**: $310.50"));
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_balance_empty_table_reports_failure() {
        let bal = parse_balance_table("<div>no table here</div>");
        assert!(bal.accounts.is_empty());
        assert!(bal.format().contains("Could not retrieve balance"));
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_extract_overview_tokens() {
        let html = r#"<script>
            getOverview("07176fd1-e4d6-4724-a4f7-10fc0ea1d808");
            $.post("funds_overview_partial.php", {"userId": id, "formToken": "6a29a1d09cdd26.64464417"},
        </script>"#;
        let (uid, token) = extract_overview_tokens(html).unwrap();
        assert_eq!(uid, "07176fd1-e4d6-4724-a4f7-10fc0ea1d808");
        assert_eq!(token, "6a29a1d09cdd26.64464417");
        assert!(extract_overview_tokens("<script>no tokens</script>").is_none());
    }

    const MOCK_LONGMENU_HTML: &str = r#"<html><body>
    <table><tr>
    <td class="longmenugridheader" width="71%" colspan="3">
      <a name="Breakfast"><div class="longmenugridheader">&nbsp;Menu for Monday</div></a>
    </td>
    </tr>
    <tr><td><div class='longmenucolmenucat'>-- Entrees --</div></td></tr>
    <tr>
      <td>
        <table border="0" width="100%" cellpadding="0"><tr><td>
          <table border="0" cellpadding="0" cellspacing="0" width="100%"><tr>
            <td><div class='longmenucoldispname'>
              <a href='label.aspx?locationNum=40&amp;RecNumAndPort=061002*3'>Scrambled Eggs</a>
            </div></td>
            <td width="10%"><img src="LegendImages/veggie.gif" alt="" width="25" height="25"></td>
            <td width="10%"><img src="LegendImages/eggs.gif" alt="" width="25" height="25"></td>
          </tr></table>
        </td></tr></table>
      </td>
    </tr>
    <tr><td><div class='longmenucolmenucat'>-- Bakery --</div></td></tr>
    <tr>
      <td>
        <table border="0" width="100%" cellpadding="0"><tr><td>
          <table border="0" cellpadding="0" cellspacing="0" width="100%"><tr>
            <td><div class='longmenucoldispname'>
              <a href='label.aspx?locationNum=40&amp;RecNumAndPort=217044*1'>Waffle</a>
            </div></td>
            <td width="10%"><img src="LegendImages/vegan.gif" alt="" width="25" height="25"></td>
          </tr></table>
        </td></tr></table>
      </td>
    </tr>
    </table>
    </body></html>"#;

    #[test]
    fn test_parse_longmenu() {
        let menu = parse_longmenu(MOCK_LONGMENU_HTML, "Test Hall");
        assert_eq!(menu.hall_name, "Test Hall");
        assert_eq!(menu.meals.len(), 1);

        let meal = &menu.meals[0];
        assert_eq!(meal.name, "Breakfast");
        assert_eq!(meal.categories.len(), 2);

        let entrees = &meal.categories[0];
        assert_eq!(entrees.name, "Entrees");
        assert_eq!(entrees.items.len(), 1);
        assert_eq!(entrees.items[0].name, "Scrambled Eggs");
        assert_eq!(entrees.items[0].recipe_id, Some("061002*3".to_string()));
        assert!(
            entrees.items[0]
                .dietary_tags
                .contains(&"vegetarian".to_string())
        );
        assert!(
            entrees.items[0]
                .dietary_tags
                .contains(&"contains_eggs".to_string())
        );

        let bakery = &meal.categories[1];
        assert_eq!(bakery.items[0].name, "Waffle");
        assert!(bakery.items[0].dietary_tags.contains(&"vegan".to_string()));
    }

    const MOCK_SHORTMENU_HTML: &str = r#"<html><body>
    <table>
      <tr><td>
        <div class="shortmenumeals">Breakfast</div>
      </td></tr>
      <tr><td colspan="4">
        <div class="shortmenucats"><span style="color: #000000">-- Entrees --</span></div>
      </td></tr>
      <tr>
        <td><table><tr>
          <td><div class='shortmenurecipes'><span style='color: #585858'>Scrambled Eggs&nbsp;</span></div></td>
          <td width="10%"><img src="LegendImages/veggie.gif" alt="" width="25px" height="25px"></td>
          <td width="10%"><img src="LegendImages/eggs.gif" alt="" width="25px" height="25px"></td>
        </tr></table></td>
      </tr>
      <tr><td colspan="4">
        <div class="shortmenucats"><span style="color: #000000">-- Bakery --</span></div>
      </td></tr>
      <tr>
        <td><table><tr>
          <td><div class='shortmenurecipes'><span style='color: #585858'>Waffle&nbsp;</span></div></td>
          <td width="10%"><img src="LegendImages/vegan.gif" alt="" width="25px" height="25px"></td>
        </tr></table></td>
      </tr>
    </table>
    </body></html>"#;

    #[test]
    fn test_parse_shortmenu() {
        let menu = parse_shortmenu(MOCK_SHORTMENU_HTML, "Test Hall");
        assert_eq!(menu.hall_name, "Test Hall");
        assert_eq!(menu.meals.len(), 1);

        let meal = &menu.meals[0];
        assert_eq!(meal.name, "Breakfast");
        assert_eq!(meal.categories.len(), 2);

        let entrees = &meal.categories[0];
        assert_eq!(entrees.name, "Entrees");
        assert_eq!(entrees.items.len(), 1);
        assert_eq!(entrees.items[0].name, "Scrambled Eggs");
        assert_eq!(entrees.items[0].recipe_id, None);
        assert!(
            entrees.items[0]
                .dietary_tags
                .contains(&"vegetarian".to_string()),
            "Expected vegetarian tag, got: {:?}",
            entrees.items[0].dietary_tags
        );

        let bakery = &meal.categories[1];
        assert_eq!(bakery.items[0].name, "Waffle");
        assert!(
            bakery.items[0].dietary_tags.contains(&"vegan".to_string()),
            "Expected vegan tag, got: {:?}",
            bakery.items[0].dietary_tags
        );
    }

    #[test]
    fn test_enrich_recipe_ids() {
        let mut short_menu = parse_shortmenu(MOCK_SHORTMENU_HTML, "Test");
        let long_menu = parse_longmenu(MOCK_LONGMENU_HTML, "Test");
        enrich_recipe_ids(&mut short_menu, &long_menu);

        let eggs = &short_menu.meals[0].categories[0].items[0];
        assert_eq!(eggs.name, "Scrambled Eggs");
        assert_eq!(eggs.recipe_id, Some("061002*3".to_string()));

        let waffle = &short_menu.meals[0].categories[1].items[0];
        assert_eq!(waffle.name, "Waffle");
        assert_eq!(waffle.recipe_id, Some("217044*1".to_string()));
    }

    const MOCK_LABEL_HTML: &str = r#"<html><body>
    <div class="labelrecipe">Belgian Waffle Squares</div>
    <table>
      <tr><td>
        <font size="5" face="arial">Serving Size&nbsp;</font><font size="5" face="arial">1 ea</font>
        <font size="5" face="arial"><b>Calories&nbsp;180</b></font>
      </td></tr>
      <tr>
        <td><font size="4" face="arial"><b>Total Fat&nbsp;</b></font><font size="4" face="arial">6g</font></td>
        <td><font size="4" face="arial"><b>Tot. Carb.&nbsp;</b></font><font size="4" face="arial">27g</font></td>
      </tr>
      <tr>
        <td><font size="4" face="arial">&nbsp;&nbsp;Sat. Fat&nbsp;</font><font size="4" face="arial">1g</font></td>
        <td><font size="4" face="arial">&nbsp;&nbsp;Dietary Fiber&nbsp;</font><font size="4" face="arial">1g</font></td>
      </tr>
      <tr>
        <td><font size="4" face="arial">&nbsp;&nbsp;Trans Fat&nbsp;</font><font size="4" face="arial">0g</font></td>
        <td><font size="4" face="arial">&nbsp;&nbsp;Sugars&nbsp;</font><font size="4" face="arial">6g</font></td>
      </tr>
      <tr>
        <td><font size="4" face="arial"><b>Cholesterol&nbsp;</b></font><font size="4" face="arial">30mg</font></td>
        <td><font size="4" face="arial"><b>Protein&nbsp;</b></font><font size="4" face="arial">3g</font></td>
      </tr>
      <tr>
        <td><font size="4" face="arial"><b>Sodium&nbsp;</b></font><font size="4" face="arial">370mg</font></td>
      </tr>
    </table>
    <span class="labelingredientscaption">INGREDIENTS:&nbsp;&nbsp;</span>
    <span class="labelingredientsvalue">Enriched Wheat Flour, Sugar, Eggs</span>
    <span class="labelallergenscaption">ALLERGENS:&nbsp;&nbsp;</span>
    <span class="labelallergensvalue">Milk, Egg, Wheat, Soy</span>
    </body></html>"#;

    #[test]
    fn test_parse_nutrition_label() {
        let info = parse_nutrition_label(MOCK_LABEL_HTML);
        assert_eq!(info.item_name, "Belgian Waffle Squares");
        assert_eq!(info.ingredients, "Enriched Wheat Flour, Sugar, Eggs");
        assert_eq!(info.allergens, "Milk, Egg, Wheat, Soy");
        assert!(info.calories.as_ref().unwrap().contains("180"));
    }

    const MOCK_HOURS_HTML: &str = r#"<html><body>
    <div itemtype="http://schema.org/Restaurant" itemscope>
      <meta itemprop="name" content="Crown/Merrill Dining Hall">
      <meta itemprop="openingHours" content="Mo-Fr 7:00-20:00">
      <div itemprop="openingHoursSpecification" itemscope itemtype="http://schema.org/OpeningHoursSpecification">
        <time itemprop="validFrom validThrough" datetime="2026-03-20"></time>
        <time itemprop="opens" datetime="07:00:00"></time>
        <time itemprop="closes" datetime="20:00:00"></time>
      </div>
      <div itemprop="openingHoursSpecification" itemscope itemtype="http://schema.org/OpeningHoursSpecification">
        <time itemprop="validFrom validThrough" datetime="2026-03-21"></time>
      </div>
    </div>
    <div itemtype="http://schema.org/LocalBusiness" itemscope>
      <meta itemprop="name" content="Merrill Market">
      <meta itemprop="openingHours" content="Mo-Fr 9:00-20:00">
    </div>
    </body></html>"#;

    #[test]
    fn test_parse_hours() {
        let locations = parse_hours(MOCK_HOURS_HTML);
        assert_eq!(locations.len(), 2);

        let crown = &locations[0];
        assert_eq!(crown.name, "Crown/Merrill Dining Hall");
        assert_eq!(crown.category, "Dining");
        assert_eq!(crown.regular_hours, vec!["Mo-Fr 7:00-20:00"]);
        assert_eq!(crown.date_hours.len(), 2);
        assert_eq!(crown.date_hours[0].date, "2026-03-20");
        assert_eq!(crown.date_hours[0].opens, Some("07:00:00".to_string()));
        assert_eq!(crown.date_hours[1].date, "2026-03-21");
        assert_eq!(crown.date_hours[1].opens, None);

        let market = &locations[1];
        assert_eq!(market.name, "Merrill Market");
        assert_eq!(market.category, "Market");
    }

    #[test]
    fn test_format_hours_with_date() {
        let locations = parse_hours(MOCK_HOURS_HTML);
        let crown = &locations[0];

        // With a date before the special hours
        let output = crown.format_with_date("2026-03-19");
        assert!(output.contains("Crown/Merrill Dining Hall"));
        assert!(output.contains("Mo-Fr 7:00-20:00"));
        assert!(output.contains("2026-03-20"));
        assert!(output.contains("CLOSED")); // 2026-03-21 has no opens/closes

        // With a date after the special hours
        let output = crown.format_with_date("2026-03-22");
        assert!(!output.contains("Upcoming Special Hours"));
    }
}
