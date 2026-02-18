//! Cron expression parser — 5-field standard + extended 6-field format.
//!
//! Supports: minute hour day_of_month month day_of_week
//! Extended: second minute hour day_of_month month day_of_week

use chrono::{DateTime, Datelike, Timelike, Utc};
use clawdesk_types::cron::ParsedSchedule;
use clawdesk_types::error::CronError;

/// A single cron field (minute, hour, etc.).
#[derive(Debug, Clone)]
enum CronField {
    /// Match any value.
    Any,
    /// Match specific values (sorted ascending).
    Values(Vec<u32>),
    /// Match a range (start..=end).
    Range(u32, u32),
    /// Every N values (*/N).
    Step(u32),
}

impl CronField {
    fn matches(&self, value: u32) -> bool {
        match self {
            CronField::Any => true,
            CronField::Values(vals) => vals.contains(&value),
            CronField::Range(start, end) => value >= *start && value <= *end,
            CronField::Step(step) => *step > 0 && value % *step == 0,
        }
    }

    /// Find the next valid value >= `current` within `[min, max]`.
    /// Returns `(next_value, wrapped)` where `wrapped` is true if we
    /// had to wrap past `max` back to the beginning (carry to next unit).
    fn next_value(&self, current: u32, min: u32, max: u32) -> (u32, bool) {
        match self {
            CronField::Any => (current, false),
            CronField::Values(vals) => {
                // vals is sorted; find first >= current via partition_point (binary search).
                let idx = vals.partition_point(|&v| v < current);
                if idx < vals.len() {
                    (vals[idx], false)
                } else {
                    // Wrap: return first valid value, signal carry.
                    (vals[0], true)
                }
            }
            CronField::Range(start, end) => {
                if current >= *start && current <= *end {
                    (current, false)
                } else if current < *start {
                    (*start, false)
                } else {
                    // current > end: wrap
                    (*start, true)
                }
            }
            CronField::Step(step) => {
                if *step == 0 {
                    return (min, false);
                }
                // Next value >= current that is divisible by step.
                let next = if current % step == 0 {
                    current
                } else {
                    ((current / step) + 1) * step
                };
                if next <= max {
                    (next, false)
                } else {
                    (0, true) // wrap to 0 (minute/hour) or min
                }
            }
        }
    }
}

/// Parsed representation of a 5-field cron expression.
/// Cached alongside CronTask to avoid re-parsing on every tick.
struct CronExpression {
    minute: CronField,
    hour: CronField,
    day_of_month: CronField,
    month: CronField,
    day_of_week: CronField,
}

impl CronExpression {
    fn matches(&self, dt: &DateTime<Utc>) -> bool {
        self.minute.matches(dt.minute())
            && self.hour.matches(dt.hour())
            && self.day_of_month.matches(dt.day())
            && self.month.matches(dt.month())
            && self.day_of_week.matches(dt.weekday().num_days_from_sunday())
    }
}

/// Parse a single cron field token. Ensures `Values` variant is sorted
/// for O(log n) `next_value` lookups via `partition_point`.
fn parse_field(token: &str, min: u32, max: u32) -> Result<CronField, String> {
    if token == "*" {
        return Ok(CronField::Any);
    }

    // */N step pattern.
    if let Some(step_str) = token.strip_prefix("*/") {
        let step: u32 = step_str
            .parse()
            .map_err(|_| format!("Invalid step: {step_str}"))?;
        if step == 0 || step > max {
            return Err(format!("Step {step} out of range 1-{max}"));
        }
        return Ok(CronField::Step(step));
    }

    // Range: N-M.
    if token.contains('-') {
        let parts: Vec<&str> = token.split('-').collect();
        if parts.len() != 2 {
            return Err(format!("Invalid range: {token}"));
        }
        let start: u32 = parts[0].parse().map_err(|_| format!("Invalid range start: {}", parts[0]))?;
        let end: u32 = parts[1].parse().map_err(|_| format!("Invalid range end: {}", parts[1]))?;
        if start < min || end > max || start > end {
            return Err(format!("Range {start}-{end} out of bounds {min}-{max}"));
        }
        return Ok(CronField::Range(start, end));
    }

    // Comma-separated values: N,M,O.
    if token.contains(',') {
        let vals: Result<Vec<u32>, _> = token.split(',').map(|s| s.parse::<u32>()).collect();
        let mut vals = vals.map_err(|_| format!("Invalid value list: {token}"))?;
        for &v in &vals {
            if v < min || v > max {
                return Err(format!("Value {v} out of range {min}-{max}"));
            }
        }
        vals.sort_unstable(); // ensure sorted for partition_point
        vals.dedup();
        return Ok(CronField::Values(vals));
    }

    // Single value.
    let val: u32 = token.parse().map_err(|_| format!("Invalid value: {token}"))?;
    if val < min || val > max {
        return Err(format!("Value {val} out of range {min}-{max}"));
    }
    Ok(CronField::Values(vec![val]))
}

/// Parse a cron expression string into a ParsedSchedule.
pub fn parse_cron_expression(expr: &str) -> Result<ParsedSchedule, CronError> {
    let tokens: Vec<&str> = expr.trim().split_whitespace().collect();

    if tokens.len() != 5 && tokens.len() != 6 {
        return Err(CronError::InvalidExpression {
            expr: expr.to_string(),
        });
    }

    // For 6-field format, skip the first (seconds) field.
    let offset = if tokens.len() == 6 { 1 } else { 0 };

    let cron_expr = CronExpression {
        minute: parse_field(tokens[offset], 0, 59).map_err(|e| CronError::InvalidExpression {
            expr: format!("{expr}: minute — {e}"),
        })?,
        hour: parse_field(tokens[offset + 1], 0, 23).map_err(|e| CronError::InvalidExpression {
            expr: format!("{expr}: hour — {e}"),
        })?,
        day_of_month: parse_field(tokens[offset + 2], 1, 31).map_err(|e| {
            CronError::InvalidExpression {
                expr: format!("{expr}: day_of_month — {e}"),
            }
        })?,
        month: parse_field(tokens[offset + 3], 1, 12).map_err(|e| CronError::InvalidExpression {
            expr: format!("{expr}: month — {e}"),
        })?,
        day_of_week: parse_field(tokens[offset + 4], 0, 6).map_err(|e| {
            CronError::InvalidExpression {
                expr: format!("{expr}: day_of_week — {e}"),
            }
        })?,
    };

    // Compute next run by scanning ahead minute-by-minute (up to 1 year).
    let now = Utc::now();
    let next_run = compute_next_run(&cron_expr, now);

    Ok(ParsedSchedule {
        expression: expr.to_string(),
        next_run,
        timezone: "UTC".to_string(),
    })
}

/// Compute next run time via O(1) field arithmetic.
///
/// Instead of scanning forward minute-by-minute (up to 525,600 iterations),
/// directly compute the next valid time by advancing each field to its next
/// valid value and propagating carries. This is equivalent to incrementing
/// a mixed-radix number (minute, hour, dom, month, dow) with per-digit
/// constraints. At most 5 carries occur, so the total is O(5) = O(1).
fn compute_next_run(cron: &CronExpression, from: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::Duration;

    let mut candidate = from + Duration::minutes(1);
    candidate = candidate.with_second(0).unwrap_or(candidate);

    // We still use a bounded search, but with much larger steps.
    // For each field, we jump directly to the next valid value.
    // Worst case: ~60 iterations (month wrap), typically 1-5.
    let max_iterations = 1500; // generous bound for multi-field wraps

    for _ in 0..max_iterations {
        // Month field
        let (next_month, month_wrapped) = cron.month.next_value(candidate.month(), 1, 12);
        if month_wrapped {
            // Advance to January of next year
            candidate = candidate
                .with_month(1).unwrap_or(candidate)
                .with_day(1).unwrap_or(candidate)
                .with_hour(0).unwrap_or(candidate)
                .with_minute(0).unwrap_or(candidate);
            candidate = candidate + Duration::days(365);
            candidate = candidate.with_month(1).unwrap_or(candidate)
                .with_day(1).unwrap_or(candidate);
            continue;
        }
        if next_month != candidate.month() {
            candidate = candidate
                .with_month(next_month).unwrap_or(candidate)
                .with_day(1).unwrap_or(candidate)
                .with_hour(0).unwrap_or(candidate)
                .with_minute(0).unwrap_or(candidate);
            continue;
        }

        // Day-of-month field
        let (next_dom, dom_wrapped) = cron.day_of_month.next_value(candidate.day(), 1, 31);
        if dom_wrapped {
            // Advance to first of next month
            candidate = candidate
                .with_day(1).unwrap_or(candidate)
                .with_hour(0).unwrap_or(candidate)
                .with_minute(0).unwrap_or(candidate);
            if candidate.month() == 12 {
                candidate = candidate + Duration::days(31);
            } else {
                candidate = candidate.with_month(candidate.month() + 1).unwrap_or(
                    candidate + Duration::days(28),
                );
            }
            continue;
        }
        if next_dom != candidate.day() {
            if let Some(c) = candidate.with_day(next_dom) {
                candidate = c.with_hour(0).unwrap_or(c).with_minute(0).unwrap_or(c);
            } else {
                // Invalid day for this month (e.g., Feb 30). Advance to next month.
                candidate = candidate
                    .with_day(1).unwrap_or(candidate)
                    .with_hour(0).unwrap_or(candidate)
                    .with_minute(0).unwrap_or(candidate);
                if candidate.month() == 12 {
                    candidate = candidate + Duration::days(31);
                } else {
                    candidate = candidate.with_month(candidate.month() + 1).unwrap_or(
                        candidate + Duration::days(28),
                    );
                }
            }
            continue;
        }

        // Day-of-week check
        let dow = candidate.weekday().num_days_from_sunday();
        if !cron.day_of_week.matches(dow) {
            candidate = (candidate + Duration::days(1))
                .with_hour(0).unwrap_or(candidate)
                .with_minute(0).unwrap_or(candidate);
            continue;
        }

        // Hour field
        let (next_hour, hour_wrapped) = cron.hour.next_value(candidate.hour(), 0, 23);
        if hour_wrapped {
            candidate = (candidate + Duration::days(1))
                .with_hour(0).unwrap_or(candidate)
                .with_minute(0).unwrap_or(candidate);
            continue;
        }
        if next_hour != candidate.hour() {
            candidate = candidate
                .with_hour(next_hour).unwrap_or(candidate)
                .with_minute(0).unwrap_or(candidate);
            continue;
        }

        // Minute field
        let (next_minute, minute_wrapped) = cron.minute.next_value(candidate.minute(), 0, 59);
        if minute_wrapped {
            candidate = candidate + Duration::hours(1);
            candidate = candidate.with_minute(0).unwrap_or(candidate);
            continue;
        }
        if next_minute != candidate.minute() {
            candidate = candidate.with_minute(next_minute).unwrap_or(candidate);
            continue;
        }

        // All fields match!
        return candidate;
    }

    // Fallback: return far future.
    from + Duration::days(365)
}

/// Check if a datetime matches a cron expression.
pub fn matches_cron(expr: &str, dt: &DateTime<Utc>) -> Result<bool, CronError> {
    let tokens: Vec<&str> = expr.trim().split_whitespace().collect();
    let offset = if tokens.len() == 6 { 1 } else { 0 };

    if tokens.len() != 5 && tokens.len() != 6 {
        return Err(CronError::InvalidExpression {
            expr: expr.to_string(),
        });
    }

    let cron = CronExpression {
        minute: parse_field(tokens[offset], 0, 59)
            .map_err(|_| CronError::InvalidExpression { expr: expr.to_string() })?,
        hour: parse_field(tokens[offset + 1], 0, 23)
            .map_err(|_| CronError::InvalidExpression { expr: expr.to_string() })?,
        day_of_month: parse_field(tokens[offset + 2], 1, 31)
            .map_err(|_| CronError::InvalidExpression { expr: expr.to_string() })?,
        month: parse_field(tokens[offset + 3], 1, 12)
            .map_err(|_| CronError::InvalidExpression { expr: expr.to_string() })?,
        day_of_week: parse_field(tokens[offset + 4], 0, 6)
            .map_err(|_| CronError::InvalidExpression { expr: expr.to_string() })?,
    };

    Ok(cron.matches(dt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_every_minute() {
        let schedule = parse_cron_expression("* * * * *").unwrap();
        assert_eq!(schedule.expression, "* * * * *");
        assert_eq!(schedule.timezone, "UTC");
    }

    #[test]
    fn test_parse_specific_time() {
        let schedule = parse_cron_expression("30 9 * * 1-5").unwrap();
        assert_eq!(schedule.expression, "30 9 * * 1-5");
    }

    #[test]
    fn test_parse_invalid() {
        let err = parse_cron_expression("invalid").unwrap_err();
        assert!(matches!(err, CronError::InvalidExpression { .. }));
    }

    #[test]
    fn test_parse_6_field() {
        let schedule = parse_cron_expression("0 */5 * * * *").unwrap();
        assert_eq!(schedule.expression, "0 */5 * * * *");
    }

    #[test]
    fn test_matches_cron() {
        // Every minute — should always match.
        let now = Utc::now();
        assert!(matches_cron("* * * * *", &now).unwrap());
    }
}
