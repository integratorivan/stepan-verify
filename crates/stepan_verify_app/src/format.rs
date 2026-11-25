use chrono::{DateTime, Utc};

/// "5 мин", "3 ч", "2 дн" relative to now.
pub fn ago(t: DateTime<Utc>) -> String {
    let secs = (Utc::now() - t).num_seconds().max(0);
    match secs {
        0..60 => "сейчас".into(),
        60..3600 => format!("{} мин", secs / 60),
        3600..86400 => format!("{} ч", secs / 3600),
        _ => format!("{} дн", secs / 86400),
    }
}
