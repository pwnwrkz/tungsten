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

pub fn progress(current: usize, total: usize, label: &str) {
    use colored::Colorize;

    const BAR_WIDTH: usize = 30;

    let ratio = current as f32 / total as f32;
    let filled = (ratio * BAR_WIDTH as f32).floor() as usize;
    let empty = BAR_WIDTH - filled;
    let percent = (ratio * 100.0).floor() as usize;

    let bar = format!(
        "{}{}",
        "█".repeat(filled).green().bold(),
        "░".repeat(empty).dimmed(),
    );

    let line = format!(
        "[{}] {} {}/{} {:<40}",
        bar,
        format!("{}%", percent).cyan().bold(),
        current.to_string().bold(),
        total.to_string().bold(),
        label.dimmed(),
    );

    if current >= total {
        println!("\r{}", line);
    } else {
        print!("\r{}", line);
        std::io::stdout().flush().unwrap();
    }
}
