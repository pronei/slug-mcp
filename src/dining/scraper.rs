use std::fmt;
use std::fmt::Write;

use std::sync::OnceLock;

use anyhow::{Context, Result};
use scraper::Html;

use crate::util::{sel, selectors};

selectors! {
    SEL_ANCHOR => "a",
    SEL_LINK => "a[href]",
    SEL_IMG => "img",
    SEL_TD => "td",
    SEL_ALL_MENU => "div.shortmenumeals, div.shortmenucats, div.shortmenurecipes",
    SEL_RECIPE => "a[href*='recipe']",
    SEL_INGREDIENTS => "span.labelingredientsvalue",
    SEL_ALLERGENS => "span.labelallergensvalue",
    SEL_SHORT_MENU => "div.shortmenumeals",
    SEL_SCHEMA => "div[itemtype='http://schema.org/FoodEstablishment']",
    SEL_NAME => "meta[itemprop='name']",
    SEL_HOURS => "meta[itemprop='openingHours']",
    SEL_SPEC => "div[itemtype='http://schema.org/OpeningHoursSpecification']",
    SEL_TIME => "time",
}

// --- Dining hall definitions ---

pub struct DiningHall {
    pub name: &'static str,
    pub short_names: &'static [&'static str],
    pub location_num: &'static str,
    pub location_name: &'static str,
}

pub static DINING_HALLS: &[DiningHall] = &[
    DiningHall {
        name: "John R. Lewis & College Nine Dining Hall",
        short_names: &["lewis", "college nine", "c9", "nine"],
        location_num: "40",
        location_name: "John+R.+Lewis+%26+College+Nine+Dining+Hall",
    },
    DiningHall {
        name: "Cowell & Stevenson Dining Hall",
        short_names: &["cowell", "stevenson"],
        location_num: "05",
        location_name: "Cowell+%26+Stevenson+Dining+Hall",
    },
    DiningHall {
        name: "Crown & Merrill Dining Hall",
        short_names: &["crown", "merrill"],
        location_num: "20",
        location_name: "Crown+%26+Merrill+Dining+Hall",
    },
    DiningHall {
        name: "Porter & Kresge Dining Hall",
        short_names: &["porter", "kresge"],
        location_num: "25",
        location_name: "Porter+%26+Kresge+Dining+Hall",
    },
    DiningHall {
        name: "Rachel Carson & Oakes Dining Hall",
        short_names: &["carson", "oakes", "rco"],
        location_num: "30",
        location_name: "Rachel+Carson+%26+Oakes+Dining+Hall",
    },
    DiningHall {
        name: "Banana Joe's",
        short_names: &["banana joe", "banana"],
        location_num: "21",
        location_name: "Banana+Joe%27s",
    },
    DiningHall {
        name: "Oakes Cafe",
        short_names: &["oakes cafe"],
        location_num: "23",
        location_name: "Oakes+Cafe",
    },
    DiningHall {
        name: "Global Village Cafe",
        short_names: &["global village", "global"],
        location_num: "46",
        location_name: "Global+Village+Cafe",
    },
    DiningHall {
        name: "Owl's Nest Cafe",
        short_names: &["owl", "owls nest"],
        location_num: "24",
        location_name: "Owl%27s+Nest+Cafe",
    },
    DiningHall {
        name: "Perk Coffee Bar",
        short_names: &["perk"],
        location_num: "22",
        location_name: "Perk+Coffee+Bar",
    },
];

const SHORTMENU_BASE: &str = "https://nutrition.sa.ucsc.edu/shortmenu.aspx";
const LONGMENU_BASE: &str = "https://nutrition.sa.ucsc.edu/longmenu.aspx";
const USER_AGENT: &str = "Mozilla/5.0 (compatible; SlugMCP/0.1)";

pub fn find_hall(query: &str) -> Option<&'static DiningHall> {
    let q = query.to_lowercase();

    // Exact match on name (no allocation — ASCII-safe)
    for hall in DINING_HALLS {
        if hall.name.eq_ignore_ascii_case(&q) {
            return Some(hall);
        }
    }

    // Short name match
    for hall in DINING_HALLS {
        for short in hall.short_names {
            if q.contains(short) || short.contains(&*q) {
                return Some(hall);
            }
        }
    }

    // Substring match on full name
    for hall in DINING_HALLS {
        if hall.name.to_lowercase().contains(&q) {
            return Some(hall);
        }
    }

    None
}

static HALL_NAMES: OnceLock<String> = OnceLock::new();

pub fn hall_names() -> &'static str {
    HALL_NAMES.get_or_init(|| {
        DINING_HALLS
            .iter()
            .map(|h| h.name)
            .collect::<Vec<_>>()
            .join(", ")
    })
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

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MenuItem {
    pub name: String,
    pub dietary_tags: Vec<String>,
    pub recipe_id: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Meal {
    pub name: String,
    pub categories: Vec<Category>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Category {
    pub name: String,
    pub items: Vec<MenuItem>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
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

impl fmt::Display for DiningMenu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.date {
            Some(ref d) => write!(f, "## {} ({})\n\n", self.hall_name, d)?,
            None => write!(f, "## {}\n\n", self.hall_name)?,
        }
        if self.meals.is_empty() {
            write!(f, "No menu items available.\n")?;
            return Ok(());
        }
        for meal in &self.meals {
            write!(f, "### {}\n", meal.name)?;
            for cat in &meal.categories {
                write!(f, "**{}**\n", cat.name)?;
                for item in &cat.items {
                    write!(f, "- {}", item.name)?;
                    if !item.dietary_tags.is_empty() {
                        write!(f, " [{}]", item.dietary_tags.join(", "))?;
                    }
                    if let Some(ref id) = item.recipe_id {
                        write!(f, " (recipe: {})", id)?;
                    }
                    writeln!(f)?;
                }
                writeln!(f)?;
            }
        }
        Ok(())
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
    let html = resp.text().await.context("Failed to read nutrition page body")?;

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

    // Best-effort: enrich with recipe IDs from longmenu (one page per meal)
    let meal_names: Vec<String> = menu.meals.iter().map(|m| m.name.clone()).collect();
    for meal_name in &meal_names {
        let long_url = menu_url(LONGMENU_BASE, hall, date, Some(meal_name));
        match fetch_with_cookies(client, &long_url).await {
            Ok(long_html) => {
                let long_menu = parse_longmenu(&long_html, hall.name);
                enrich_recipe_ids(&mut menu, &long_menu);
            }
            Err(e) => {
                tracing::warn!("Longmenu fallback failed for {} ({}): {}", hall.name, meal_name, e);
            }
        }
    }

    Ok(menu)
}

fn parse_shortmenu(html: &str, hall_name: &str) -> DiningMenu {
    let document = Html::parse_document(html);

    let img_sel = sel(&SEL_IMG, "img");
    let td_sel = sel(&SEL_TD, "td");
    let all_sel = sel(
        &SEL_SHORT_MENU,
        "div.shortmenumeals, div.shortmenucats, div.shortmenurecipes",
    );

    let mut meals: Vec<Meal> = Vec::new();
    let mut current_meal: Option<Meal> = None;
    let mut current_cat: Option<Category> = None;

    for element in document.select(all_sel) {
        let classes: Vec<&str> = element.value().classes().collect();

        if classes.contains(&"shortmenumeals") {
            // Flush previous state
            if let Some(cat) = current_cat.take() {
                if let Some(meal) = current_meal.as_mut() {
                    if !cat.items.is_empty() {
                        meal.categories.push(cat);
                    }
                }
            }
            if let Some(meal) = current_meal.take() {
                if !meal.categories.is_empty() {
                    meals.push(meal);
                }
            }

            let meal_name = element.text().collect::<String>().trim().to_string();
            current_meal = Some(Meal {
                name: meal_name,
                categories: Vec::new(),
            });
        } else if classes.contains(&"shortmenucats") {
            if let Some(cat) = current_cat.take() {
                if let Some(meal) = current_meal.as_mut() {
                    if !cat.items.is_empty() {
                        meal.categories.push(cat);
                    }
                }
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
        } else if classes.contains(&"shortmenurecipes") {
            if let Some(cat) = current_cat.as_mut() {
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
                if let Some(parent_td) = element.parent() {
                    if let Some(parent_tr) = parent_td.parent() {
                        // Check the outer row's sibling tds for images
                        if let Some(outer_td) = parent_tr.parent() {
                            if let Some(outer_tr) = outer_td.parent() {
                                if let Some(outer_el) = scraper::ElementRef::wrap(outer_tr) {
                                    for td in outer_el.select(td_sel) {
                                        for img in td.select(img_sel) {
                                            if let Some(src) = img.value().attr("src") {
                                                if let Some(icon_name) = src
                                                    .strip_prefix("LegendImages/")
                                                    .and_then(|s| s.strip_suffix(".gif"))
                                                {
                                                    if let Some(tag) = icon_to_tag(icon_name) {
                                                        dietary_tags.push(tag.to_string());
                                                    }
                                                }
                                            }
                                        }
                                    }
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
    }

    // Flush remaining
    if let Some(cat) = current_cat {
        if let Some(meal) = current_meal.as_mut() {
            if !cat.items.is_empty() {
                meal.categories.push(cat);
            }
        }
    }
    if let Some(meal) = current_meal {
        if !meal.categories.is_empty() {
            meals.push(meal);
        }
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
                if item.recipe_id.is_none() {
                    if let Some(rid) = recipe_map.get(&item.name.to_lowercase()) {
                        item.recipe_id = Some(rid.clone());
                    }
                }
            }
        }
    }
}

fn parse_longmenu(html: &str, hall_name: &str) -> DiningMenu {
    let document = Html::parse_document(html);

    let anchor_sel = sel(&SEL_ANCHOR, "a[name]");
    let link_sel = sel(&SEL_LINK, "a[href]");
    let img_sel = sel(&SEL_IMG, "img");
    let td_sel = sel(&SEL_TD, "td");
    let all_sel = sel(
        &SEL_ALL_MENU,
        "td.longmenugridheader, div.longmenucolmenucat, div.longmenucoldispname",
    );

    let mut meals: Vec<Meal> = Vec::new();
    let mut current_meal: Option<Meal> = None;
    let mut current_cat: Option<Category> = None;

    for element in document.select(all_sel) {
        let classes: Vec<&str> = element.value().classes().collect();

        if classes.contains(&"longmenugridheader") {
            // Finish previous category and meal
            if let Some(cat) = current_cat.take() {
                if let Some(meal) = current_meal.as_mut() {
                    if !cat.items.is_empty() {
                        meal.categories.push(cat);
                    }
                }
            }
            if let Some(meal) = current_meal.take() {
                if !meal.categories.is_empty() {
                    meals.push(meal);
                }
            }

            // Extract meal name from <a name="MealName">
            let meal_name = element
                .select(anchor_sel)
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
            if let Some(cat) = current_cat.take() {
                if let Some(meal) = current_meal.as_mut() {
                    if !cat.items.is_empty() {
                        meal.categories.push(cat);
                    }
                }
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
        } else if classes.contains(&"longmenucoldispname") {
            if let Some(cat) = current_cat.as_mut() {
                // Extract item name and recipe ID from <a> link
                let (item_name, recipe_id) = if let Some(a) = element.select(link_sel).next() {
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
                {
                    if let Some(parent_el) = scraper::ElementRef::wrap(parent_tr) {
                        for td in parent_el.select(td_sel) {
                            for img in td.select(img_sel) {
                                if let Some(src) = img.value().attr("src") {
                                    if let Some(icon_name) = src
                                        .strip_prefix("LegendImages/")
                                        .and_then(|s| s.strip_suffix(".gif"))
                                    {
                                        if let Some(tag) = icon_to_tag(icon_name) {
                                            dietary_tags.push(tag.to_string());
                                        }
                                    }
                                }
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
    }

    // Flush remaining category and meal
    if let Some(cat) = current_cat {
        if let Some(meal) = current_meal.as_mut() {
            if !cat.items.is_empty() {
                meal.categories.push(cat);
            }
        }
    }
    if let Some(meal) = current_meal {
        if !meal.categories.is_empty() {
            meals.push(meal);
        }
    }

    DiningMenu {
        hall_name: hall_name.to_string(),
        date: None,
        meals,
    }
}

// --- Meal balance ---

#[cfg(feature = "auth")]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MealBalance {
    pub slug_points: Option<f64>,
    pub banana_bucks: Option<f64>,
    pub meal_swipes: Option<u32>,
}

#[cfg(feature = "auth")]
impl fmt::Display for MealBalance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "# Meal Plan Balance\n\n")?;
        if let Some(sp) = self.slug_points {
            write!(f, "- **Slug Points**: ${:.2}\n", sp)?;
        }
        if let Some(bb) = self.banana_bucks {
            write!(f, "- **Banana Bucks**: ${:.2}\n", bb)?;
        }
        if let Some(ms) = self.meal_swipes {
            write!(f, "- **Meal Swipes Remaining**: {}\n", ms)?;
        }
        if self.slug_points.is_none() && self.banana_bucks.is_none() && self.meal_swipes.is_none() {
            write!(f, "Could not retrieve balance information. The balance page may have changed.\n")?;
        }
        Ok(())
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
pub async fn scrape_balance(client: &reqwest::Client) -> Result<BalanceResult> {
    // The meal plan balance is available through the GET system at
    // get.cbord.com/ucsc. This requires an authenticated session — the
    // client should have IdP cookies from browser SSO that allow the
    // SAML redirect chain to auto-approve.
    let url = "https://get.cbord.com/ucsc/full/funds_home.php";

    let resp = crate::auth::saml_aware_get(client, url)
        .await
        .context("failed to fetch balance page")?;

    if !resp.status.is_success() {
        anyhow::bail!("Balance page returned status {}", resp.status);
    }

    // Try to parse balance values
    if let Some(balance) = try_parse_balance(&resp.body) {
        return Ok(BalanceResult {
            balance,
            debug_snippet: None,
        });
    }

    // Parsing failed — extract visible text (skipping script/style) for debugging
    let page_text = extract_visible_text(&resp.body);
    let clean_text: String = page_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let snippet = if clean_text.len() > 1000 {
        format!("{}...", &clean_text[..1000])
    } else {
        clean_text
    };

    Ok(BalanceResult {
        balance: MealBalance {
            slug_points: None,
            banana_bucks: None,
            meal_swipes: None,
        },
        debug_snippet: Some(snippet),
    })
}

#[cfg(feature = "auth")]
/// Extract visible text from HTML, stripping script/style/noscript blocks first.
fn extract_visible_text(html: &str) -> String {
    // Strip <script>...</script>, <style>...</style>, <noscript>...</noscript> blocks
    let stripped = strip_tag_blocks(html, "script");
    let stripped = strip_tag_blocks(&stripped, "style");
    let stripped = strip_tag_blocks(&stripped, "noscript");

    let document = Html::parse_document(&stripped);
    document.root_element().text().collect::<String>()
}

#[cfg(feature = "auth")]
/// Remove all occurrences of <tag ...>...</tag> (case-insensitive) from HTML.
fn strip_tag_blocks(html: &str, tag: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let lower = html.to_lowercase();
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);

    let mut pos = 0;
    while pos < html.len() {
        if let Some(start) = lower[pos..].find(&open) {
            let abs_start = pos + start;
            result.push_str(&html[pos..abs_start]);
            // Find closing tag
            if let Some(end) = lower[abs_start..].find(&close) {
                pos = abs_start + end + close.len();
            } else {
                // No closing tag found — skip rest
                break;
            }
        } else {
            result.push_str(&html[pos..]);
            break;
        }
    }
    result
}

#[cfg(feature = "auth")]
fn try_parse_balance(html: &str) -> Option<MealBalance> {
    let text = extract_visible_text(html);

    let slug_points = extract_balance_value(&text, "slug points");
    let banana_bucks = extract_balance_value(&text, "banana bucks");

    if slug_points.is_some() || banana_bucks.is_some() {
        Some(MealBalance {
            slug_points,
            banana_bucks,
            meal_swipes: None,
        })
    } else {
        None
    }
}

#[cfg(feature = "auth")]
fn extract_balance_value(text: &str, label: &str) -> Option<f64> {
    let lower = text.to_lowercase();
    let idx = lower.find(label)?;
    let after = &text[idx + label.len()..];

    let mut num_str = String::new();
    let mut found_dollar = false;
    for c in after.chars() {
        if c == '$' {
            found_dollar = true;
            continue;
        }
        if found_dollar && (c.is_ascii_digit() || c == '.' || c == ',') {
            if c != ',' {
                num_str.push(c);
            }
        } else if found_dollar && !num_str.is_empty() {
            break;
        }
    }

    if num_str.is_empty() {
        None
    } else {
        num_str.parse().ok()
    }
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

pub async fn scrape_nutrition(
    client: &reqwest::Client,
    recipe_id: &str,
) -> Result<NutritionInfo> {
    let url = format!("{}?RecNumAndPort={}", LABEL_URL, recipe_id);
    let html = fetch_with_cookies(client, &url).await?;
    Ok(parse_nutrition_label(&html))
}

fn parse_nutrition_label(html: &str) -> NutritionInfo {
    let document = Html::parse_document(html);

    let recipe_sel = sel(&SEL_RECIPE, "div.labelrecipe");
    let item_name = document
        .select(recipe_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    let ingredients_sel = sel(&SEL_INGREDIENTS, "span.labelingredientsvalue");
    let ingredients = document
        .select(ingredients_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    let allergens_sel = sel(&SEL_ALLERGENS, "span.labelallergensvalue");
    let allergens = document
        .select(allergens_sel)
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

fn display_nutrient(f: &mut fmt::Formatter<'_>, label: &str, value: &Option<String>) -> fmt::Result {
    match value {
        Some(v) => write!(f, "| {} | {} |\n", label, v),
        None => write!(f, "| {} | N/A |\n", label),
    }
}

impl fmt::Display for NutritionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "# {}\n\n", self.item_name)?;
        write!(f, "**Serving Size:** {}\n", self.serving_size)?;
        match self.calories {
            Some(ref c) => write!(f, "**Calories:** {}\n\n", c)?,
            None => write!(f, "**Calories:** N/A\n\n")?,
        }
        write!(f, "| Nutrient | Amount |\n")?;
        write!(f, "|----------|--------|\n")?;
        display_nutrient(f, "Total Fat", &self.total_fat)?;
        display_nutrient(f, "Saturated Fat", &self.saturated_fat)?;
        display_nutrient(f, "Trans Fat", &self.trans_fat)?;
        display_nutrient(f, "Cholesterol", &self.cholesterol)?;
        display_nutrient(f, "Sodium", &self.sodium)?;
        display_nutrient(f, "Total Carbs", &self.total_carbs)?;
        display_nutrient(f, "Dietary Fiber", &self.dietary_fiber)?;
        display_nutrient(f, "Sugars", &self.sugars)?;
        display_nutrient(f, "Protein", &self.protein)?;
        write!(f, "\n**Ingredients:** {}\n\n", self.ingredients)?;
        write!(f, "**Allergens:** {}\n", self.allergens)?;
        Ok(())
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

    let schema_sel = sel(&SEL_SCHEMA, r#"div[itemtype]"#);
    let name_sel = sel(&SEL_NAME, r#"meta[itemprop="name"]"#);
    let hours_sel = sel(&SEL_HOURS, r#"meta[itemprop="openingHours"]"#);
    let spec_sel = sel(&SEL_SPEC, r#"div[itemprop="openingHoursSpecification"]"#);
    let time_sel = sel(&SEL_TIME, "time");

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for schema_div in document.select(schema_sel) {
        let itemtype = schema_div.value().attr("itemtype").unwrap_or("");
        if !itemtype.contains("schema.org/Restaurant")
            && !itemtype.contains("schema.org/FoodEstablishment")
            && !itemtype.contains("schema.org/LocalBusiness")
        {
            continue;
        }

        let name = match schema_div.select(name_sel).next() {
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
            .select(hours_sel)
            .filter_map(|el| el.value().attr("content").map(|s| s.to_string()))
            .collect();

        let mut date_hours = Vec::new();
        for spec in schema_div.select(spec_sel) {
            let times: Vec<_> = spec.select(time_sel).collect();
            if times.is_empty() {
                continue;
            }

            let date = times[0]
                .value()
                .attr("datetime")
                .unwrap_or("")
                .to_string();

            let (opens, closes) = if times.len() >= 3 {
                (
                    times[1].value().attr("datetime").map(|s| s.to_string()),
                    times[2].value().attr("datetime").map(|s| s.to_string()),
                )
            } else {
                (None, None)
            };

            if !date.is_empty() {
                date_hours.push(DateHours { date, opens, closes });
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
        let _ = write!(out, "### {} ({})\n", self.name, self.category);
        if self.regular_hours.is_empty() {
            out.push_str("Hours not available\n");
        } else {
            out.push_str("**Regular Hours:**\n");
            for h in &self.regular_hours {
                let _ = write!(out, "- {}\n", h);
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
                        let _ = write!(out, "- {}: {} - {}\n", dh.date, o, c);
                    }
                    _ => {
                        let _ = write!(out, "- {}: CLOSED\n", dh.date);
                    }
                }
            }
        }
        out.push('\n');
        out
    }
}

impl fmt::Display for DiningLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        f.write_str(&self.format_with_date(&today))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(entrees.items[0].dietary_tags.contains(&"vegetarian".to_string()));
        assert!(entrees.items[0].dietary_tags.contains(&"contains_eggs".to_string()));

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
            entrees.items[0].dietary_tags.contains(&"vegetarian".to_string()),
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
