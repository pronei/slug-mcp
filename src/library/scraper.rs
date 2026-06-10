use std::collections::HashMap;
use std::fmt::Write;

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
    pub fn format(&self) -> String {
        let mut out = format!("## {} — {}\n", self.library_name, self.date);
        if self.rooms.is_empty() {
            out.push_str("No rooms found or availability data unavailable.");
            return out;
        }
        for room in &self.rooms {
            out.push('\n');
            out.push_str(&room.format());
        }
        out
    }
}

impl Room {
    pub fn format(&self) -> String {
        let cap_str = self
            .capacity
            .map(|c| format!(" (capacity: {})", c))
            .unwrap_or_default();
        let id_str = self
            .space_id
            .map(|id| format!(" [space_id: {}]", id))
            .unwrap_or_default();
        let mut out = format!("### {}{}{}", self.name, cap_str, id_str);

        if !self.available_slots.is_empty() {
            let slots: Vec<String> = self
                .available_slots
                .iter()
                .map(|s| format!("{} - {}", s.start, s.end))
                .collect();
            let _ = write!(out, "\n- **Available**: {}", slots.join(", "));
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

#[cfg(feature = "auth")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BookingResult {
    pub success: bool,
    pub message: String,
}

#[cfg(feature = "auth")]
impl BookingResult {
    pub fn format(&self) -> String {
        if self.success {
            format!("**Booking confirmed!** {}", self.message)
        } else {
            format!("**Booking failed.** {}", self.message)
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
    /// Booking group id (`gid`) — varies per library/category and is required
    /// by the booking endpoints.
    gid: Option<u32>,
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
        // Extract gid (booking group, needed by the booking endpoints)
        let gid = extract_field(chunk, "gid: ");
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
            gid,
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

// ─── Booking flow ───
//
// The real LibCal Spaces booking protocol, reverse-engineered from the
// spaces page JS (June 2026). All requests ride one cookie session:
//
//   1. POST /spaces/availability/booking/add — claim a slot. Requires the
//      slot `checksum` from the availability grid. Returns a pending
//      booking carrying end-time `options` + `optionChecksums`.
//   2. If the requested end is later than the returned end, POST the same
//      endpoint with `update{id, end, checksum: optionChecksums[k]}`.
//   3. POST /ajax/space/times (patron/patronHash blank for SSO sites).
//      Unauthenticated → `{redirect: <libauth SSO url>}`; authenticated →
//      `{html: <patron booking form>}`.
//   4. Follow the redirect through Shibboleth with `saml_aware_get` (the
//      IdP cookies captured at browser login auto-assert), then retry 3.
//   5. Parse the form, fill patron answers (group name etc.), append the
//      hidden `bookings`/`returnUrl`/`method` fields the page JS would
//      add, and POST to the form action.

/// `springyPage.bookingMethod` on the spaces page.
#[cfg(feature = "auth")]
const BOOKING_METHOD: &str = "11";

#[cfg(feature = "auth")]
#[derive(Debug, Clone, serde::Deserialize)]
struct PendingBooking {
    id: u64,
    eid: u64,
    #[serde(default)]
    seat_id: u64,
    gid: u64,
    lid: u64,
    start: String,
    end: String,
    checksum: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default, rename = "optionChecksums")]
    option_checksums: Vec<String>,
}

#[cfg(feature = "auth")]
#[derive(Debug, serde::Deserialize)]
struct BookingAddResponse {
    #[serde(default)]
    bookings: Vec<PendingBooking>,
    #[serde(default)]
    error: Option<String>,
}

#[cfg(feature = "auth")]
#[derive(Debug, serde::Deserialize)]
struct TimesResponse {
    #[serde(default)]
    redirect: Option<String>,
    #[serde(default)]
    html: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// jQuery-style bracket-encoded pairs for the pending bookings array, as
/// `preparePendingBookingsPayload()` would serialize them.
#[cfg(feature = "auth")]
fn booking_context_pairs(bookings: &[PendingBooking]) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for (i, b) in bookings.iter().enumerate() {
        let mut push = |f: &str, v: String| pairs.push((format!("bookings[{i}][{f}]"), v));
        push("id", b.id.to_string());
        push("eid", b.eid.to_string());
        push("seat_id", b.seat_id.to_string());
        push("gid", b.gid.to_string());
        push("lid", b.lid.to_string());
        push("start", b.start.clone());
        push("end", b.end.clone());
        push("checksum", b.checksum.clone());
    }
    pairs
}

/// JSON form of the pending bookings for the hidden `bookings` form field.
#[cfg(feature = "auth")]
fn bookings_json(bookings: &[PendingBooking]) -> String {
    let arr: Vec<serde_json::Value> = bookings
        .iter()
        .map(|b| {
            serde_json::json!({
                "id": b.id,
                "eid": b.eid,
                "seat_id": b.seat_id,
                "gid": b.gid,
                "lid": b.lid,
                "start": b.start,
                "end": b.end,
                "checksum": b.checksum,
            })
        })
        .collect();
    serde_json::Value::Array(arr).to_string()
}

/// Normalize a user-supplied time ("9:00", "09:00", "2:00 PM", "2pm") to "HH:MM".
#[cfg(feature = "auth")]
fn normalize_time(s: &str) -> Option<String> {
    let mut s = s.trim().to_uppercase();
    // chrono needs minutes — expand bare-hour forms ("2 PM", "2PM", "14")
    if !s.contains(':') {
        if let Some(pos) = s.find(|c: char| !c.is_ascii_digit()) {
            if pos > 0 {
                s.insert_str(pos, ":00");
            }
        } else if !s.is_empty() {
            s.push_str(":00");
        }
    }
    let formats = ["%H:%M", "%H:%M:%S", "%I:%M %p", "%I:%M%p"];
    for fmt in formats {
        if let Ok(t) = chrono::NaiveTime::parse_from_str(&s, fmt) {
            return Some(t.format("%H:%M").to_string());
        }
    }
    None
}

/// A parsed patron booking form (the HTML returned by /ajax/space/times).
#[cfg(feature = "auth")]
#[derive(Debug)]
struct BookingForm {
    action: String,
    /// name → value for every submittable field, in document order.
    fields: Vec<(String, String)>,
    /// Names of required fields that are still empty.
    missing_required: Vec<String>,
}

/// Parse the booking form fragment: collect every submittable field with its
/// current value, auto-check agreement checkboxes, pick selected/first
/// options for selects, and fill any field whose name/id/label/placeholder
/// mentions "group" or "nickname" with `group_name`.
#[cfg(feature = "auth")]
fn parse_booking_form(html: &str, group_name: Option<&str>) -> Option<BookingForm> {
    use std::sync::LazyLock;

    static FORM_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("form").expect("hardcoded selector"));
    static FIELD_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("input, select, textarea").expect("hardcoded selector"));
    static LABEL_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("label").expect("hardcoded selector"));
    static OPTION_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("option").expect("hardcoded selector"));

    let document = Html::parse_document(html);

    // Label text by `for` target, used to identify the group-name question.
    let mut labels: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for label in document.select(&LABEL_SEL) {
        if let Some(target) = label.value().attr("for") {
            labels.insert(
                target.to_string(),
                label.text().collect::<String>().to_lowercase(),
            );
        }
    }

    // The /ajax/space/times fragment contains exactly one form; if several,
    // take the one with the most fields.
    let form = document
        .select(&FORM_SEL)
        .max_by_key(|f| f.select(&FIELD_SEL).count())?;

    let mut fields: Vec<(String, String)> = Vec::new();
    let mut missing_required: Vec<String> = Vec::new();

    for el in form.select(&FIELD_SEL) {
        let v = el.value();
        let Some(name) = v.attr("name").map(str::to_string) else {
            continue;
        };
        let id = v.attr("id").unwrap_or_default();
        let label_text = labels.get(id).cloned().unwrap_or_default();
        let placeholder = v.attr("placeholder").unwrap_or_default().to_lowercase();
        let is_group_field = {
            let hay = format!("{} {} {} {}", name.to_lowercase(), id.to_lowercase(), label_text, placeholder);
            hay.contains("group") || hay.contains("nickname")
        };
        let required = v.attr("required").is_some()
            || v.attr("aria-required") == Some("true")
            || v.attr("class").unwrap_or_default().contains("required");

        let mut value = match v.name() {
            "input" => {
                let input_type = v.attr("type").unwrap_or("text");
                match input_type {
                    // Auto-agree: the user initiated this booking; terms
                    // checkboxes gate the submit button in the page JS.
                    "checkbox" => v.attr("value").unwrap_or("1").to_string(),
                    "radio" => {
                        if v.attr("checked").is_some() {
                            v.attr("value").unwrap_or_default().to_string()
                        } else {
                            continue;
                        }
                    }
                    "submit" | "button" | "image" | "reset" => continue,
                    _ => v.attr("value").unwrap_or_default().to_string(),
                }
            }
            "select" => el
                .select(&OPTION_SEL)
                .find(|o| o.value().attr("selected").is_some())
                .or_else(|| el.select(&OPTION_SEL).find(|o| !o.value().attr("value").unwrap_or_default().is_empty()))
                .and_then(|o| o.value().attr("value").map(str::to_string))
                .unwrap_or_default(),
            "textarea" => el.text().collect::<String>().trim().to_string(),
            _ => continue,
        };

        if is_group_field && value.is_empty() {
            if let Some(g) = group_name {
                value = g.to_string();
            }
        }

        if required && value.is_empty() {
            let display = if label_text.is_empty() {
                name.clone()
            } else {
                label_text.trim().to_string()
            };
            missing_required.push(display);
        }

        fields.push((name, value));
    }

    let action = form.value().attr("action").unwrap_or_default().to_string();

    Some(BookingForm {
        action,
        fields,
        missing_required,
    })
}

/// Set or replace a field value by name.
#[cfg(feature = "auth")]
fn upsert_field(fields: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some(f) = fields.iter_mut().find(|(n, _)| n == name) {
        f.1 = value;
    } else {
        fields.push((name.to_string(), value));
    }
}

/// Outcome of claiming a pending hold on a slot.
#[cfg(feature = "auth")]
enum ClaimHold {
    /// A live hold, plus the "HH:MM" start/end actually claimed (may differ
    /// from the request when `flexible` shifted to the nearest open block).
    Held {
        bookings: Vec<PendingBooking>,
        start_hm: String,
        end_hm: String,
    },
    /// LibCal refused — message is user-facing.
    Failed(String),
}

/// Count of 30-minute slots between two "HH:MM" times (end exclusive).
#[cfg(feature = "auth")]
fn slot_span(start_hm: &str, end_hm: &str) -> usize {
    let mins = |hm: &str| -> i32 {
        let (h, m) = hm.split_once(':').unwrap_or((hm, "0"));
        h.parse::<i32>().unwrap_or(0) * 60 + m.parse::<i32>().unwrap_or(0)
    };
    (((mins(end_hm) - mins(start_hm)) / 30).max(1)) as usize
}

/// "HH:MM" plus `slots` × 30 minutes.
#[cfg(feature = "auth")]
fn add_slots(start_hm: &str, slots: usize) -> String {
    let (h, m) = start_hm.split_once(':').unwrap_or((start_hm, "0"));
    let total = h.parse::<i32>().unwrap_or(0) * 60 + m.parse::<i32>().unwrap_or(0) + slots as i32 * 30;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// From a room's grid slots, find the earliest contiguous run of `want`
/// available 30-min slots starting at or after 08:00 (library hours). Returns
/// the raw "YYYY-MM-DD HH:MM:SS" start string of the run's first slot.
#[cfg(feature = "auth")]
fn first_open_run(slots: &[GridSlot], space_id: u32, want: usize) -> Option<String> {
    let mut open: Vec<&GridSlot> = slots
        .iter()
        .filter(|s| {
            s.item_id == space_id && s.class_name.is_empty() && format_time(&s.start).as_str() >= "08:00"
        })
        .collect();
    open.sort_by(|a, b| a.start.cmp(&b.start));
    for i in 0..open.len() {
        let mut end = open[i].end.clone();
        let mut have = 1;
        if have >= want {
            return Some(open[i].start.clone());
        }
        for w in &open[i + 1..] {
            if w.start == end {
                end = w.end.clone();
                have += 1;
                if have >= want {
                    return Some(open[i].start.clone());
                }
            } else {
                break;
            }
        }
    }
    None
}

/// Fetch the availability grid, claim the start slot, and extend to the
/// requested end time. Returns a fresh hold each call — LibCal holds expire
/// within ~1 minute, so this is re-run after any SSO detour rather than reused.
#[cfg(feature = "auth")]
#[allow(clippy::too_many_arguments)]
async fn claim_hold(
    auth_client: &reqwest::Client,
    lid: u32,
    gid: u32,
    space_id: u32,
    date: &str,
    end_date: &str,
    spaces_url: &str,
    start_hm: &str,
    end_hm: &str,
    room_name: &str,
    flexible: bool,
) -> Result<ClaimHold> {
    let grid_url = format!("{}/spaces/availability/grid", LIBCAL_BASE);
    let grid: GridResponse = auth_client
        .post(&grid_url)
        .header("Referer", spaces_url)
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&[
            ("lid", lid.to_string()),
            ("gid", gid.to_string()),
            ("eid", space_id.to_string()),
            ("seat", "0".to_string()),
            ("seatId", "0".to_string()),
            ("zone", String::new()),
            ("filters", String::new()),
            ("start", date.to_string()),
            ("end", end_date.to_string()),
            ("pageIndex", "0".to_string()),
            ("pageSize", "50".to_string()),
        ])
        .send()
        .await
        .context("Failed to fetch availability grid")?
        .json()
        .await
        .context("Failed to parse availability grid JSON")?;

    // Resolve the slot to claim against THIS grid (no read-to-claim gap). If
    // the exact start is taken and `flexible`, shift to the earliest open block
    // of the same duration; otherwise fail with the available starts.
    let want = slot_span(start_hm, end_hm);
    let slot_prefix = format!("{date} {start_hm}");
    let exact = grid
        .slots
        .iter()
        .find(|s| {
            s.item_id == space_id && s.start.starts_with(&slot_prefix) && s.class_name.is_empty()
        })
        .map(|s| (s.start.clone(), s.checksum.clone()));

    let (claim_start_raw, claim_checksum) = match exact {
        Some(pair) => pair,
        None if flexible => match first_open_run(&grid.slots, space_id, want) {
            // Match by start AND item_id — many rooms share a start time, and
            // each slot's checksum is room-specific (a cross-room match yields
            // "Invalid Checksum" at add).
            Some(run_start) => grid
                .slots
                .iter()
                .find(|s| s.start == run_start && s.item_id == space_id)
                .map(|s| (s.start.clone(), s.checksum.clone()))
                .expect("run_start came from this room's grid slots"),
            None => {
                return Ok(ClaimHold::Failed(format!(
                    "No open {}-hour block for {room_name} on {date}.",
                    want as f64 / 2.0
                )));
            }
        },
        None => {
            let available: Vec<&str> = grid
                .slots
                .iter()
                .filter(|s| s.item_id == space_id && s.class_name.is_empty())
                .map(|s| s.start.as_str())
                .collect();
            return Ok(ClaimHold::Failed(if available.is_empty() {
                format!("No available slots for {room_name} (space {space_id}) on {date}.")
            } else {
                format!(
                    "No available slot at {slot_prefix} for {room_name}. Available starts: {}",
                    available.join(", ")
                )
            }));
        }
    };

    // Actual window we're claiming (may differ from the request under flexible).
    let actual_start_hm = format_time(&claim_start_raw);
    let actual_end_hm = add_slots(&actual_start_hm, want);

    // ── Claim the start slot ──
    let add_url = format!("{}/spaces/availability/booking/add", LIBCAL_BASE);
    let add_form: Vec<(String, String)> = vec![
        ("add[eid]".into(), space_id.to_string()),
        ("add[seat_id]".into(), "0".into()),
        ("add[gid]".into(), gid.to_string()),
        ("add[lid]".into(), lid.to_string()),
        ("add[start]".into(), claim_start_raw.clone()),
        ("add[checksum]".into(), claim_checksum.clone()),
        ("lid".into(), lid.to_string()),
        ("gid".into(), gid.to_string()),
        ("start".into(), date.to_string()),
        ("end".into(), end_date.to_string()),
    ];
    // The first authenticated LibCal action in an IdP session can bounce
    // through Shibboleth, so `add` may return an IdP HTML page instead of JSON.
    // Distinguish that (a real SSO page → establish the session and re-POST)
    // from a plain error body like "Invalid Checksum." (surface as-is). Two
    // tries max.
    let mut add_resp: Option<BookingAddResponse> = None;
    for attempt in 0..2 {
        let raw = auth_client
            .post(&add_url)
            .header("Referer", spaces_url)
            .header("X-Requested-With", "XMLHttpRequest")
            .form(&add_form)
            .send()
            .await
            .context("booking/add request failed")?;
        let status = raw.status();
        let final_url = raw.url().clone();
        let text = raw.text().await.context("reading booking/add response")?;
        if let Ok(parsed) = serde_json::from_str::<BookingAddResponse>(&text) {
            add_resp = Some(parsed);
            break;
        }
        let looks_like_sso = text.contains("SAMLResponse")
            || text.contains("Shibboleth")
            || text.contains("_eventId_proceed") // IdP consent page
            || final_url.host_str() == Some("login.ucsc.edu");
        if attempt == 0 && looks_like_sso {
            let landed = crate::auth::saml_continue(
                auth_client,
                crate::auth::SamlResponse { status, final_url, body: text },
            )
            .await
            .context("SSO establishment for LibCal booking failed")?;
            if landed.final_url.host_str() == Some("login.ucsc.edu") {
                return Ok(ClaimHold::Failed(
                    "Shibboleth wants interactive login (IdP session missing or expired). Run the `login` tool, then book again.".to_string(),
                ));
            }
            // Session established — loop to re-POST add.
        } else {
            // Plain error body (e.g. "Invalid Checksum.", "Slot unavailable").
            return Ok(ClaimHold::Failed(format!(
                "LibCal rejected the slot claim: {}",
                crate::util::truncate(&crate::util::strip_html_tags(&text), 200)
            )));
        }
    }
    let add_resp = add_resp.expect("add loop sets Some or returns");
    if let Some(err) = add_resp.error {
        return Ok(ClaimHold::Failed(format!(
            "LibCal rejected the slot: {}",
            crate::util::strip_html_tags(&err)
        )));
    }
    let mut bookings = add_resp.bookings;
    if bookings.is_empty() {
        return Ok(ClaimHold::Failed(
            "LibCal accepted the request but returned no pending booking.".to_string(),
        ));
    }

    // ── Extend the end time if the default hold is shorter ──
    let end_prefix = format!("{date} {actual_end_hm}");
    if !bookings[0].end.starts_with(&end_prefix) {
        let booking = &bookings[0];
        let Some(idx) = booking.options.iter().position(|o| o.starts_with(&end_prefix)) else {
            let opts: Vec<&str> = booking.options.iter().map(String::as_str).collect();
            return Ok(ClaimHold::Failed(format!(
                "End time {actual_end_hm} not offered for this slot. Available end times: {}",
                if opts.is_empty() {
                    bookings[0].end.clone()
                } else {
                    opts.join(", ")
                }
            )));
        };
        let update_checksum = booking
            .option_checksums
            .get(idx)
            .cloned()
            .unwrap_or_else(|| booking.checksum.clone());
        let mut update_form: Vec<(String, String)> = vec![
            ("update[id]".into(), booking.id.to_string()),
            ("update[checksum]".into(), update_checksum),
            ("update[end]".into(), booking.options[idx].clone()),
            ("lid".into(), lid.to_string()),
            ("gid".into(), gid.to_string()),
            ("start".into(), date.to_string()),
            ("end".into(), end_date.to_string()),
        ];
        update_form.append(&mut booking_context_pairs(&bookings));
        let update_resp: BookingAddResponse = auth_client
            .post(&add_url)
            .header("Referer", spaces_url)
            .header("X-Requested-With", "XMLHttpRequest")
            .form(&update_form)
            .send()
            .await
            .context("booking update request failed")?
            .json()
            .await
            .context("booking update returned non-JSON")?;
        if let Some(err) = update_resp.error {
            return Ok(ClaimHold::Failed(format!(
                "Could not extend end time: {}",
                crate::util::strip_html_tags(&err)
            )));
        }
        if !update_resp.bookings.is_empty() {
            bookings = update_resp.bookings;
        }
    }

    Ok(ClaimHold::Held {
        bookings,
        start_hm: actual_start_hm,
        end_hm: actual_end_hm,
    })
}

/// True if a LibCal error/body indicates the pending hold lapsed.
#[cfg(feature = "auth")]
fn is_hold_expired(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("become unavailable") || m.contains("no longer available") || m.contains("expired")
}

/// Re-run `claim_hold`, flattening to `Ok((bookings, start, end))` or
/// `Err(user_message)`.
#[cfg(feature = "auth")]
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
async fn reclaim(
    auth_client: &reqwest::Client,
    lid: u32,
    gid: u32,
    space_id: u32,
    date: &str,
    end_date: &str,
    spaces_url: &str,
    start_hm: &str,
    end_hm: &str,
    room_name: &str,
    flexible: bool,
) -> Result<std::result::Result<(Vec<PendingBooking>, String, String), String>> {
    Ok(match claim_hold(
        auth_client, lid, gid, space_id, date, end_date, spaces_url, start_hm, end_hm, room_name,
        flexible,
    )
    .await?
    {
        ClaimHold::Held {
            bookings,
            start_hm,
            end_hm,
        } => Ok((bookings, start_hm, end_hm)),
        ClaimHold::Failed(message) => Err(message),
    })
}

#[cfg(feature = "auth")]
pub async fn book_room(
    auth_client: &reqwest::Client,
    space_id: u32,
    date: &str,
    start_time: &str,
    end_time: &str,
    group_name: Option<&str>,
    flexible: bool,
) -> Result<BookingResult> {
    let Some(start_hm) = normalize_time(start_time) else {
        return Ok(BookingResult {
            success: false,
            message: format!("Could not parse start time '{start_time}'. Use e.g. \"09:00\" or \"2:00 PM\"."),
        });
    };
    let Some(end_hm) = normalize_time(end_time) else {
        return Ok(BookingResult {
            success: false,
            message: format!("Could not parse end time '{end_time}'. Use e.g. \"10:00\" or \"3:00 PM\"."),
        });
    };

    // ── Locate the room (lid + gid) from the spaces page metadata ──
    let mut room_info: Option<(u32, u32, String)> = None; // (lid, gid, name)
    for lib in LIBRARIES {
        let spaces_url = format!("{}/spaces?lid={}&d={}", LIBCAL_BASE, lib.lid, date);
        let html = auth_client
            .get(&spaces_url)
            .send()
            .await
            .context("Failed to load spaces page")?
            .text()
            .await
            .context("Failed to read spaces page")?;
        if let Some(meta) = extract_room_metadata(&html).into_iter().find(|m| m.eid == space_id) {
            room_info = Some((meta.lid, meta.gid.unwrap_or(STUDY_ROOMS_GID), meta.name));
            break;
        }
    }
    let Some((lid, gid, room_name)) = room_info else {
        return Ok(BookingResult {
            success: false,
            message: format!(
                "Space {space_id} not found at any library. Use get_study_room_availability to list valid space_ids."
            ),
        });
    };

    let spaces_url = format!("{}/spaces?lid={}&d={}", LIBCAL_BASE, lid, date);
    let return_url = format!("/spaces?lid={lid}");

    let end_date = next_day(date);

    // Claim an initial hold up front. If the `times` step later bounces through
    // SSO (first contact with LibCal's Shibboleth SP), the hold will have
    // expired by the time we return — so we re-claim a fresh one per attempt.
    // `actual_*` track the window actually held (flexible may shift it).
    let (mut bookings, mut actual_start, mut actual_end) = match claim_hold(
        auth_client, lid, gid, space_id, date, &end_date, &spaces_url, &start_hm, &end_hm,
        &room_name, flexible,
    )
    .await?
    {
        ClaimHold::Held { bookings, start_hm, end_hm } => (bookings, start_hm, end_hm),
        ClaimHold::Failed(message) => {
            return Ok(BookingResult { success: false, message })
        }
    };

    // ── Submit times, authenticating through Shibboleth on first contact ──
    //
    // LibCal gates checkout behind its own Shibboleth SP. The first booking in
    // an IdP session bounces `ajax/space/times` through SSO — either as a JSON
    // `redirect` or as an inline redirect reqwest follows (leaving us holding an
    // IdP page instead of JSON). Establish the session, then re-claim a fresh
    // hold (the original expires during the detour) and POST times again.
    let times_url = format!("{}/ajax/space/times", LIBCAL_BASE);
    let mut form_html: Option<String> = None;
    let mut authed = false; // have we already completed the SSO handshake?
    // Up to 4 passes: SSO establish, hold re-claim after expiry, and the
    // authenticated retry that returns the form.
    for _ in 0..4 {
        let mut times_form: Vec<(String, String)> = vec![
            ("patron".into(), String::new()),
            ("patronHash".into(), String::new()),
            ("returnUrl".into(), return_url.clone()),
            ("method".into(), BOOKING_METHOD.into()),
        ];
        times_form.append(&mut booking_context_pairs(&bookings));
        let times_raw = auth_client
            .post(&times_url)
            .header("Referer", &spaces_url)
            .header("X-Requested-With", "XMLHttpRequest")
            .form(&times_form)
            .send()
            .await
            .context("ajax/space/times request failed")?;
        let times_status = times_raw.status();
        let times_final = times_raw.url().clone();
        let times_text = times_raw
            .text()
            .await
            .context("reading ajax/space/times response")?;

        // Classify the response. JSON {html}=form (done), JSON {redirect}=follow
        // SSO, JSON {error}/non-JSON content = maybe an expired hold, otherwise
        // a non-JSON body = inline SSO that reqwest already followed.
        let redirect_or_sso: Option<crate::auth::SamlResponse> =
            match serde_json::from_str::<TimesResponse>(&times_text) {
                Ok(resp) => {
                    if let Some(html) = resp.html {
                        form_html = Some(html);
                        break;
                    }
                    if let Some(err) = resp.error {
                        // An expired/stale hold is recoverable: re-claim once.
                        if is_hold_expired(&err) && authed {
                            match reclaim(
                                auth_client, lid, gid, space_id, date, &end_date, &spaces_url,
                                &start_hm, &end_hm, &room_name, flexible,
                            )
                            .await?
                            {
                                Ok((b, s, e)) => {
                                    bookings = b;
                                    actual_start = s;
                                    actual_end = e;
                                    continue;
                                }
                                Err(message) => {
                                    return Ok(BookingResult { success: false, message })
                                }
                            }
                        }
                        return Ok(BookingResult {
                            success: false,
                            message: format!(
                                "LibCal rejected the booking times: {}",
                                crate::util::strip_html_tags(&err)
                            ),
                        });
                    }
                    match resp.redirect {
                        Some(redirect) => {
                            let redirect_abs = reqwest::Url::parse(LIBCAL_BASE)
                                .and_then(|b| b.join(&redirect))
                                .map(|u| u.to_string())
                                .unwrap_or(redirect);
                            Some(
                                crate::auth::saml_aware_get(auth_client, &redirect_abs)
                                    .await
                                    .context("SSO redirect for LibCal failed")?,
                            )
                        }
                        None => {
                            return Ok(BookingResult {
                                success: false,
                                message: "Unexpected response from LibCal (no form, no redirect)."
                                    .to_string(),
                            });
                        }
                    }
                }
                // Non-JSON "Sorry, the dates have become unavailable." = expired
                // hold (a 400 text body, not an SSO page).
                Err(_) if is_hold_expired(&times_text) => {
                    if !authed {
                        return Ok(BookingResult {
                            success: false,
                            message: "LibCal reports the slot is unavailable before authentication — try a different time.".to_string(),
                        });
                    }
                    match reclaim(
                        auth_client, lid, gid, space_id, date, &end_date, &spaces_url, &start_hm,
                        &end_hm, &room_name, flexible,
                    )
                    .await?
                    {
                        Ok((b, s, e)) => {
                            bookings = b;
                            actual_start = s;
                            actual_end = e;
                            continue;
                        }
                        Err(message) => return Ok(BookingResult { success: false, message }),
                    }
                }
                Err(_) => Some(
                    crate::auth::saml_continue(
                        auth_client,
                        crate::auth::SamlResponse {
                            status: times_status,
                            final_url: times_final,
                            body: times_text,
                        },
                    )
                    .await
                    .context("SSO continuation for LibCal failed")?,
                ),
            };

        if let Some(landed) = redirect_or_sso {
            if authed {
                // We already authenticated once and times still bounces.
                return Ok(BookingResult {
                    success: false,
                    message: "LibCal kept bouncing through SSO — the stored session can't authenticate. Run the `login` tool again.".to_string(),
                });
            }
            if landed.final_url.host_str() == Some("login.ucsc.edu") {
                return Ok(BookingResult {
                    success: false,
                    message: "Shibboleth wants interactive login (IdP session missing or expired). Run the `login` tool, then book again.".to_string(),
                });
            }
            tracing::debug!("LibCal booking SSO established at {}", landed.final_url);
            // LibCal's token callback (/spaces/auth?token=…&returnUrl=…) sets the
            // session but finalizes by navigating to returnUrl — which a non-JS
            // client must do explicitly, or the session won't stick for the next
            // AJAX call. Follow it.
            if landed.final_url.path().contains("/spaces/auth") {
                if let Some(ret) = landed
                    .final_url
                    .query_pairs()
                    .find(|(k, _)| k == "returnUrl")
                    .map(|(_, v)| v.into_owned())
                {
                    let ret_abs = reqwest::Url::parse(LIBCAL_BASE)
                        .and_then(|b| b.join(&ret))
                        .map(|u| u.to_string())
                        .unwrap_or_else(|_| format!("{LIBCAL_BASE}{ret}"));
                    let _ = auth_client.get(&ret_abs).send().await;
                }
            }
            authed = true;
            // The session is now live. Reuse the existing hold for the next
            // times POST — it usually survives the ~1s SSO hop; if it expired,
            // the next pass detects "dates unavailable" and re-claims.
        }
    }
    let Some(form_html) = form_html else {
        return Ok(BookingResult {
            success: false,
            message: "Never received the booking form from LibCal after authenticating.".to_string(),
        });
    };

    // ── Step 5: fill and submit the patron form ──
    let Some(mut form) = parse_booking_form(&form_html, group_name) else {
        return Ok(BookingResult {
            success: false,
            message: "Could not find the booking form in LibCal's response.".to_string(),
        });
    };
    if !form.missing_required.is_empty() {
        return Ok(BookingResult {
            success: false,
            message: format!(
                "The booking form has required fields I can't infer: {}. Provide them (e.g. group_name) and retry.",
                form.missing_required.join("; ")
            ),
        });
    }

    upsert_field(&mut form.fields, "bookings", bookings_json(&bookings));
    upsert_field(&mut form.fields, "returnUrl", return_url.clone());
    upsert_field(&mut form.fields, "pickupHolds", String::new());
    upsert_field(&mut form.fields, "method", BOOKING_METHOD.into());

    let action_abs = if form.action.is_empty() {
        format!("{}/ajax/space/book", LIBCAL_BASE)
    } else {
        reqwest::Url::parse(LIBCAL_BASE)
            .and_then(|b| b.join(&form.action))
            .map(|u| u.to_string())
            .unwrap_or(form.action.clone())
    };

    let resp = auth_client
        .post(&action_abs)
        .header("Referer", &spaces_url)
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&form.fields)
        .send()
        .await
        .context("Final booking submit failed")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    // Outcome: JSON {error} | JSON {html: confirmation} | HTML page
    if let Ok(outcome) = serde_json::from_str::<TimesResponse>(&body) {
        if let Some(err) = outcome.error {
            return Ok(BookingResult {
                success: false,
                message: crate::util::strip_html_tags(&err),
            });
        }
        if let Some(html) = outcome.html {
            let text = crate::util::truncate(&crate::util::strip_html_tags(&html), 400);
            return Ok(BookingResult {
                success: true,
                message: format!("{room_name}, {date} {actual_start}–{actual_end}. {text}"),
            });
        }
    }
    let text_lower = body.to_lowercase();
    if status.is_success() && (text_lower.contains("confirm") || text_lower.contains("booking id")) {
        Ok(BookingResult {
            success: true,
            message: format!(
                "{room_name}, {date} {actual_start}–{actual_end}. {}",
                crate::util::truncate(&crate::util::strip_html_tags(&body), 400)
            ),
        })
    } else {
        Ok(BookingResult {
            success: false,
            message: format!(
                "Final submit returned HTTP {status}: {}",
                crate::util::truncate(&crate::util::strip_html_tags(&body), 400)
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "auth")]
    #[test]
    fn test_slot_span_and_add_slots() {
        assert_eq!(slot_span("13:00", "15:00"), 4);
        assert_eq!(slot_span("09:00", "09:30"), 1);
        assert_eq!(slot_span("09:00", "09:00"), 1); // never zero
        assert_eq!(add_slots("13:00", 4), "15:00");
        assert_eq!(add_slots("08:30", 3), "10:00");
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_first_open_run_skips_booked_and_early() {
        let slots: Vec<GridSlot> = serde_json::from_str(
            r#"[
            {"start":"2026-06-12 07:00:00","end":"2026-06-12 07:30:00","itemId":1,"checksum":"a","className":""},
            {"start":"2026-06-12 09:00:00","end":"2026-06-12 09:30:00","itemId":1,"checksum":"b","className":""},
            {"start":"2026-06-12 09:30:00","end":"2026-06-12 10:00:00","itemId":1,"checksum":"c","className":""},
            {"start":"2026-06-12 10:00:00","end":"2026-06-12 10:30:00","itemId":1,"checksum":"d","className":"booked"},
            {"start":"2026-06-12 11:00:00","end":"2026-06-12 11:30:00","itemId":1,"checksum":"e","className":""},
            {"start":"2026-06-12 11:30:00","end":"2026-06-12 12:00:00","itemId":1,"checksum":"f","className":""},
            {"start":"2026-06-12 12:00:00","end":"2026-06-12 12:30:00","itemId":1,"checksum":"g","className":""}
        ]"#,
        )
        .unwrap();
        // Want 3 contiguous: 07:00 is before-hours; 09:00-09:30 is only 2 long
        // (10:00 booked); first 3-run is 11:00-12:30.
        assert_eq!(
            first_open_run(&slots, 1, 3).as_deref(),
            Some("2026-06-12 11:00:00")
        );
        // Want 2: the 09:00-10:00 pair qualifies first.
        assert_eq!(
            first_open_run(&slots, 1, 2).as_deref(),
            Some("2026-06-12 09:00:00")
        );
        // No room 2.
        assert_eq!(first_open_run(&slots, 2, 1), None);
    }

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
        assert!(rendered.contains("**Available**: 09:00 - 09:30"));
        assert!(rendered.contains("**Booked**: 09:30 - 10:00"));
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
        assert_eq!(rooms[0].gid, Some(34977));

        assert_eq!(rooms[1].eid, 139537);
        assert_eq!(rooms[1].name, "Room 200");
        assert_eq!(rooms[1].capacity, Some(6));
        assert_eq!(rooms[1].lid, 16578);
        assert_eq!(rooms[1].gid, Some(34977));
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_normalize_time() {
        assert_eq!(normalize_time("09:00").as_deref(), Some("09:00"));
        assert_eq!(normalize_time("9:00").as_deref(), Some("09:00"));
        assert_eq!(normalize_time("2:00 PM").as_deref(), Some("14:00"));
        assert_eq!(normalize_time("2:30pm").as_deref(), Some("14:30"));
        assert_eq!(normalize_time("2 PM").as_deref(), Some("14:00"));
        assert_eq!(normalize_time("14:00:00").as_deref(), Some("14:00"));
        assert_eq!(normalize_time("not a time"), None);
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_times_response_variants() {
        let r: TimesResponse =
            serde_json::from_str(r#"{"redirect":"/libauth/checkout/123"}"#).unwrap();
        assert_eq!(r.redirect.as_deref(), Some("/libauth/checkout/123"));
        assert!(r.html.is_none());

        let r: TimesResponse = serde_json::from_str(r#"{"html":"<form></form>"}"#).unwrap();
        assert!(r.html.is_some());

        let r: TimesResponse =
            serde_json::from_str(r#"{"error":"Sorry, this exceeds your limit"}"#).unwrap();
        assert!(r.error.is_some());
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_booking_add_response_parse() {
        let json = r#"{
            "bookings": [{
                "id": 1, "eid": 139537, "seat_id": 0, "gid": 34972, "lid": 16578,
                "start": "2026-06-12 09:00:00", "end": "2026-06-12 09:30:00",
                "checksum": "abc123",
                "options": ["2026-06-12 09:30:00", "2026-06-12 10:00:00"],
                "optionChecksums": ["crc1", "crc2"],
                "optionSelected": 0, "cost": 0, "name": "Room 200"
            }],
            "limitIssues": null
        }"#;
        let resp: BookingAddResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.bookings.len(), 1);
        let b = &resp.bookings[0];
        assert_eq!(b.eid, 139537);
        assert_eq!(b.option_checksums, vec!["crc1", "crc2"]);
        assert!(resp.error.is_none());

        // The end-extension lookup: find option matching desired end prefix
        let idx = b.options.iter().position(|o| o.starts_with("2026-06-12 10:00"));
        assert_eq!(idx, Some(1));
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_booking_context_pairs_shape() {
        let b = PendingBooking {
            id: 1,
            eid: 139537,
            seat_id: 0,
            gid: 34972,
            lid: 16578,
            start: "2026-06-12 09:00:00".into(),
            end: "2026-06-12 10:00:00".into(),
            checksum: "abc".into(),
            options: vec![],
            option_checksums: vec![],
        };
        let pairs = booking_context_pairs(std::slice::from_ref(&b));
        assert!(pairs.contains(&("bookings[0][eid]".to_string(), "139537".to_string())));
        assert!(pairs.contains(&("bookings[0][checksum]".to_string(), "abc".to_string())));

        let json = bookings_json(std::slice::from_ref(&b));
        assert!(json.starts_with('['));
        assert!(json.contains("\"checksum\":\"abc\""));
        assert!(json.contains("\"eid\":139537"));
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_parse_booking_form_fills_group_and_terms() {
        let html = r#"
        <div id="s-lc-eq-form">
        <form action="/ajax/space/book" method="post">
            <input type="hidden" name="session" value="se55ion" />
            <input type="text" name="fname" id="fname" value="Sammy" required />
            <input type="text" name="lname" id="lname" value="Slug" required />
            <input type="email" name="email" id="email" value="sammy@ucsc.edu" required />
            <label for="q43210">Group Name</label>
            <input type="text" name="q43210" id="q43210" value="" />
            <select name="q999"><option value="">Choose</option><option value="2-3">2-3 people</option></select>
            <input type="checkbox" name="terms" value="1" required />
            <button type="submit">Submit my Booking</button>
        </form>
        </div>"#;

        let form = parse_booking_form(html, Some("CSE 115A standup")).unwrap();
        assert_eq!(form.action, "/ajax/space/book");
        assert!(form.missing_required.is_empty(), "missing: {:?}", form.missing_required);

        let get = |n: &str| {
            form.fields
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("session"), Some("se55ion"));
        assert_eq!(get("fname"), Some("Sammy"));
        assert_eq!(get("q43210"), Some("CSE 115A standup")); // group filled
        assert_eq!(get("q999"), Some("2-3")); // first non-empty option
        assert_eq!(get("terms"), Some("1")); // auto-agreed
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_parse_booking_form_reports_missing_required() {
        let html = r#"
        <form action="/ajax/space/book">
            <label for="q1">Department</label>
            <input type="text" name="q1" id="q1" value="" required />
        </form>"#;
        let form = parse_booking_form(html, None).unwrap();
        assert_eq!(form.missing_required, vec!["department"]);
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_parse_booking_form_required_group_field_fixture() {
        const FIXTURE: &str = include_str!("fixtures/booking_form_group_required.html");

        // With a group name, the required group field is satisfied.
        let form = parse_booking_form(FIXTURE, Some("CSE 115A Standup")).unwrap();
        assert_eq!(form.action, "/ajax/space/book");
        assert!(
            form.missing_required.is_empty(),
            "missing: {:?}",
            form.missing_required
        );

        let get = |n: &str| {
            form.fields
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, v)| v.as_str())
        };
        // hidden fields carried verbatim
        assert_eq!(get("iid"), Some("998"));
        assert_eq!(get("session"), Some("se55ionABC"));
        // pre-filled patron identity fields
        assert_eq!(get("fname"), Some("Sammy"));
        assert_eq!(get("email"), Some("sammy@ucsc.edu"));
        // required group question filled from group_name
        assert_eq!(get("q12345"), Some("CSE 115A Standup"));
        // first non-empty select option chosen
        assert_eq!(get("q67890"), Some("2-4"));
        // terms checkbox auto-agreed to its value attr
        assert_eq!(get("terms"), Some("agreed"));
        // submit button excluded
        assert!(get("submit").is_none());
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_parse_booking_form_required_group_missing_without_name() {
        const FIXTURE: &str = include_str!("fixtures/booking_form_group_required.html");

        // Without a group name, the required group field is reported missing
        // (by its label text, lowercased).
        let form = parse_booking_form(FIXTURE, None).unwrap();
        assert!(
            form.missing_required
                .iter()
                .any(|m| m.contains("group")),
            "missing: {:?}",
            form.missing_required
        );
    }

    #[cfg(feature = "auth")]
    #[test]
    fn test_upsert_field_replaces() {
        let mut fields = vec![("method".to_string(), "old".to_string())];
        upsert_field(&mut fields, "method", "11".into());
        upsert_field(&mut fields, "bookings", "[]".into());
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].1, "11");
    }
}
