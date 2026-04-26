//! Output formatting helpers.

use chrono::{DateTime, Utc};
use colored::{ColoredString, Colorize};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};

use crate::storage::SnapshotRecord;

/// Render a unix timestamp as a short relative phrase like "2 min ago".
pub fn relative_age(ts: i64) -> String {
    let now = Utc::now().timestamp();
    let secs = (now - ts).max(0);
    if secs < 5 {
        return "just now".to_string();
    }
    if secs < 60 {
        return format!("{}s ago", secs);
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{} min ago", mins);
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{}h ago", hours);
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{}d ago", days);
    }
    let dt = DateTime::<Utc>::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "—".to_string());
    dt
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn files_cell(rec: &SnapshotRecord) -> String {
    if rec.clean {
        "—".to_string()
    } else {
        format!("+{}/-{}", rec.files_added, rec.files_deleted)
    }
}

/// Colorize a trigger label so urgent ones (`pre-bash`) stand out.
fn colored_trigger(trigger: &str) -> ColoredString {
    match trigger {
        "pre-bash" => trigger.red().bold(),
        "pre-edit" => trigger.yellow(),
        "manual" => trigger.cyan(),
        "session-start" => trigger.blue(),
        _ => trigger.normal(),
    }
}

/// Render the snapshot list as a table.
pub fn list_table(records: &[SnapshotRecord]) -> String {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["ID", "AGE", "TRIGGER", "FILES", "MESSAGE"]);
    for rec in records.iter().rev() {
        let msg = rec.message.as_deref().unwrap_or("");
        table.add_row(vec![
            Cell::new(rec.id.bold()),
            Cell::new(relative_age(rec.timestamp).dimmed()),
            Cell::new(colored_trigger(&rec.trigger)),
            Cell::new(files_cell(rec)),
            Cell::new(truncate(msg, 60)),
        ]);
    }
    table.to_string()
}

/// One-shot status summary for the `status` subcommand.
pub fn status_summary(records: &[SnapshotRecord], index_bytes: u64) -> String {
    let count = records.len();
    let last = records
        .last()
        .map(|r| {
            format!(
                "{}  {}  {}",
                r.id.bold(),
                relative_age(r.timestamp).dimmed(),
                r.message.as_deref().unwrap_or(&r.trigger),
            )
        })
        .unwrap_or_else(|| "(none)".dimmed().to_string());
    format!(
        "{}: {}\n{}: {}\n{}: {}",
        "snapshots".bold(),
        count,
        "latest".bold(),
        last,
        "index size".bold(),
        format_bytes(index_bytes),
    )
}

fn format_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    if n < K {
        format!("{} B", n as u64)
    } else if n < K * K {
        format!("{:.1} KiB", n / K)
    } else {
        format!("{:.1} MiB", n / (K * K))
    }
}
