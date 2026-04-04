use std::collections::HashMap;
use std::fmt;

use anyhow::{Context, Result};
use chrono::NaiveDate;
#[cfg(feature = "auth")]
use scraper::{Html, Selector};

const LIBCAL_BASE: &str = "https://calendar.library.ucsc.edu";
const STUDY_ROOMS_GID: u32 = 34977;

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
        lid: 16578,
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

#[cfg(feature = "auth")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BookingResult {
    pub success: bool,
    pub message: String,
}

#[cfg(feature = "auth")]
impl fmt::Display for BookingResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.success {
            write!(f, "**Booking confirmed!** {}", self.message)
        } else {
            write!(f, "**Booking failed.** {}", self.message)
        }
    }
}

// ─── Grid API types ───

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GridSlot {
    start: String,
    end: String,
    item_id: u32,
    #[allow(dead_code)]
    checksum: String,
    #[serde(default)]
    class_name: String,
}

#[derive(Debug, serde::Deserialize)]
struct GridResponse {
    slots: Vec<GridSlot>,
}

// ─── Room metadata from page JS ───

struct RoomMeta {
    eid: u32,
    name: String,
    capacity: Option<u32>,
    lid: u32,
}

/// Extract room metadata from the LibCal spaces page JavaScript.
///
/// The page embeds room data in two forms:
///   resources.push({ eid: 139536, lid: 16577, capacity: 10, ... });
///   resourceNameIdMap["eid_139536"] = "4th\u0020Floor\u0020Room\u00204360";
fn extract_room_metadata(html: &str) -> Vec<RoomMeta> {
    // Step 1: Extract names from resourceNameIdMap["eid_XXXXX"] = "Name"
    let mut names: HashMap<u32, String> = HashMap::new();
    for chunk in html.split("resourceNameIdMap[\"eid_") {
        // chunk starts with: 139536"] = "4th\u0020Floor..."
        let Some(eid_end) = chunk.find("\"]") else {
            continue;
        };
        let Ok(eid) = chunk[..eid_end].parse::<u32>() else {
            continue;
        };
        // Find '= "' after '"]', then extract the name until the closing '"'
        let rest = &chunk[eid_end..];
        let Some(eq_pos) = rest.find("= \"") else {
            continue;
        };
        let name_rest = &rest[eq_pos + 3..]; // skip past '= "'
        let Some(name_end) = name_rest.find('"') else {
            continue;
        };
        let raw_name = &name_rest[..name_end];
        // Decode JS unicode escapes like \u0020
        let name = decode_js_unicode(raw_name);
        if !name.is_empty() {
            names.insert(eid, name);
        }
    }

    // Step 2: Extract eid, lid, capacity from resources.push blocks
    let mut rooms: Vec<RoomMeta> = Vec::new();
    for chunk in html.split("eid: ") {
        // chunk starts with: 139536,\n    gid: 34977,\n    lid: 16577,...capacity: 10,
        let Some(eid_end) = chunk.find(',') else {
            continue;
        };
        let Ok(eid) = chunk[..eid_end].trim().parse::<u32>() else {
            continue;
        };
        // Extract lid
        let lid = extract_field(chunk, "lid: ");
        // Extract capacity
        let capacity = extract_field(chunk, "capacity: ");

        let Some(lid) = lid else { continue };

        let name = names
            .remove(&eid)
            .unwrap_or_else(|| format!("Room {}", eid));

        rooms.push(RoomMeta {
            eid,
            name,
            capacity,
            lid,
        });
    }

    rooms
}

/// Extract a numeric field value from a JS object chunk (e.g., "lid: 16577,")
fn extract_field(text: &str, prefix: &str) -> Option<u32> {
    let start = text.find(prefix)? + prefix.len();
    let rest = &text[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}

/// Decode JavaScript unicode escapes like \u0020 → ' '
fn decode_js_unicode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some('u') = chars.next() {
                let hex: String = chars.by_ref().take(4).collect();
                if let Ok(code) = u32::from_str_radix(&hex, 16) {
                    if let Some(ch) = char::from_u32(code) {
                        result.push(ch);
                        continue;
                    }
                }
                // Malformed escape — keep raw
                result.push('\\');
                result.push('u');
                result.push_str(&hex);
            } else {
                result.push('\\');
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Extract "HH:MM" from "YYYY-MM-DD HH:MM:SS"
fn format_time(datetime_str: &str) -> String {
    if let Some(time_part) = datetime_str.split_whitespace().nth(1) {
        time_part
            .split(':')
            .take(2)
            .collect::<Vec<_>>()
            .join(":")
    } else {
        datetime_str.to_string()
    }
}

/// Compute the next day in YYYY-MM-DD format (exclusive end for FullCalendar range).
fn next_day(date: &str) -> String {
    if let Ok(d) = NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        (d + chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string()
    } else {
        date.to_string()
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

    // GET the spaces page to extract room metadata from embedded JS
    let spaces_url = format!("{}/spaces?lid={}&d={}", LIBCAL_BASE, lid, date);
    let page_html = client
        .get(&spaces_url)
        .send()
        .await
        .context("Failed to load library spaces page")?
        .text()
        .await
        .context("Failed to read spaces page")?;

    let room_meta = extract_room_metadata(&page_html);

    // POST to the grid API for availability slots
    let grid_url = format!("{}/spaces/availability/grid", LIBCAL_BASE);
    let end_date = next_day(date);

    let grid_resp = client
        .post(&grid_url)
        .header("Referer", &spaces_url)
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&[
            ("lid", lid.to_string()),
            ("gid", STUDY_ROOMS_GID.to_string()),
            ("eid", String::new()),
            ("seat", "0".to_string()),
            ("seatId", "0".to_string()),
            ("zone", String::new()),
            ("filters", String::new()),
            ("start", date.to_string()),
            ("end", end_date),
            ("pageIndex", "0".to_string()),
            ("pageSize", "50".to_string()),
        ])
        .send()
        .await
        .context("Failed to fetch availability grid")?;

    let grid: GridResponse = grid_resp
        .json()
        .await
        .context("Failed to parse availability grid JSON")?;

    // Build metadata lookup
    let meta_map: HashMap<u32, &RoomMeta> = room_meta.iter().map(|m| (m.eid, m)).collect();

    // Group slots by room, filtering to only rooms belonging to this library
    let mut slots_by_room: HashMap<u32, (Vec<TimeSlot>, Vec<TimeSlot>)> = HashMap::new();
    for slot in &grid.slots {
        // Filter by lid: only include rooms belonging to the requested library
        if let Some(meta) = meta_map.get(&slot.item_id) {
            if meta.lid != lid {
                continue;
            }
        }

        let entry = slots_by_room.entry(slot.item_id).or_default();
        let time_slot = TimeSlot {
            start: format_time(&slot.start),
            end: format_time(&slot.end),
        };
        if slot.class_name.is_empty() {
            entry.0.push(time_slot); // available
        } else {
            entry.1.push(time_slot); // booked
        }
    }

    let mut rooms: Vec<Room> = slots_by_room
        .into_iter()
        .map(|(item_id, (available, booked))| {
            let (name, capacity) = meta_map
                .get(&item_id)
                .map(|m| (m.name.clone(), m.capacity))
                .unwrap_or_else(|| (format!("Room {}", item_id), None));
            Room {
                name,
                space_id: Some(item_id),
                capacity,
                available_slots: available,
                booked_slots: booked,
            }
        })
        .collect();

    rooms.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(RoomAvailability {
        library_name: library_name.to_string(),
        date: date.to_string(),
        rooms,
    })
}

#[cfg(feature = "auth")]
pub async fn book_room(
    auth_client: &reqwest::Client,
    space_id: u32,
    date: &str,
    start_time: &str,
    end_time: &str,
) -> Result<BookingResult> {
    // Visit the space page via SAML-aware GET to follow Shibboleth SSO redirects
    // and establish a LibCal session using the IdP cookies from browser login.
    let space_url = format!("{}/space/{}", LIBCAL_BASE, space_id);
    let saml_resp = crate::auth::saml_aware_get(auth_client, &space_url)
        .await
        .context("Failed to load space page for booking")?;

    let page_html = saml_resp.body;

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
        } else if body.contains("login") || body.contains("Login") || body.contains("authenticate")
        {
            "Authentication required. Please use the `login` tool first.".to_string()
        } else if body.contains("maximum") {
            "You've reached your maximum booking limit (4 hours/day).".to_string()
        } else {
            format!(
                "Server returned status {}. The booking may require different parameters or authentication.",
                status
            )
        };

        Ok(BookingResult {
            success: false,
            message: msg,
        })
    }
}

#[cfg(feature = "auth")]
fn extract_csrf_token(html: &str) -> Option<String> {
    // Look for hidden input with name="_token" or "csrf_token"
    let document = Html::parse_document(html);
    if let Ok(sel) =
        Selector::parse("input[name='_token'], input[name='csrf_token'], meta[name='csrf-token']")
    {
        if let Some(el) = document.select(&sel).next() {
            return el
                .value()
                .attr("value")
                .or(el.value().attr("content"))
                .map(|s| s.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_grid_response() {
        let json = r#"{"slots":[
            {"start":"2026-03-31 13:30:00","end":"2026-03-31 14:00:00",
             "itemId":139536,"checksum":"abc","className":""},
            {"start":"2026-03-31 14:00:00","end":"2026-03-31 14:30:00",
             "itemId":139536,"checksum":"def","className":"s-lc-eq-checkout"},
            {"start":"2026-03-31 13:30:00","end":"2026-03-31 14:00:00",
             "itemId":139537,"checksum":"ghi","className":""}
        ]}"#;
        let grid: GridResponse = serde_json::from_str(json).unwrap();
        assert_eq!(grid.slots.len(), 3);
        assert_eq!(grid.slots[0].item_id, 139536);
        assert!(grid.slots[0].class_name.is_empty()); // available
        assert!(!grid.slots[1].class_name.is_empty()); // booked
    }

    #[test]
    fn test_format_time() {
        assert_eq!(format_time("2026-03-31 13:30:00"), "13:30");
        assert_eq!(format_time("2026-03-31 09:00:00"), "09:00");
        assert_eq!(format_time("invalid"), "invalid");
    }

    #[test]
    fn test_next_day() {
        assert_eq!(next_day("2026-03-31"), "2026-04-01");
        assert_eq!(next_day("2026-12-31"), "2027-01-01");
        assert_eq!(next_day("invalid"), "invalid");
    }

    #[test]
    fn test_decode_js_unicode() {
        assert_eq!(
            decode_js_unicode(r"4th\u0020Floor\u0020Room\u00204360"),
            "4th Floor Room 4360"
        );
        assert_eq!(decode_js_unicode("no escapes"), "no escapes");
        assert_eq!(
            decode_js_unicode(r"Science\u0020\u0026\u0020Engineering"),
            "Science & Engineering"
        );
    }

    #[test]
    fn test_extract_room_metadata() {
        let html = r#"
            resources.push({
                id: "eid_139536",
                title: "4th Floor Room 4360 (Capacity 10)",
                url: "/space/139536",
                eid: 139536,
                gid: 34977,
                lid: 16577,
                grouping: "Study Rooms",
                capacity: 10,
            });
            resourceNameIdMap["eid_139536"] = "4th\u0020Floor\u0020Room\u00204360";

            resources.push({
                id: "eid_139537",
                title: "Room 200 (Capacity 6)",
                url: "/space/139537",
                eid: 139537,
                gid: 34977,
                lid: 16578,
                grouping: "Study Rooms",
                capacity: 6,
            });
            resourceNameIdMap["eid_139537"] = "Room\u0020200";
        "#;

        let rooms = extract_room_metadata(html);
        assert_eq!(rooms.len(), 2);

        assert_eq!(rooms[0].eid, 139536);
        assert_eq!(rooms[0].name, "4th Floor Room 4360");
        assert_eq!(rooms[0].capacity, Some(10));
        assert_eq!(rooms[0].lid, 16577);

        assert_eq!(rooms[1].eid, 139537);
        assert_eq!(rooms[1].name, "Room 200");
        assert_eq!(rooms[1].capacity, Some(6));
        assert_eq!(rooms[1].lid, 16578);
    }
}
