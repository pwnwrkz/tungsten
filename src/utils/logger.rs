use std::io::Write;

#[macro_export]
macro_rules! log {
    ($level:ident, $($arg:tt)*) => {{
        use colored::Colorize;
        let msg = format!($($arg)*);
        match stringify!($level) {
            "info"    => println!("{} {}", "∙".white().bold(), msg.white()),
            "success" => println!("{} {}", "✓".green().bold(), msg.green()),
            "warn"    => println!("{} {}", "⚠".yellow().bold(), msg.yellow()),
            "error"   => println!("{} {}", "✗".red().bold(), msg.red()),
            "section" => println!("\n{} {}{}", "──".cyan().bold(), msg.cyan().bold(), " ──".cyan().bold()),
            _         => println!("{}", msg),
        }
    }};
}

const BAR_WIDTH: usize = 28;
/// Max width of the phase label column (padded with spaces).
const PHASE_WIDTH: usize = 12;
/// Max width of the item name shown at the right of the bar.
const ITEM_WIDTH: usize = 36;

/// Render a labelled progress bar on the current line (no newline unless done).
///
/// ```text
///   Uploading    [████████░░░░░░░░░░░░░░░░░░░░]  40%   2/5   my_icon.png
/// ```
///
/// When `current >= total` the line is finalized with a newline.
pub fn progress(phase: &str, current: usize, total: usize, item: &str) {
    use colored::Colorize;

    let ratio = if total == 0 {
        1.0f32
    } else {
        current as f32 / total as f32
    };
    let filled = (ratio * BAR_WIDTH as f32).floor() as usize;
    let empty = BAR_WIDTH - filled;
    let percent = (ratio * 100.0).floor() as usize;

    let bar = format!(
        "{}{}",
        "█".repeat(filled).green().bold(),
        "░".repeat(empty).dimmed(),
    );

    // Truncate item name if it's too long.
    let item_display = if item.len() > ITEM_WIDTH {
        format!("…{}", &item[item.len() - (ITEM_WIDTH - 1)..])
    } else {
        item.to_string()
    };

    let line = format!(
        "  {:<phase_w$} [{}] {:>3}%  {}/{} {:<item_w$}",
        phase.cyan().bold(),
        bar,
        format!("{}%", percent).cyan(),
        current.to_string().bold(),
        total.to_string().bold(),
        item_display.dimmed(),
        phase_w = PHASE_WIDTH,
        item_w = ITEM_WIDTH,
    );

    if current >= total {
        // Finalize: overwrite with a clean ✓ line and move to the next line.
        let done_line = format!(
            "  {:<phase_w$} [{}] {}  {}/{}",
            phase.cyan().bold(),
            "█".repeat(BAR_WIDTH).green().bold(),
            "100%".cyan(),
            total.to_string().bold(),
            total.to_string().bold(),
            phase_w = PHASE_WIDTH,
        );
        println!("\r{}", done_line);
    } else {
        print!("\r{}", line);
        std::io::stdout().flush().unwrap();
    }
}

/// Clear the current progress line (call before printing a warn/error mid-progress).
pub fn clear_progress_line() {
    // Overwrite with enough spaces to clear a typical terminal line.
    print!("\r{:<120}\r", "");
    std::io::stdout().flush().unwrap();
}
