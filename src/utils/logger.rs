use std::io::Write;

#[macro_export]
macro_rules! log {
    ($level:ident, $($arg:tt)*) => {{
        use colored::Colorize;
        let msg = format!($($arg)*);
        match stringify!($level) {
            "info"    => println!("{} {}", " INFO    ".on_white().black().bold(), msg.white()),
            "success" => println!("{} {}", " SUCCESS ".on_bright_green().green().bold(), msg.green()),
            "warn"    => println!("{} {}", " WARNING ".on_bright_yellow().yellow().bold(), msg.yellow()),
            "error"   => println!("{} {}", " ERROR   ".on_bright_red().red().bold(), msg.red()),
            "section" => {
                let width = crossterm::terminal::size()
                    .map(|(w, _)| w as usize)
                    .unwrap_or(120);
                let padded = format!(" {}", msg);
                println!("\n{}", format!("{:<width$}", padded, width = width).blue().bold().on_cyan());
            }
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
/// Uploading    [████████░░░░░░░░░░░░░░░░░░░░]  40%   2/5   my_icon.png
/// ```
///
/// When `current >= total` the line is finalized with a newline.
pub fn progress(phase: &str, current: usize, total: usize, item: &str) {
    use colored::Colorize;

    let ratio = if total == 0 {
        0.0f32
    } else {
        current as f32 / total as f32
    };
    let filled = (ratio * BAR_WIDTH as f32).floor() as usize;
    let empty = BAR_WIDTH - filled;
    let percent = (ratio * 100.0).floor() as usize;

    let num_width = total.to_string().len();
    let current_str = format!("{:>width$}", current, width = num_width);
    let total_str = format!("{:>width$}", total, width = num_width);

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
        "{:<phase_w$} [{}] {:>3}%  {}/{}  {:<item_w$}",
        phase.cyan().bold(),
        bar,
        percent.to_string().cyan(),
        current_str.bold(),
        total_str.bold(),
        item_display.dimmed(),
        phase_w = PHASE_WIDTH,
        item_w = ITEM_WIDTH,
    );

    if current >= total {
        // Finalize: overwrite with a clean ✓ line and move to the next line.
        let done_line = format!(
            "{} {:<phase_w$} [{}] {}  {}/{}",
            " SUCCESS ".on_green().black().bold(),
            phase.cyan().bold(),
            "█".repeat(BAR_WIDTH).green().bold(),
            "100%".green(),
            total_str.bold(),
            total_str.bold(),
            phase_w = PHASE_WIDTH,
        );
        println!("\r{}", done_line);
    } else {
        print!("\r{}", line);
        std::io::stdout()
            .flush()
            .expect("Failed to flush stdout during progress update (this may indicate a system I/O error or broken pipe)");
    }
}

/// Clear the current progress line (call before printing a warn/error mid-progress).
pub fn clear_progress_line() {
    // Overwrite with enough spaces to clear a typical terminal line.
    print!("\r{:<120}\r", "");
    std::io::stdout()
        .flush()
        .expect("Failed to flush stdout while clearing progress line (this may indicate a system I/O error or broken pipe)");
}
