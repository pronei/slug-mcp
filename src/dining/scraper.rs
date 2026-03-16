use anyhow::{Context, Result};
use scraper::{Html, Selector};

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

const BASE_URL: &str = "https://nutrition.sa.ucsc.edu/longmenu.aspx";

pub fn find_hall(query: &str) -> Option<&'static DiningHall> {
    let q = query.to_lowercase();

    // Exact match on name
    for hall in DINING_HALLS {
        if hall.name.to_lowercase() == q {
            return Some(hall);
        }
    }

    // Short name match
    for hall in DINING_HALLS {
        for short in hall.short_names {
            if q.contains(short) || short.contains(&q) {
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

pub fn hall_names() -> String {
    DINING_HALLS
        .iter()
        .map(|h| h.name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn menu_url(hall: &DiningHall) -> String {
    format!(
        "{}?sName=UC+Santa+Cruz+Dining&locationNum={}&locationName={}&naFlag=1",
        BASE_URL, hall.location_num, hall.location_name
    )
}

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
        let mut out = format!("## {}\n\n", self.hall_name);
        if self.meals.is_empty() {
            out.push_str("No menu items available.\n");
            return out;
        }
        for meal in &self.meals {
            out.push_str(&format!("### {}\n", meal.name));
            for cat in &meal.categories {
                out.push_str(&format!("**{}**\n", cat.name));
                for item in &cat.items {
                    out.push_str(&format!("- {}", item.name));
                    if !item.dietary_tags.is_empty() {
                        out.push_str(&format!(" [{}]", item.dietary_tags.join(", ")));
                    }
                    if let Some(ref id) = item.recipe_id {
                        out.push_str(&format!(" (recipe: {})", id));
                    }
                    out.push('\n');
                }
                out.push('\n');
            }
        }
        out
    }
}

/// The nutrition site requires certain cookies to be present or it returns 500.
/// The cookies can be empty-valued; they just need to exist in the request.
const NUTRITION_COOKIES: &str =
    "WebInaCartLocation=; WebInaCartDates=; WebInaCartMeals=; WebInaCartRecipes=; WebInaCartQtys=";

pub async fn scrape_menu(
    client: &reqwest::Client,
    hall: &DiningHall,
) -> Result<DiningMenu> {
    let url = menu_url(hall);
    let resp = client
        .get(&url)
        .header("Cookie", NUTRITION_COOKIES)
        .send()
        .await
        .context("Failed to fetch menu page")?;

    let status = resp.status();
    let html = resp.text().await.context("Failed to read menu page body")?;

    if !status.is_success() || html.contains("Runtime Error") || html.contains("Server Error") {
        return Ok(DiningMenu {
            hall_name: hall.name.to_string(),
            meals: vec![],
        });
    }

    Ok(parse_longmenu(&html, hall.name))
}

fn parse_longmenu(html: &str, hall_name: &str) -> DiningMenu {
    let document = Html::parse_document(html);

    let anchor_sel = Selector::parse("a[name]").unwrap();
    let link_sel = Selector::parse("a[href]").unwrap();
    let img_sel = Selector::parse("img").unwrap();

    // Walk all relevant elements in document order
    let all_sel = Selector::parse(
        "td.longmenugridheader, div.longmenucolmenucat, div.longmenucoldispname",
    )
    .unwrap();

    let mut meals: Vec<Meal> = Vec::new();
    let mut current_meal: Option<Meal> = None;
    let mut current_cat: Option<Category> = None;

    for element in document.select(&all_sel) {
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
                .select(&anchor_sel)
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
                let (item_name, recipe_id) = if let Some(a) = element.select(&link_sel).next() {
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
                    let td_sel = Selector::parse("td").unwrap();
                    let parent_el = scraper::ElementRef::wrap(parent_tr);
                    if let Some(parent_el) = parent_el {
                        for td in parent_el.select(&td_sel) {
                            for img in td.select(&img_sel) {
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
        meals,
    }
}

#[derive(Debug)]
pub struct MealBalance {
    pub slug_points: Option<f64>,
    pub banana_bucks: Option<f64>,
    pub meal_swipes: Option<u32>,
}

impl MealBalance {
    pub fn format(&self) -> String {
        let mut out = String::from("# Meal Plan Balance\n\n");
        if let Some(sp) = self.slug_points {
            out.push_str(&format!("- **Slug Points**: ${:.2}\n", sp));
        }
        if let Some(bb) = self.banana_bucks {
            out.push_str(&format!("- **Banana Bucks**: ${:.2}\n", bb));
        }
        if let Some(ms) = self.meal_swipes {
            out.push_str(&format!("- **Meal Swipes Remaining**: {}\n", ms));
        }
        if self.slug_points.is_none() && self.banana_bucks.is_none() && self.meal_swipes.is_none() {
            out.push_str("Could not retrieve balance information. The balance page may have changed.\n");
        }
        out
    }
}

pub async fn scrape_balance(client: &reqwest::Client) -> Result<MealBalance> {
    // The meal plan balance is typically available through the GET system
    // at get.cbord.com/ucsc or through the UCSC dining portal.
    // This requires an authenticated session with CAS cookies.
    //
    // TODO: Once we have a real session to test with, discover the exact
    // URL and HTML structure for balance data.
    let urls = [
        "https://nutrition.sa.ucsc.edu/longmenu.aspx",
        "https://get.cbord.com/ucsc/full/funds_home.php",
    ];

    for url in &urls {
        let resp = client.get(*url).send().await;
        if let Ok(resp) = resp {
            if resp.status().is_success() {
                let html = resp.text().await.unwrap_or_default();
                if let Some(balance) = try_parse_balance(&html) {
                    return Ok(balance);
                }
            }
        }
    }

    // Return empty balance if we can't find the data
    Ok(MealBalance {
        slug_points: None,
        banana_bucks: None,
        meal_swipes: None,
    })
}

fn try_parse_balance(html: &str) -> Option<MealBalance> {
    let document = Html::parse_document(html);

    // Try to find balance values - the exact selectors will need to be
    // determined from a real authenticated page
    let mut slug_points = None;
    let mut banana_bucks = None;

    // Look for common patterns in dining balance pages
    let text = document.root_element().text().collect::<String>();

    // Search for "Slug Points" followed by a dollar amount
    if let Some(sp) = extract_balance_value(&text, "slug points") {
        slug_points = Some(sp);
    }
    if let Some(bb) = extract_balance_value(&text, "banana bucks") {
        banana_bucks = Some(bb);
    }

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

fn extract_balance_value(text: &str, label: &str) -> Option<f64> {
    let lower = text.to_lowercase();
    let idx = lower.find(label)?;
    let after = &text[idx + label.len()..];

    // Find the next dollar amount pattern
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

    if !num_str.is_empty() {
        num_str.parse().ok()
    } else {
        None
    }
}

// --- Nutrition label scraper ---

#[derive(Debug, Clone)]
pub struct NutritionInfo {
    pub item_name: String,
    pub serving_size: String,
    pub calories: String,
    pub total_fat: String,
    pub saturated_fat: String,
    pub trans_fat: String,
    pub cholesterol: String,
    pub sodium: String,
    pub total_carbs: String,
    pub dietary_fiber: String,
    pub sugars: String,
    pub protein: String,
    pub ingredients: String,
    pub allergens: String,
}

const LABEL_URL: &str = "https://nutrition.sa.ucsc.edu/label.aspx";

pub async fn scrape_nutrition(
    client: &reqwest::Client,
    recipe_id: &str,
) -> Result<NutritionInfo> {
    let url = format!("{}?RecNumAndPort={}", LABEL_URL, recipe_id);
    let resp = client
        .get(&url)
        .header("Cookie", NUTRITION_COOKIES)
        .send()
        .await
        .context("Failed to fetch nutrition label")?;

    let html = resp.text().await.context("Failed to read label page")?;
    parse_nutrition_label(&html)
}

fn parse_nutrition_label(html: &str) -> Result<NutritionInfo> {
    let document = Html::parse_document(html);

    let recipe_sel = Selector::parse("div.labelrecipe").unwrap();
    let item_name = document
        .select(&recipe_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    let ingredients_sel = Selector::parse("span.labelingredientsvalue").unwrap();
    let ingredients = document
        .select(&ingredients_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    let allergens_sel = Selector::parse("span.labelallergensvalue").unwrap();
    let allergens = document
        .select(&allergens_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    let text = document.root_element().text().collect::<String>();

    let serving_size = extract_after(&text, "Serving Size").unwrap_or_default();
    let calories = extract_after(&text, "Calories").unwrap_or_default();
    let total_fat = extract_nutrient(&text, "Total Fat");
    let saturated_fat = extract_nutrient(&text, "Sat. Fat");
    let trans_fat = extract_nutrient(&text, "Trans Fat");
    let cholesterol = extract_nutrient(&text, "Cholesterol");
    let sodium = extract_nutrient(&text, "Sodium");
    let total_carbs = extract_nutrient(&text, "Tot. Carb.");
    let dietary_fiber = extract_nutrient(&text, "Dietary Fiber");
    let sugars = extract_nutrient(&text, "Sugars");
    let protein = extract_nutrient(&text, "Protein");

    Ok(NutritionInfo {
        item_name,
        serving_size,
        calories,
        total_fat,
        saturated_fat,
        trans_fat,
        cholesterol,
        sodium,
        total_carbs,
        dietary_fiber,
        sugars,
        protein,
        ingredients,
        allergens,
    })
}

fn extract_after(text: &str, label: &str) -> Option<String> {
    let idx = text.find(label)?;
    let after = text[idx + label.len()..].trim_start();
    let value: String = after
        .chars()
        .take_while(|c| !c.is_control() && *c != '\n')
        .collect::<String>()
        .trim()
        .to_string();
    let value = value
        .split(|c: char| c == '\n' || c == '\r')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn extract_nutrient(text: &str, label: &str) -> String {
    extract_after(text, label).unwrap_or_else(|| "N/A".to_string())
}

impl NutritionInfo {
    pub fn format(&self) -> String {
        format!(
            "# {}\n\n\
             **Serving Size:** {}\n\
             **Calories:** {}\n\n\
             | Nutrient | Amount |\n\
             |----------|--------|\n\
             | Total Fat | {} |\n\
             | Saturated Fat | {} |\n\
             | Trans Fat | {} |\n\
             | Cholesterol | {} |\n\
             | Sodium | {} |\n\
             | Total Carbs | {} |\n\
             | Dietary Fiber | {} |\n\
             | Sugars | {} |\n\
             | Protein | {} |\n\n\
             **Ingredients:** {}\n\n\
             **Allergens:** {}\n",
            self.item_name,
            self.serving_size,
            self.calories,
            self.total_fat,
            self.saturated_fat,
            self.trans_fat,
            self.cholesterol,
            self.sodium,
            self.total_carbs,
            self.dietary_fiber,
            self.sugars,
            self.protein,
            self.ingredients,
            self.allergens,
        )
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
    let document = Html::parse_document(html);
    let mut locations = Vec::new();

    let schema_sel = Selector::parse(r#"div[itemtype]"#).unwrap();
    let name_sel = Selector::parse(r#"meta[itemprop="name"]"#).unwrap();
    let hours_sel = Selector::parse(r#"meta[itemprop="openingHours"]"#).unwrap();
    let spec_sel =
        Selector::parse(r#"div[itemprop="openingHoursSpecification"]"#).unwrap();
    let time_sel = Selector::parse("time").unwrap();

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for schema_div in document.select(&schema_sel) {
        let itemtype = schema_div.value().attr("itemtype").unwrap_or("");
        if !itemtype.contains("schema.org/Restaurant")
            && !itemtype.contains("schema.org/FoodEstablishment")
            && !itemtype.contains("schema.org/LocalBusiness")
        {
            continue;
        }

        let name = match schema_div.select(&name_sel).next() {
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
            .select(&hours_sel)
            .filter_map(|el| el.value().attr("content").map(|s| s.to_string()))
            .collect();

        let mut date_hours = Vec::new();
        for spec in schema_div.select(&spec_sel) {
            let times: Vec<_> = spec.select(&time_sel).collect();
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
    pub fn format(&self) -> String {
        let mut out = format!("### {} ({})\n", self.name, self.category);
        if self.regular_hours.is_empty() {
            out.push_str("Hours not available\n");
        } else {
            out.push_str("**Regular Hours:**\n");
            for h in &self.regular_hours {
                out.push_str(&format!("- {}\n", h));
            }
        }

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let upcoming: Vec<_> = self
            .date_hours
            .iter()
            .filter(|d| d.date >= today)
            .take(5)
            .collect();
        if !upcoming.is_empty() {
            out.push_str("\n**Upcoming Special Hours:**\n");
            for dh in upcoming {
                match (&dh.opens, &dh.closes) {
                    (Some(o), Some(c)) => {
                        out.push_str(&format!("- {}: {} - {}\n", dh.date, o, c))
                    }
                    _ => out.push_str(&format!("- {}: CLOSED\n", dh.date)),
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
        let info = parse_nutrition_label(MOCK_LABEL_HTML).unwrap();
        assert_eq!(info.item_name, "Belgian Waffle Squares");
        assert_eq!(info.ingredients, "Enriched Wheat Flour, Sugar, Eggs");
        assert_eq!(info.allergens, "Milk, Egg, Wheat, Soy");
        assert!(info.calories.contains("180"));
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
}
