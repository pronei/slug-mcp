use std::collections::HashMap;
use std::fmt::Write;

use anyhow::{Context, Result};
use chrono::NaiveDate;

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
    let q_matcher = crate::util::FuzzyMatcher::new([query]).case_insensitive();
    LIBRARIES.iter().find(|lib| {
        let labels: Vec<&str> = std::iter::once(lib.name)
            .chain(lib.short_names.iter().copied())
            .collect();
        let label_matcher =
            crate::util::FuzzyMatcher::new(labels.iter().copied()).case_insensitive();
        label_matcher.matches(query) || labels.iter().any(|label| q_matcher.matches(label))
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
    /// LibCal library id, used to build the booking-grid deep link.
    pub lid: u32,
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

impl RoomAvailability {
    /// Deep link to the LibCal booking grid for this library + date. Dropping
    /// the student here lets them pick an open slot and check out in-browser
    /// (where LibCal's JS-gated SSO works), which a headless HTTP client can't do.
    pub fn booking_url(&self) -> String {
        format!("{}/spaces?lid={}&d={}", LIBCAL_BASE, self.lid, self.date)
    }

    pub fn format(&self) -> String {
        let mut out = format!("## {} — {}\n", self.library_name, self.date);
        if self.rooms.is_empty() {
            let _ = write!(
                out,
                "No open rooms parsed for this date (the library may be closed or fully booked). \
                 Check the live grid: [📅 LibCal booking →]({})",
                self.booking_url()
            );
            return out;
        }
        // Lead with the one-click booking link for the whole day's grid.
        let _ = writeln!(out, "**[📅 Reserve a room →]({})**", self.booking_url());
        let open: usize = self
            .rooms
            .iter()
            .filter(|r| !r.available_slots.is_empty())
            .count();
        let _ = writeln!(
            out,
            "_{} of {} rooms have open slots. Pick a slot below, then book via the link._",
            open,
            self.rooms.len()
        );
        for room in &self.rooms {
            out.push('\n');
            out.push_str(&room.format());
        }
        out
    }
}

impl Room {
    /// Deep link to this specific room's LibCal page (`/space/<eid>`).
    pub fn booking_url(&self) -> Option<String> {
        self.space_id
            .map(|eid| format!("{}/space/{}", LIBCAL_BASE, eid))
    }

    pub fn format(&self) -> String {
        let cap_str = self
            .capacity
            .map(|c| format!(" (capacity: {})", c))
            .unwrap_or_default();
        // Link the room name to its LibCal page when we have the eid.
        let mut out = match self.booking_url() {
            Some(url) => format!("### [{}]({}){}", self.name, url, cap_str),
            None => format!("### {}{}", self.name, cap_str),
        };

        if !self.available_slots.is_empty() {
            let slots: Vec<String> = self
                .available_slots
                .iter()
                .map(|s| format!("{} - {}", s.start, s.end))
                .collect();
            let _ = write!(out, "\n- **Open**: {}", slots.join(", "));
        }
        if !self.booked_slots.is_empty() {
            let slots: Vec<String> = self
                .booked_slots
                .iter()
                .map(|s| format!("{} - {}", s.start, s.end))
                .collect();
            let _ = write!(out, "\n- **Booked**: {}", slots.join(", "));
        }
        if self.available_slots.is_empty() && self.booked_slots.is_empty() {
            out.push_str("\n- No time slot data available");
        }
        out
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
                if let Ok(code) = u32::from_str_radix(&hex, 16)
                    && let Some(ch) = char::from_u32(code) {
                        result.push(ch);
                        continue;
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
        if let Some(meta) = meta_map.get(&slot.item_id)
            && meta.lid != lid {
                continue;
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
        lid,
        date: date.to_string(),
        rooms,
    })
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

    const AVAILABILITY_GRID_FIXTURE: &str = include_str!("fixtures/availability_grid.json");

    #[test]
    fn test_grid_mixed_booked_available_same_room() {
        let grid: GridResponse = serde_json::from_str(AVAILABILITY_GRID_FIXTURE).unwrap();
        assert_eq!(grid.slots.len(), 7);

        // Replicate scrape_availability's grouping (available = empty className,
        // booked = non-empty) for room 139536, which has both across the day.
        let mut available: Vec<TimeSlot> = Vec::new();
        let mut booked: Vec<TimeSlot> = Vec::new();
        for slot in grid.slots.iter().filter(|s| s.item_id == 139536) {
            let ts = TimeSlot {
                start: format_time(&slot.start),
                end: format_time(&slot.end),
            };
            if slot.class_name.is_empty() {
                available.push(ts);
            } else {
                booked.push(ts);
            }
        }

        // 3 free + 2 booked slots for the same room.
        assert_eq!(available.len(), 3, "available: {:?}", available);
        assert_eq!(booked.len(), 2, "booked: {:?}", booked);
        // times are normalized to HH:MM
        assert_eq!(available[0].start, "09:00");
        assert_eq!(booked[0].start, "09:30");
        assert_eq!(booked[1].start, "10:00");

        // Rendering surfaces both an Available and a Booked line for the room.
        let room = Room {
            name: "4th Floor Room 4360".to_string(),
            space_id: Some(139536),
            capacity: Some(10),
            available_slots: available,
            booked_slots: booked,
        };
        let rendered = room.format();
        assert!(rendered.contains("**Open**: 09:00 - 09:30"));
        assert!(rendered.contains("**Booked**: 09:30 - 10:00"));
        // Room name links to its LibCal deep link.
        assert!(rendered.contains("[4th Floor Room 4360](https://calendar.library.ucsc.edu/space/139536)"));
    }

    #[test]
    fn availability_includes_booking_deep_link() {
        let avail = RoomAvailability {
            library_name: "Science & Engineering Library".to_string(),
            lid: 16578,
            date: "2026-06-15".to_string(),
            rooms: vec![Room {
                name: "Room 3360".to_string(),
                space_id: Some(139549),
                capacity: Some(8),
                available_slots: vec![TimeSlot {
                    start: "13:00".to_string(),
                    end: "13:30".to_string(),
                }],
                booked_slots: vec![],
            }],
        };
        let out = avail.format();
        // Library-level grid deep link carries lid + date.
        assert!(out.contains(
            "(https://calendar.library.ucsc.edu/spaces?lid=16578&d=2026-06-15)"
        ));
        // "Reserve a room" CTA and an open-room count line.
        assert!(out.contains("Reserve a room"));
        assert!(out.contains("1 of 1 rooms have open slots"));
        assert_eq!(
            avail.booking_url(),
            "https://calendar.library.ucsc.edu/spaces?lid=16578&d=2026-06-15"
        );
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
