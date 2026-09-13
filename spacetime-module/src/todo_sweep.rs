use spacetimedb::{ReducerContext, ScheduleAt, Table};

use crate::{SpaceFile, file_content, space_file};

const ONE_MONTH_DAYS: i64 = 30;
const MICROS_PER_DAY: i64 = 24 * 60 * 60 * 1_000_000;
pub const MONTHLY_MICROS: i64 = ONE_MONTH_DAYS * MICROS_PER_DAY;

#[spacetimedb::table(accessor = todo_sweep_schedule, scheduled(sweep_expired_todos))]
pub struct TodoSweepSchedule {
    #[primary_key]
    #[auto_inc]
    pub scheduled_id: u64,
    pub scheduled_at: ScheduleAt,
}

fn is_workflow_todo(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("Workflows/") else {
        return false;
    };
    let Some((workflow, tail)) = rest.split_once('/') else {
        return false;
    };
    if workflow.is_empty() {
        return false;
    }
    let Some(name) = tail.strip_prefix("status/todos/") else {
        return false;
    };
    !name.is_empty() && !name.contains('/')
}

fn frontmatter(content: &str) -> Option<&str> {
    let body = content.strip_prefix("---\n")?;
    let end = body.find("\n---")?;
    Some(&body[..end])
}

fn created_value(content: &str) -> Option<&str> {
    let block = frontmatter(content)?;
    block.lines().find_map(|line| {
        let value = line.strip_prefix("created:")?;
        Some(value.trim())
    })
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) {
        return None;
    }
    let lengths = [
        31,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day < 1 || day > lengths[(month - 1) as usize] {
        return None;
    }

    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

fn parse_created_days(value: &str) -> Option<i64> {
    let text = value.trim().trim_matches('"').trim_matches('\'');
    let date = text.split_whitespace().next()?;
    let date = date.split(['T', 't']).next()?;

    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }

    days_from_civil(year, month, day)
}

/// Content is passed in rather than read from `file`, which no longer carries
/// it — and that keeps the tests able to call this without a database.
fn should_expire(file: &SpaceFile, content: &str, today_days: i64) -> bool {
    if !is_workflow_todo(&file.path) {
        return false;
    }
    match created_value(content).and_then(parse_created_days) {
        Some(created_days) => today_days - created_days > ONE_MONTH_DAYS,
        None => true,
    }
}

/// Arm the monthly todo sweep on an already-published database, where `init` no
/// longer runs. Idempotent.
#[spacetimedb::reducer]
pub fn arm_todo_sweep_schedule(ctx: &ReducerContext) {
    if ctx.db.todo_sweep_schedule().iter().next().is_some() {
        log::info!("arm_todo_sweep_schedule: already armed");
        return;
    }
    ctx.db.todo_sweep_schedule().insert(TodoSweepSchedule {
        scheduled_id: 0,
        scheduled_at: spacetimedb::TimeDuration::from_micros(MONTHLY_MICROS).into(),
    });
    log::info!("arm_todo_sweep_schedule: armed monthly sweep");
}

#[spacetimedb::reducer]
pub fn sweep_expired_todos(ctx: &ReducerContext, _schedule: TodoSweepSchedule) {
    let today_days = ctx
        .timestamp
        .to_micros_since_unix_epoch()
        .div_euclid(MICROS_PER_DAY);

    let expired: Vec<(String, String)> = ctx
        .db
        .space_file()
        .iter()
        .filter(|f| should_expire(f, &crate::file_reducers::content_of(ctx, &f.id), today_days))
        .map(|f| (f.id.clone(), f.path.clone()))
        .collect();

    for (id, path) in &expired {
        ctx.db.space_file().id().delete(id);
        // Cascade, same as delete_file — otherwise the swept todo's body stays.
        ctx.db.file_content().file_id().delete(id);
        log::info!("sweep_expired_todos: deleted {} (ID: {})", path, id);
    }

    log::info!("sweep_expired_todos: swept {} todo(s)", expired.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_workflow_todo_notes() {
        assert!(is_workflow_todo("Workflows/spacenotes/status/todos/thing.md"));
        assert!(is_workflow_todo("Workflows/workflow-agent/status/todos/a.md"));

        assert!(!is_workflow_todo("Workflows/spacenotes/status/sessions/x.md"));
        assert!(!is_workflow_todo("Workflows/spacenotes/status/setup/x.md"));
        assert!(!is_workflow_todo("Workflows/spacenotes/knowledge/x.md"));
        assert!(!is_workflow_todo("Personal/status/todos/x.md"));
        assert!(!is_workflow_todo("Workflows/status/todos/x.md"));
        assert!(!is_workflow_todo("Workflows/spacenotes/status/todos/"));
        assert!(!is_workflow_todo(
            "Workflows/spacenotes/status/todos/nested/x.md"
        ));
    }

    #[test]
    fn reads_created_from_frontmatter() {
        let note = "---\ncreated: 2026-08-25\nworkflow: spacenotes\n---\n\n# Thing\n";
        assert_eq!(created_value(note), Some("2026-08-25"));

        assert_eq!(created_value("# No frontmatter\n"), None);
        assert_eq!(created_value("---\nworkflow: x\n---\n"), None);
    }

    #[test]
    fn parses_iso_dates_and_rejects_junk() {
        let epoch = parse_created_days("1970-01-01").unwrap();
        assert_eq!(epoch, 0);
        assert_eq!(parse_created_days("1970-01-02").unwrap(), 1);
        assert_eq!(parse_created_days("2026-08-25").unwrap(), 20690);
        assert_eq!(
            parse_created_days("\"2026-08-25\"").unwrap(),
            parse_created_days("2026-08-25").unwrap()
        );
        assert_eq!(
            parse_created_days("2026-08-25T10:30:00Z").unwrap(),
            parse_created_days("2026-08-25").unwrap()
        );

        assert_eq!(parse_created_days(""), None);
        assert_eq!(parse_created_days("soon"), None);
        assert_eq!(parse_created_days("2026-08"), None);
        assert_eq!(parse_created_days("2026-13-01"), None);
        assert_eq!(parse_created_days("2026-02-30"), None);
        assert_eq!(parse_created_days("2026-08-25-01"), None);
    }

    #[test]
    fn leap_day_is_valid_only_in_a_leap_year() {
        assert!(parse_created_days("2024-02-29").is_some());
        assert_eq!(parse_created_days("2023-02-29"), None);
        assert_eq!(parse_created_days("2100-02-29"), None);
        assert!(parse_created_days("2000-02-29").is_some());
    }

    /// Returns the row and its body separately, since a file's content no
    /// longer lives on the row.
    fn todo(path: &str, content: &str) -> (SpaceFile, String) {
        let file = SpaceFile {
            id: "id".to_string(),
            path: path.to_string(),
            name: "n".to_string(),
            folder_path: "Workflows/spacenotes/status/todos/".to_string(),
            depth: 4,
            extension: "md".to_string(),
            size: 0,
            created_time: 0,
            modified_time: 0,
            db_updated_at: spacetimedb::Timestamp::from_micros_since_unix_epoch(0),
            has_thumbnail: false,
        };
        (file, content.to_string())
    }

    fn expires(todo: &(SpaceFile, String), today: i64) -> bool {
        should_expire(&todo.0, &todo.1, today)
    }

    #[test]
    fn expires_on_age_and_on_a_missing_or_broken_date() {
        let today = parse_created_days("2026-08-25").unwrap();
        let p = "Workflows/spacenotes/status/todos/t.md";

        let fresh = todo(p, "---\ncreated: 2026-08-20\n---\n");
        assert!(!expires(&fresh, today));

        let exactly_a_month = todo(p, "---\ncreated: 2026-07-26\n---\n");
        assert!(!expires(&exactly_a_month, today));

        let old = todo(p, "---\ncreated: 2026-07-01\n---\n");
        assert!(expires(&old, today));

        let undated = todo(p, "---\nworkflow: spacenotes\n---\n");
        assert!(expires(&undated, today));

        let unparseable = todo(p, "---\ncreated: whenever\n---\n");
        assert!(expires(&unparseable, today));

        let no_frontmatter = todo(p, "# Just a todo\n");
        assert!(expires(&no_frontmatter, today));
    }

    #[test]
    fn never_expires_a_note_outside_a_todos_folder() {
        let today = parse_created_days("2026-08-25").unwrap();
        let ancient = "---\ncreated: 2020-01-01\n---\n";

        let session = todo("Workflows/spacenotes/status/sessions/s.md", ancient);
        assert!(!expires(&session, today));

        let knowledge = todo("Workflows/spacenotes/knowledge/k.md", ancient);
        assert!(!expires(&knowledge, today));

        let undated_session = todo("Workflows/spacenotes/status/sessions/s.md", "# x\n");
        assert!(!expires(&undated_session, today));
    }
}
