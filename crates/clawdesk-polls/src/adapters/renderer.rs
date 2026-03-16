//! Poll results renderer — visual tally display.

use crate::vote::VoteTally;

/// Render a text-based bar chart of poll results.
pub fn render_tally(question: &str, options: &[String], tally: &VoteTally, bar_width: usize) -> String {
    let mut lines = Vec::new();
    lines.push(format!("📊 **{question}**"));
    lines.push(String::new());

    let max_count = tally.counts.iter().copied().max().unwrap_or(1).max(1);

    for (i, option) in options.iter().enumerate() {
        let count = tally.counts.get(i).copied().unwrap_or(0);
        let pct = tally.percentages.get(i).copied().unwrap_or(0.0);
        let bar_len = (count as f64 / max_count as f64 * bar_width as f64) as usize;
        let bar = "█".repeat(bar_len);
        let pad = "░".repeat(bar_width.saturating_sub(bar_len));
        lines.push(format!("  {option}"));
        lines.push(format!("  {bar}{pad} {count} ({pct:.0}%)"));
    }

    lines.push(String::new());
    lines.push(format!("Total votes: {}", tally.total_votes));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_basic_tally() {
        let tally = VoteTally {
            counts: vec![5, 3, 2],
            percentages: vec![50.0, 30.0, 20.0],
            total_votes: 10,
        };
        let output = render_tally("Fav?", &["A".into(), "B".into(), "C".into()], &tally, 20);
        assert!(output.contains("Total votes: 10"));
        assert!(output.contains("50%"));
        assert!(output.contains("█"));
    }
}
