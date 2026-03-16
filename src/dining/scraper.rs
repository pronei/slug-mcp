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

const BASE_URL: &str = "https://nutrition.sa.ucsc.edu/shortmenu.aspx";

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

#[derive(Debug)]
pub struct Meal {
    pub name: String,
    pub items: Vec<String>,
}

#[derive(Debug)]
pub struct DiningMenu {
    pub hall_name: String,
    pub meals: Vec<Meal>,
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
            for item in &meal.items {
                out.push_str(&format!("- {}\n", item));
            }
            out.push('\n');
        }
        out
    }
}

pub async fn scrape_menu(
    client: &reqwest::Client,
    hall: &DiningHall,
) -> Result<DiningMenu> {
    let url = menu_url(hall);
    let resp = client
        .get(&url)
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

    let document = Html::parse_document(&html);

    // FoodPro uses these CSS classes:
    // .shortmenumeals - meal period headers (Breakfast, Lunch, etc.)
    // .shortmenucats - station/category headers
    // .shortmenurecipes - individual menu items
    let meal_sel = Selector::parse(".shortmenumeals").unwrap();
    let recipe_sel = Selector::parse(".shortmenurecipes a").unwrap();
    let cat_sel = Selector::parse(".shortmenucats").unwrap();

    let mut meals: Vec<Meal> = Vec::new();

    // Walk the DOM: each meal header starts a section, recipes follow
    // We need to iterate through all relevant elements in document order
    let all_sel = Selector::parse(".shortmenumeals, .shortmenucats, .shortmenurecipes").unwrap();

    let mut current_meal: Option<Meal> = None;

    for element in document.select(&all_sel) {
        if element.value().classes().any(|c| c == "shortmenumeals") {
            // Save previous meal if exists
            if let Some(meal) = current_meal.take() {
                if !meal.items.is_empty() {
                    meals.push(meal);
                }
            }
            let name = element.text().collect::<String>().trim().to_string();
            if !name.is_empty() {
                current_meal = Some(Meal {
                    name,
                    items: Vec::new(),
                });
            }
        } else if element.value().classes().any(|c| c == "shortmenurecipes") {
            // Extract recipe name from the anchor tag inside
            if let Some(meal) = current_meal.as_mut() {
                // Try to get text from nested <a> tag
                if let Some(link) = element.select(&recipe_sel).next() {
                    let text = link.text().collect::<String>().trim().to_string();
                    if !text.is_empty() {
                        meal.items.push(text);
                    }
                } else {
                    // Fallback: get direct text
                    let text = element.text().collect::<String>().trim().to_string();
                    if !text.is_empty() {
                        meal.items.push(text);
                    }
                }
            }
        }
        // We skip .shortmenucats - they're station names, not needed for the basic menu
    }

    // Don't forget the last meal
    if let Some(meal) = current_meal {
        if !meal.items.is_empty() {
            meals.push(meal);
        }
    }

    // Suppress unused selector warnings
    let _ = &meal_sel;
    let _ = &cat_sel;

    Ok(DiningMenu {
        hall_name: hall.name.to_string(),
        meals,
    })
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

pub fn dining_hours() -> String {
    // Hours change quarterly but rarely mid-quarter.
    // This is a reasonable default; can be updated each quarter.
    r#"# UCSC Dining Hall Hours (approximate)

**Note**: Hours may vary. Check https://dining.ucsc.edu for current hours.

## Dining Halls (general schedule)
- **Breakfast**: 7:00 AM - 10:00 AM
- **Continuous Dining**: 10:00 AM - 11:00 AM
- **Lunch**: 11:00 AM - 2:00 PM
- **Continuous Dining**: 2:00 PM - 5:00 PM
- **Dinner**: 5:00 PM - 8:00 PM
- **Late Night**: 8:00 PM - 11:00 PM (select locations)

## Cafes & Coffee Bars
- Hours vary by location, generally 8:00 AM - 4:00 PM weekdays

## Weekend Hours
- Brunch typically replaces breakfast/lunch: 10:00 AM - 2:00 PM
- Dinner: 5:00 PM - 8:00 PM
- Not all locations open on weekends
"#
    .to_string()
}
