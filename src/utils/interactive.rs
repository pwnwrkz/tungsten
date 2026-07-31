use crate::log;
use anyhow::Result;
use crossterm::{
    cursor, execute,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use inquire::{Confirm, Select, Text};
use std::io::{Write, stdout};

pub fn input(prompt: &str) -> Result<String> {
    let ans = Text::new(prompt)
        .with_help_message("Press Enter to confirm")
        .prompt()?;
    Ok(ans)
}

pub fn confirm(prompt: &str, default: bool) -> Result<bool> {
    let ans = Confirm::new(prompt).with_default(default).prompt()?;
    Ok(ans)
}

pub fn select(prompt: &str, options: &[&str], default: Option<usize>) -> Result<usize> {
    let mut sel = Select::new(prompt, options.to_vec());
    if let Some(d) = default {
        sel = sel.with_starting_cursor(d);
    }
    let ans = sel.raw_prompt()?;
    Ok(ans.index)
}

pub fn multi_select(prompt: &str, options: &[&str], defaults: &[bool]) -> Result<Vec<usize>> {
    let default_indices: Vec<usize> = defaults
        .iter()
        .enumerate()
        .filter_map(|(i, &checked)| checked.then_some(i))
        .collect();

    let ans = inquire::MultiSelect::new(prompt, options.to_vec())
        .with_default(&default_indices)
        .with_help_message("↑/↓ navigate, Space toggle, Enter confirm")
        .raw_prompt()?;

    Ok(ans.into_iter().map(|opt| opt.index).collect())
}

#[derive(Debug, Clone)]
pub struct FolderSelection {
    pub path: String,
    pub display_name: String,
    pub asset_type: String,
}

const ASSET_TYPES: &[&str] = &["auto", "image", "decal", "model", "audio", "animation"];

#[derive(Debug, Clone)]
pub struct DiscoveredFolder {
    pub path: String,
    pub display_name: String,
    pub suggested_type: String,
    pub asset_counts: AssetCounts,
}

#[derive(Debug, Clone, Default)]
pub struct AssetCounts {
    pub images: usize,
    pub audio: usize,
    pub models: usize,
    pub animations: usize,
    pub other: usize,
}

impl AssetCounts {
    pub fn total(&self) -> usize {
        self.images + self.audio + self.models + self.animations + self.other
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.images > 0 {
            parts.push(format!("{} images", self.images));
        }
        if self.audio > 0 {
            parts.push(format!("{} audio", self.audio));
        }
        if self.models > 0 {
            parts.push(format!("{} models", self.models));
        }
        if self.animations > 0 {
            parts.push(format!("{} animations", self.animations));
        }
        if self.other > 0 {
            parts.push(format!("{} other", self.other));
        }
        if parts.is_empty() {
            "empty".to_string()
        } else {
            parts.join(", ")
        }
    }
}

struct FolderItem {
    folder: DiscoveredFolder,
    selected: bool,
    type_index: usize,
}

pub fn select_folders_with_types(
    prompt: &str,
    folders: &[DiscoveredFolder],
) -> Result<Vec<FolderSelection>> {
    if folders.is_empty() {
        return Ok(Vec::new());
    }

    // Initialize items with suggested types
    let mut items: Vec<FolderItem> = folders
        .iter()
        .map(|f| {
            let type_index = match f.suggested_type.as_str() {
                "image" => 1,
                "decal" => 2,
                "model" => 3,
                "audio" => 4,
                "animation" => 5,
                _ => 0,
            };
            FolderItem {
                folder: f.clone(),
                selected: true, // Default all to selected
                type_index,
            }
        })
        .collect();

    log!(
        info,
        "Starting folder selector with {} folders",
        items.len()
    );
    match run_folder_selector(prompt, &mut items) {
        Ok(()) => {
            log!(info, "Folder selector completed successfully");
        }
        Err(e) => {
            log!(
                warn,
                "Folder selector failed: {}, trying fallback selector",
                e
            );
            if let Err(e2) = run_folder_selector_fallback(prompt, &mut items) {
                log!(
                    warn,
                    "Fallback selector also failed: {}, falling back to all selected",
                    e2
                );
            }
        }
    }

    // Collect selections
    let selections: Vec<FolderSelection> = items
        .into_iter()
        .filter(|item| item.selected)
        .map(|item| FolderSelection {
            path: item.folder.path,
            display_name: item.folder.display_name,
            asset_type: ASSET_TYPES[item.type_index].to_string(),
        })
        .collect();

    log!(info, "Returning {} selected folders", selections.len());
    Ok(selections)
}

fn run_folder_selector(prompt: &str, items: &mut [FolderItem]) -> Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

    let mut selected_index = 0;
    let mut stdout = stdout();

    terminal::enable_raw_mode()?;
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;

    while event::poll(std::time::Duration::from_millis(0))? {
        let _ = event::read();
    }

    let result = loop {
        draw_selector(&mut stdout, prompt, items, selected_index)?;

        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = event::read()?
        {
            if kind != crossterm::event::KeyEventKind::Press {
                continue;
            }
            match code {
                KeyCode::Up => {
                    if selected_index > 0 {
                        selected_index = selected_index.saturating_sub(1);
                    }
                }
                KeyCode::Down => {
                    if selected_index < items.len() - 1 {
                        selected_index += 1;
                    }
                }
                KeyCode::Left => {
                    // Cycle type backward
                    let item = &mut items[selected_index];
                    item.type_index = if item.type_index == 0 {
                        ASSET_TYPES.len() - 1
                    } else {
                        item.type_index - 1
                    };
                }
                KeyCode::Right => {
                    // Cycle type forward
                    let item = &mut items[selected_index];
                    item.type_index = (item.type_index + 1) % ASSET_TYPES.len();
                }
                KeyCode::Char(' ') => {
                    // Toggle selection
                    items[selected_index].selected = !items[selected_index].selected;
                }
                KeyCode::Enter => {
                    break Ok(());
                }
                KeyCode::Esc | KeyCode::Char('q') if modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("Cancelled"));
                }
                KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+A: Select all
                    for item in items.iter_mut() {
                        item.selected = true;
                    }
                }
                KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+D: Deselect all
                    for item in items.iter_mut() {
                        item.selected = false;
                    }
                }
                _ => {}
            }
        }
    };

    execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;

    result
}

/// Fallback folder selector using inquire's MultiSelect (for terminals where crossterm raw mode fails)
fn run_folder_selector_fallback(prompt: &str, items: &mut [FolderItem]) -> Result<()> {
    let folder_options: Vec<String> = items
        .iter()
        .map(|f| {
            format!(
                "{}  [{}]  ({})",
                f.folder.display_name,
                ASSET_TYPES[f.type_index],
                f.folder.asset_counts.summary()
            )
        })
        .collect();

    let option_strs: Vec<&str> = folder_options.iter().map(|s| s.as_str()).collect();
    let defaults: Vec<bool> = vec![true; items.len()];

    let selected_indices = multi_select(prompt, &option_strs, &defaults)?;

    // Reset all to unselected first
    for item in items.iter_mut() {
        item.selected = false;
    }
    // Mark selected ones
    for &idx in &selected_indices {
        items[idx].selected = true;
    }

    // For each selected folder, prompt for type
    for &idx in &selected_indices {
        let type_options: Vec<&str> = ASSET_TYPES.to_vec();
        let type_prompt = format!("Asset type for '{}':", items[idx].folder.display_name);
        let default_type = items[idx].type_index;
        let selected_type_idx = select(&type_prompt, &type_options, Some(default_type))?;
        items[idx].type_index = selected_type_idx;
    }

    Ok(())
}

fn draw_selector(
    stdout: &mut std::io::Stdout,
    prompt: &str,
    items: &[FolderItem],
    selected_index: usize,
) -> Result<()> {
    execute!(stdout, Clear(ClearType::All), cursor::MoveTo(0, 0))?;

    // Print prompt
    execute!(
        stdout,
        SetForegroundColor(Color::Cyan),
        Print(prompt),
        ResetColor,
        Print("\n\n")
    )?;

    for (i, item) in items.iter().enumerate() {
        let is_current = i == selected_index;

        // Checkbox
        let checkbox = if item.selected { "[✓]" } else { "[ ]" };

        // Type indicator
        let type_str = ASSET_TYPES[item.type_index];

        // Folder display
        let folder_display = &item.folder.display_name;
        let path_display = &item.folder.path;
        let summary = item.folder.asset_counts.summary();

        if is_current {
            execute!(
                stdout,
                SetForegroundColor(Color::Yellow),
                Print("► "),
                ResetColor,
            )?;
        } else {
            execute!(stdout, Print("  "))?;
        }

        // Checkbox
        execute!(stdout, Print(checkbox), Print(" "))?;

        // Folder name
        if is_current {
            execute!(
                stdout,
                SetForegroundColor(Color::White),
                Print(folder_display),
                ResetColor,
            )?;
        } else {
            execute!(stdout, Print(folder_display))?;
        }

        // Path and summary
        execute!(
            stdout,
            SetForegroundColor(Color::DarkGrey),
            Print("  "),
            Print(path_display),
            Print("  ["),
            Print(summary),
            Print("]"),
            ResetColor,
        )?;

        // Type indicator with cycling hint
        execute!(
            stdout,
            Print("  "),
            SetForegroundColor(Color::Cyan),
            Print("["),
            ResetColor,
            SetForegroundColor(Color::Yellow),
            Print(type_str),
            ResetColor,
            SetForegroundColor(Color::Cyan),
            Print("]"),
            ResetColor,
        )?;

        execute!(stdout, Print("\n"))?;
    }

    // Help text
    execute!(
        stdout,
        Print("\n"),
        SetForegroundColor(Color::DarkGrey),
        Print(
            "↑/↓: Navigate  ←/→: Cycle type  Space: Toggle  Enter: Confirm  Ctrl+A: All  Ctrl+D: None"
        ),
        ResetColor,
    )?;

    stdout.flush()?;
    Ok(())
}
