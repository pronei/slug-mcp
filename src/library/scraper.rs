use std::fmt;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use scraper::{Html, Selector};

fn sel<'a>(cell: &'a OnceLock<Selector>, s: &str) -> &'a Selector {
    cell.get_or_init(|| Selector::parse(s).expect("hardcoded selector"))
}

static SEL_SPACE_ITEM: OnceLock<Selector> = OnceLock::new();
static SEL_SPACE_NAME: OnceLock<Selector> = OnceLock::new();
static SEL_SPACE_LINK: OnceLock<Selector> = OnceLock::new();
static SEL_AVAIL_SLOT: OnceLock<Selector> = OnceLock::new();
static SEL_BOOKED_SLOT: OnceLock<Selector> = OnceLock::new();

const LIBCAL_BASE: &str = "https://calendar.library.ucsc.edu";

// ─── Library definitions ───

pub struct Library {
    pub name: &'static str,
    pub lid: u32,
    pub short_names: &'static [&'static str],
}

pub static LIBRARIES: &[Library] = &[
    Library {
        name: "McHenry Library",
        lid: 16577,
        short_names: &["mchenry", "mc henry"],
    },
    Library {
        name: "Science & Engineering Library",
        lid: 16640,
        short_names: &["science", "s&e", "se", "engineering", "s and e"],
    },
];

pub fn find_library(query: &str) -> Option<&'static Library> {
    let q = query.to_lowercase();
    LIBRARIES.iter().find(|lib| {
        lib.name.to_lowercase().contains(&q)
            || lib.short_names.iter().any(|s| s.contains(&q) || q.contains(s))
    })
}

pub fn library_names() -> String {
    LIBRARIES
        .iter()
        .map(|l| l.name)
        .collect::<Vec<_>>()
        .join(", ")
}

// ─── Data types ───

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoomAvailability {
    pub library_name: String,
    pub date: String,
    pub rooms: Vec<Room>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Room {
    pub name: String,
    pub space_id: Option<u32>,
    pub capacity: Option<u32>,
    pub available_slots: Vec<TimeSlot>,
    pub booked_slots: Vec<TimeSlot>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TimeSlot {
    pub start: String,
    pub end: String,
}

impl fmt::Display for RoomAvailability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "## {} — {}", self.library_name, self.date)?;
        if self.rooms.is_empty() {
            write!(f, "No rooms found or availability data unavailable.")?;
            return Ok(());
        }
        for room in &self.rooms {
            write!(f, "\n{}", room)?;
        }
        Ok(())
    }
}

impl fmt::Display for Room {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cap_str = self
            .capacity
            .map(|c| format!(" (capacity: {})", c))
            .unwrap_or_default();
        let id_str = self
            .space_id
            .map(|id| format!(" [space_id: {}]", id))
            .unwrap_or_default();
        write!(f, "### {}{}{}", self.name, cap_str, id_str)?;

        if !self.available_slots.is_empty() {
            let slots: Vec<String> = self
                .available_slots
                .iter()
                .map(|s| format!("{} - {}", s.start, s.end))
                .collect();
            write!(f, "\n- **Available**: {}", slots.join(", "))?;
        }
        if !self.booked_slots.is_empty() {
            let slots: Vec<String> = self
                .booked_slots
                .iter()
                .map(|s| format!("{} - {}", s.start, s.end))
                .collect();
            write!(f, "\n- **Booked**: {}", slots.join(", "))?;
        }
        if self.available_slots.is_empty() && self.booked_slots.is_empty() {
            write!(f, "\n- No time slot data available")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BookingResult {
    pub success: bool,
    pub message: String,
}

impl fmt::Display for BookingResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.success {
            write!(f, "**Booking confirmed!** {}", self.message)
        } else {
            write!(f, "**Booking failed.** {}", self.message)
        }
    }
}

// ─── Scraper functions ───

pub async fn scrape_availability(
    client: &reqwest::Client,
    lid: u32,
    date: &str,
) -> Result<RoomAvailability> {
    let library_name = LIBRARIES
        .iter()
        .find(|l| l.lid == lid)
        .map(|l| l.name)
        .unwrap_or("Unknown Library");

    // First GET the spaces page to establish any session cookies
    let spaces_url = format!("{}/spaces?lid={}&d={}", LIBCAL_BASE, lid, date);
    let page_resp = client
        .get(&spaces_url)
        .send()
        .await
        .context("Failed to load library spaces page")?;

    let page_html = page_resp
        .text()
        .await
        .context("Failed to read spaces page")?;

    // Parse the spaces page directly — it contains the room grid
    let rooms = parse_availability_page(&page_html);

    Ok(RoomAvailability {
        library_name: library_name.to_string(),
        date: date.to_string(),
        rooms,
    })
}

fn parse_availability_page(html: &str) -> Vec<Room> {
    let document = Html::parse_document(html);

    // LibCal renders rooms as items in a space grid
    // Try multiple selector strategies
    let item_sel = sel(&SEL_SPACE_ITEM, ".s-lc-eq-container, .s-lc-space-item, .fc-resource");
    let name_sel = sel(&SEL_SPACE_NAME, ".s-lc-eq-name, .fc-resource-label, h4, h3");
    let link_sel = sel(&SEL_SPACE_LINK, "a[href*='/space/']");
    let avail_sel = sel(&SEL_AVAIL_SLOT, ".s-lc-eq-avail");
    let booked_sel = sel(&SEL_BOOKED_SLOT, ".s-lc-eq-period-booked, .s-lc-eq-pending");

    let mut rooms = Vec::new();

    // Strategy 1: Parse structured space containers
    for item in document.select(item_sel) {
        let name = item
            .select(name_sel)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        if name.is_empty() {
            continue;
        }

        let space_id = item.select(link_sel).next().and_then(|a| {
            a.value()
                .attr("href")
                .and_then(|h| h.split("/space/").nth(1))
                .and_then(|s| s.trim_end_matches('/').parse().ok())
        });

        let available_slots: Vec<TimeSlot> = item
            .select(avail_sel)
            .filter_map(|el| extract_time_slot(&el))
            .collect();

        let booked_slots: Vec<TimeSlot> = item
            .select(booked_sel)
            .filter_map(|el| extract_time_slot(&el))
            .collect();

        rooms.push(Room {
            name,
            space_id,
            capacity: None,
            available_slots,
            booked_slots,
        });
    }

    // Strategy 2: If no structured containers, look for room links with space IDs
    if rooms.is_empty() {
        for link in document.select(link_sel) {
            let name = link.text().collect::<String>().trim().to_string();
            let space_id = link
                .value()
                .attr("href")
                .and_then(|h| h.split("/space/").nth(1))
                .and_then(|s| s.trim_end_matches('/').parse().ok());

            if !name.is_empty() {
                rooms.push(Room {
                    name,
                    space_id,
                    capacity: None,
                    available_slots: Vec::new(),
                    booked_slots: Vec::new(),
                });
            }
        }
    }

    rooms
}

fn extract_time_slot(el: &scraper::ElementRef) -> Option<TimeSlot> {
    // Time slots may have title/data attributes with time info
    let title = el.value().attr("title").or(el.value().attr("data-time"));
    if let Some(t) = title {
        let parts: Vec<&str> = t.split(" - ").collect();
        if parts.len() == 2 {
            return Some(TimeSlot {
                start: parts[0].trim().to_string(),
                end: parts[1].trim().to_string(),
            });
        }
    }

    // Try text content
    let text = el.text().collect::<String>().trim().to_string();
    if text.contains('-') {
        let parts: Vec<&str> = text.splitn(2, '-').collect();
        if parts.len() == 2 {
            return Some(TimeSlot {
                start: parts[0].trim().to_string(),
                end: parts[1].trim().to_string(),
            });
        }
    }

    None
}

pub async fn book_room(
    auth_client: &reqwest::Client,
    space_id: u32,
    date: &str,
    start_time: &str,
    end_time: &str,
) -> Result<BookingResult> {
    // First, visit the space page to establish session and get any CSRF tokens
    let space_url = format!("{}/space/{}", LIBCAL_BASE, space_id);
    let page_resp = auth_client
        .get(&space_url)
        .send()
        .await
        .context("Failed to load space page for booking")?;

    let page_html = page_resp.text().await.unwrap_or_default();

    // Look for CSRF token in the page
    let csrf_token = extract_csrf_token(&page_html);

    // Attempt to create a booking via the AJAX cart endpoint
    let cart_url = format!("{}/ajax/space/createcart", LIBCAL_BASE);
    let mut form: Vec<(&str, String)> = vec![
        ("id", space_id.to_string()),
        ("date", date.to_string()),
        ("start", start_time.to_string()),
        ("end", end_time.to_string()),
    ];

    if let Some(token) = &csrf_token {
        form.push(("_token", token.clone()));
    }

    let resp = auth_client
        .post(&cart_url)
        .form(&form)
        .send()
        .await
        .context("Failed to submit booking request")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if status.is_success() && !body.contains("error") && !body.contains("Error") {
        Ok(BookingResult {
            success: true,
            message: format!(
                "Room {} booked for {} from {} to {}. Check your email for confirmation.",
                space_id, date, start_time, end_time
            ),
        })
    } else {
        // Try to extract error message
        let msg = if body.contains("already booked") {
            "This room is already booked for the requested time.".to_string()
        } else if body.contains("login") || body.contains("Login") || body.contains("authenticate") {
            "Authentication required. Please use the `login` tool first.".to_string()
        } else if body.contains("maximum") {
            "You've reached your maximum booking limit (4 hours/day).".to_string()
        } else {
            format!("Server returned status {}. The booking may require different parameters or authentication.", status)
        };

        Ok(BookingResult {
            success: false,
            message: msg,
        })
    }
}

fn extract_csrf_token(html: &str) -> Option<String> {
    // Look for hidden input with name="_token" or "csrf_token"
    let document = Html::parse_document(html);
    if let Ok(sel) = Selector::parse("input[name='_token'], input[name='csrf_token'], meta[name='csrf-token']") {
        if let Some(el) = document.select(&sel).next() {
            return el.value().attr("value").or(el.value().attr("content")).map(|s| s.to_string());
        }
    }
    None
}
