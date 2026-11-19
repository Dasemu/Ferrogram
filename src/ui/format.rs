//! Telegram-Desktop-style time, size and text formatting (pure, tested).

use chrono::{Datelike, FixedOffset, TimeZone};

fn tz(offset_secs: i32) -> FixedOffset {
    use chrono::Offset;
    FixedOffset::east_opt(offset_secs).unwrap_or_else(|| chrono::Utc.fix())
}

fn local(ts: i64, offset: i32) -> Option<chrono::DateTime<FixedOffset>> {
    tz(offset).timestamp_opt(ts, 0).single()
}

/// "14:32" inside bubbles.
pub fn clock(ts: i64, offset: i32) -> String {
    local(ts, offset).map_or_else(String::new, |d| d.format("%H:%M").to_string())
}

/// Chat list time: today → "14:32", last 7 days → "Mon", else "18.11.26".
pub fn list_time(ts: i64, now: i64, offset: i32) -> String {
    let (Some(d), Some(n)) = (local(ts, offset), local(now, offset)) else {
        return String::new();
    };
    if d.date_naive() == n.date_naive() {
        d.format("%H:%M").to_string()
    } else if (n.date_naive() - d.date_naive()).num_days() < 7 && d <= n {
        d.format("%a").to_string()
    } else {
        d.format("%d.%m.%y").to_string()
    }
}

/// Date separator in history: "Today", "Yesterday", "November 12", "March 3, 2024".
pub fn day_separator(ts: i64, now: i64, offset: i32) -> String {
    let (Some(d), Some(n)) = (local(ts, offset), local(now, offset)) else {
        return String::new();
    };
    let days = (n.date_naive() - d.date_naive()).num_days();
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ if d.year() == n.year() => d.format("%B %-d").to_string(),
        _ => d.format("%B %-d, %Y").to_string(),
    }
}

/// Calendar day key for grouping.
pub fn day_key(ts: i64, offset: i32) -> i64 {
    local(ts, offset).map_or(0, |d| i64::from(d.num_days_from_ce()))
}

/// "2.3 MB", "512 KB", "12 B".
pub fn file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.1} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Truncate to `max` chars on one line, adding "…".
pub fn ellipsize(s: &str, max: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let line = s.lines().next().unwrap_or_default();
    if line.graphemes(true).count() <= max && !s.contains('\n') {
        return line.to_owned();
    }
    // Cut on grapheme boundaries so a ZWJ/flag/keycap emoji is never split.
    let mut out: String = line.graphemes(true).take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_owned();
    out.push('…');
    out
}

/// Header status for a user, as Telegram Desktop words it. The flag is true
/// when the text is "online" (drawn in the accent colour).
pub fn presence_text(p: crate::model::Presence, now: i64, offset: i32) -> (String, bool) {
    use crate::model::Presence as P;
    let seen = |was: i64| -> String {
        let ago = now - was;
        if ago < 0 || ago < 60 {
            return "last seen just now".into();
        }
        if ago < 3600 {
            let m = ago / 60;
            return format!("last seen {m} minute{} ago", if m == 1 { "" } else { "s" });
        }
        let (Some(d), Some(n)) = (local(was, offset), local(now, offset)) else {
            return "last seen recently".into();
        };
        match (n.date_naive() - d.date_naive()).num_days() {
            0 => format!("last seen today at {}", d.format("%H:%M")),
            1 => format!("last seen yesterday at {}", d.format("%H:%M")),
            _ => format!("last seen {}", d.format("%d.%m.%y")),
        }
    };
    match p {
        P::Online { until } if until > now => ("online".into(), true),
        P::Online { until } => (seen(until), false),
        P::Offline { was_online } => (seen(was_online), false),
        P::Unknown | P::Recently => ("last seen recently".into(), false),
        P::LastWeek => ("last seen within a week".into(), false),
        P::LastMonth => ("last seen within a month".into(), false),
        P::LongAgo => ("last seen a long time ago".into(), false),
    }
}

/// Badge text: "128", "1.2K".
pub fn badge_count(n: u32) -> String {
    if n >= 10_000 {
        format!("{}K", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}K", f64::from(n) / 1000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MOCK_NOW;

    #[test]
    fn clock_and_list_time() {
        assert_eq!(clock(MOCK_NOW, 0), "14:32");
        assert_eq!(clock(MOCK_NOW, 3600), "15:32");
        assert_eq!(list_time(MOCK_NOW - 60, MOCK_NOW, 0), "14:31");
        assert_eq!(list_time(MOCK_NOW - 86_400, MOCK_NOW, 0), "Tue");
        assert_eq!(list_time(MOCK_NOW - 30 * 86_400, MOCK_NOW, 0), "19.10.26");
    }

    #[test]
    fn separators() {
        assert_eq!(day_separator(MOCK_NOW, MOCK_NOW, 0), "Today");
        assert_eq!(day_separator(MOCK_NOW - 86_400, MOCK_NOW, 0), "Yesterday");
        assert_eq!(
            day_separator(MOCK_NOW - 6 * 86_400, MOCK_NOW, 0),
            "November 12"
        );
        assert_eq!(
            day_separator(MOCK_NOW - 400 * 86_400, MOCK_NOW, 0),
            "October 14, 2025"
        );
        assert_ne!(day_key(MOCK_NOW, 0), day_key(MOCK_NOW - 86_400, 0));
    }

    #[test]
    fn presence_wording() {
        use crate::model::Presence as P;
        let t = |p| presence_text(p, MOCK_NOW, 0);
        assert_eq!(
            t(P::Online {
                until: MOCK_NOW + 60
            }),
            ("online".into(), true)
        );
        assert_eq!(
            t(P::Offline {
                was_online: MOCK_NOW - 20
            })
            .0,
            "last seen just now"
        );
        assert_eq!(
            t(P::Offline {
                was_online: MOCK_NOW - 60
            })
            .0,
            "last seen 1 minute ago"
        );
        assert_eq!(
            t(P::Offline {
                was_online: MOCK_NOW - 600
            })
            .0,
            "last seen 10 minutes ago"
        );
        assert_eq!(
            t(P::Offline {
                was_online: MOCK_NOW - 3 * 3600
            })
            .0,
            "last seen today at 11:32"
        );
        assert_eq!(
            t(P::Offline {
                was_online: MOCK_NOW - 24 * 3600
            })
            .0,
            "last seen yesterday at 14:32"
        );
        assert_eq!(
            t(P::Offline {
                was_online: MOCK_NOW - 9 * 86_400
            })
            .0,
            "last seen 09.11.26"
        );
        // A stale "online" decays into last seen.
        assert_eq!(
            t(P::Online {
                until: MOCK_NOW - 300
            })
            .0,
            "last seen 5 minutes ago"
        );
        assert_eq!(t(P::Unknown).0, "last seen recently");
        assert_eq!(t(P::LastWeek).0, "last seen within a week");
        assert_eq!(t(P::LongAgo).0, "last seen a long time ago");
    }

    #[test]
    fn sizes_and_text() {
        assert_eq!(file_size(12), "12 B");
        assert_eq!(file_size(2_457_600), "2.3 MB");
        assert_eq!(file_size(524_288), "512 KB");
        assert_eq!(ellipsize("hello world", 20), "hello world");
        assert_eq!(ellipsize("hello world", 6), "hello…");
        assert_eq!(ellipsize("a\nb", 10), "a…");
        let fam = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(ellipsize(&format!("{fam}{fam}{fam}"), 2), format!("{fam}…"));
        assert_eq!(badge_count(128), "128");
        assert_eq!(badge_count(1234), "1.2K");
        assert_eq!(badge_count(25_000), "25K");
    }
}
