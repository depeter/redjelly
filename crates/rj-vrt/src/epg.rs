//! The TV guide for one channel and day (`/vrtmax/tv-gids/{slug}/{date}/`).

use serde::Deserialize;
use serde_json::Value;

use crate::models::{self, RawProgress, Tile, TileAction};

/// Unix seconds.
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// `YYYY-MM-DD`, `offset` days from today (local time).
pub fn local_date(offset: i64) -> String {
    (chrono::Local::now() + chrono::Duration::days(offset)).format("%Y-%m-%d").to_string()
}

/// "Vandaag", "Gisteren", "Vrijdag 23/10".
pub fn day_label(offset: i64) -> String {
    const DAYS: [&str; 7] = ["Maandag", "Dinsdag", "Woensdag", "Donderdag", "Vrijdag", "Zaterdag", "Zondag"];
    match offset {
        0 => "Vandaag".into(),
        -1 => "Gisteren".into(),
        1 => "Morgen".into(),
        _ => {
            use chrono::Datelike;
            let d = chrono::Local::now() + chrono::Duration::days(offset);
            format!("{} {}/{}", DAYS[d.weekday().num_days_from_monday() as usize], d.day(), d.month())
        }
    }
}

/// "19:48" in local time.
pub fn clock(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_opt(ts, 0).single().map_or_else(String::new, |t| t.format("%H:%M").to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Programme {
    pub title: String,
    pub description: String,
    /// Unix seconds.
    pub start: i64,
    pub end: i64,
    pub image: Option<String>,
    /// The episode to watch again, when VRT put it online.
    pub vod: Option<String>,
    pub live: bool,
}

impl Programme {
    /// "19:48 – 20:22".
    pub fn times(&self) -> String {
        format!("{} – {}", clock(self.start), clock(self.end))
    }

    pub fn progress(&self, now: i64) -> f32 {
        let span = (self.end - self.start).max(1) as f32;
        ((now - self.start) as f32 / span).clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Guide {
    /// In broadcast order; the one on air has `live` set.
    pub programmes: Vec<Programme>,
}

impl Guide {
    pub fn current(&self) -> Option<&Programme> {
        self.programmes.iter().find(|p| p.live)
    }

    pub fn next(&self) -> Option<&Programme> {
        let i = self.programmes.iter().position(|p| p.live)?;
        self.programmes.get(i + 1)
    }

    /// `data.page` of the guide query. `now` places the programme on air,
    /// whose tile only carries its elapsed time.
    pub(crate) fn parse(page: &Value, now: i64) -> Guide {
        let list = |key: &str| -> Vec<Tile> { models::tiles(&page[key]).0 };
        let mut programmes: Vec<Programme> = Vec::new();
        let mut push = |t: Tile, live: bool, start: Option<i64>| {
            let Some(start) = start.or(t.start) else { return };
            let vod = match t.action() {
                TileAction::Play(link) => Some(link),
                _ => None,
            };
            programmes.push(Programme {
                end: start + i64::from(t.duration.unwrap_or(0)),
                title: t.title,
                description: t.description,
                start,
                image: t.image,
                vod,
                live,
            });
        };
        for t in list("previous") {
            push(t, false, None);
        }
        let current = &page["current"]["tile"];
        if let Some(t) = models::tile(current) {
            let elapsed = RawProgress::deserialize(&current["progress"])
                .ok()
                .and_then(|p| p.progress_in_seconds)
                .unwrap_or(0.0) as i64;
            push(t, true, Some(now - elapsed));
        }
        for t in list("next") {
            push(t, false, None);
        }
        programmes.sort_by_key(|p| p.start);
        // Lengths are rounded to minutes; the next start is exact.
        for i in 1..programmes.len() {
            let next = programmes[i].start;
            let p = &mut programmes[i - 1];
            if p.end <= p.start || (next > p.start && next - p.end < 600) {
                p.end = next;
            }
        }
        Guide { programmes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_with_current() {
        let path = format!("{}/tests/fixtures/epg_vrt1.json", env!("CARGO_MANIFEST_DIR"));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        // The fixture was taken 38 s into "Switch" (19:48 - 20:22).
        let now = 1_792_777_718;
        let g = Guide::parse(&v["data"]["page"], now);
        assert!(g.programmes.len() > 30);
        let cur = g.current().unwrap();
        assert_eq!(cur.title, "Switch");
        assert_eq!(cur.start, now - 38);
        assert!(cur.progress(now) < 0.05);
        assert!(g.next().unwrap().start >= cur.start);
        assert!(g.programmes.windows(2).all(|w| w[0].start <= w[1].start));
        let journaal = g.programmes.iter().find(|p| p.title == "VRT NWS journaal" && p.vod.is_some()).unwrap();
        assert!(journaal.end > journaal.start);
        // Not online (yet): no VOD.
        assert!(g.programmes.iter().any(|p| p.title == "Het weer" && p.vod.is_none()));
    }
}
