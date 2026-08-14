use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{self, ClearType};
use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

pub struct ChecklistItem {
    pub id: String,
    pub label: String,
}

/// Dependency maps: name -> list of dependency names.
pub fn build_repo_dependency_maps(repos: &[crate::repos::RepoEntry]) -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
    let mut requires: HashMap<String, Vec<String>> = HashMap::new();
    let mut required_by: HashMap<String, Vec<String>> = HashMap::new();
    for repo in repos {
        requires.insert(repo.name.clone(), repo.requires.clone());
        required_by.entry(repo.name.clone()).or_default();
    }
    for repo in repos {
        for dep in repo.requires.iter() {
            required_by.entry(dep.clone()).or_default().push(repo.name.clone());
        }
    }
    (requires, required_by)
}

/// Collect transitive dependencies of repoName.
pub fn collect_dependencies(repo_name: &str, requires_by: &HashMap<String, Vec<String>>) -> HashSet<String> {
    let mut into = HashSet::new();
    collect_dependencies_rec(repo_name, requires_by, &mut into);
    into
}

fn collect_dependencies_rec(
    repo_name: &str,
    requires_by: &HashMap<String, Vec<String>>,
    into: &mut HashSet<String>,
) {
    for dep in requires_by.get(repo_name).cloned().unwrap_or_default() {
        if into.insert(dep.clone()) {
            collect_dependencies_rec(&dep, requires_by, into);
        }
    }
}

/// Repos that cannot be deselected because a selected dependent requires them.
pub fn compute_locked_repos(selected: &HashSet<String>, required_by: &HashMap<String, Vec<String>>) -> HashSet<String> {
    let mut locked = HashSet::new();
    for repo in selected {
        if let Some(dependents) = required_by.get(repo) {
            if dependents.iter().any(|d| selected.contains(d)) {
                locked.insert(repo.clone());
            }
        }
    }
    locked
}

fn ensure_interactive() -> Result<(), String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("This command requires an interactive terminal.".to_string());
    }
    Ok(())
}

fn render_checklist(
    title: &str,
    hint: &str,
    items: &[ChecklistItem],
    cursor: usize,
    selected: &HashSet<String>,
    locked: &HashSet<String>,
) -> String {
    let mut lines = vec![String::new(), title.to_string(), hint.to_string(), String::new()];
    for (index, item) in items.iter().enumerate() {
        let pointer = if index == cursor { "❯".to_string() } else { " ".to_string() };
        let mark = if selected.contains(&item.id) { "x" } else { " " };
        let suffix = if locked.contains(&item.id) { " [required]".to_string() } else { String::new() };
        lines.push(format!(" {pointer} [{mark}] {}{suffix}", item.label));
    }
    lines.join("\n")
}

/// Run an interactive arrow-key checklist. Returns the selected ids.
#[allow(clippy::too_many_arguments)]
pub fn run_checklist(
    items: &[ChecklistItem],
    title: &str,
    hint: &str,
    requires_by: Option<&HashMap<String, Vec<String>>>,
    required_by: Option<&HashMap<String, Vec<String>>>,
    min_selected: usize,
    initial_selected: &[String],
    locked_ids: &[String],
) -> Result<Vec<String>, String> {
    ensure_interactive()?;
    let mut cursor = 0usize;
    let mut selected: HashSet<String> = initial_selected.iter().cloned().collect();
    let permanently_locked: HashSet<String> = locked_ids.iter().cloned().collect();
    let mut error = String::new();

    terminal::enable_raw_mode().map_err(|e| format!("raw mode: {e}"))?;
    let result = (|| -> Result<Vec<String>, String> {
        let paint = |cursor: usize, selected: &HashSet<String>, error: &str| -> io::Result<()> {
            let mut locked: HashSet<String> = permanently_locked.clone();
            if let Some(rb) = required_by {
                locked.extend(compute_locked_repos(selected, rb));
            }
            let text = render_checklist(title, hint, items, cursor, selected, &locked);
            let mut out = io::stdout();
            crossterm::execute!(out, cursor::MoveTo(0, 0), terminal::Clear(ClearType::FromCursorDown))?;
            write!(out, "{text}")?;
            if !error.is_empty() {
                write!(out, "\n\n{error}")?;
            }
            out.flush()
        };

        paint(cursor, &selected, &error).map_err(|e| e.to_string())?;
        loop {
            let event = event::poll(Duration::from_millis(200))
                .map_err(|e| e.to_string())?;
            if !event {
                continue;
            }
            let ev = event::read().map_err(|e| e.to_string())?;
            match ev {
                Event::Key(key) => match key.code {
                    KeyCode::Up => {
                        cursor = if cursor == 0 { items.len() - 1 } else { cursor - 1 };
                        error.clear();
                    }
                    KeyCode::Down => {
                        cursor = if cursor == items.len() - 1 { 0 } else { cursor + 1 };
                        error.clear();
                    }
                    KeyCode::Char(' ') | KeyCode::Char('x') | KeyCode::Char('X') => {
                        let id = &items[cursor].id;
                        let mut locked = permanently_locked.clone();
                        if let Some(rb) = required_by {
                            locked.extend(compute_locked_repos(&selected, rb));
                        }
                        if selected.contains(id) {
                            if locked.contains(id) {
                                error = format!("{id} is required by another selected item and cannot be unselected.");
                            } else {
                                selected.remove(id);
                            }
                        } else {
                            selected.insert(id.clone());
                            if let Some(rb) = requires_by {
                                for dep in collect_dependencies(id, rb) {
                                    selected.insert(dep);
                                }
                            }
                        }
                        if error.is_empty() {
                            error.clear();
                        }
                    }
                    KeyCode::Enter => {
                        let result: Vec<String> = items
                            .iter()
                            .filter(|item| selected.contains(&item.id))
                            .map(|item| item.id.clone())
                            .collect();
                        if result.len() < min_selected {
                            error = format!(
                                "At least {min_selected} item{} must be selected.",
                                if min_selected == 1 { "" } else { "s" }
                            );
                        } else {
                            return Ok(result);
                        }
                    }
                    KeyCode::Char('c') => {
                        if let Ok(Event::Key(k)) = crossterm::event::read() {
                            // consume possible modifiers; Ctrl+C is handled by KeyCode::Char('c') with Control modifier
                            let _ = k;
                        }
                        return Err("Cancelled.".to_string());
                    }
                    _ => {}
                },
                _ => {}
            }
            paint(cursor, &selected, &error).map_err(|e| e.to_string())?;
        }
    })();
    let _ = terminal::disable_raw_mode();
    match result {
        Ok(ids) => {
            println!();
            Ok(ids)
        }
        Err(e) => {
            println!();
            Err(e)
        }
    }
}

/// One-key confirm prompt (y/N) with Enter = default.
pub fn confirm_with_single_key(message: &str, default_yes: bool) -> Result<bool, String> {
    ensure_interactive()?;
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("{message} {hint} (Enter = {}): ", if default_yes { "yes" } else { "no" });
    io::stdout().flush().ok();

    terminal::enable_raw_mode().map_err(|e| format!("raw mode: {e}"))?;
    let result = (|| -> Result<bool, String> {
        loop {
            let ev = event::read().map_err(|e| e.to_string())?;
            if let Event::Key(key) = ev {
                match key.code {
                    KeyCode::Char('c') => return Err("Cancelled.".to_string()),
                    KeyCode::Char(c) => match c.to_ascii_lowercase() {
                        'y' => return Ok(true),
                        'n' => return Ok(false),
                        _ => {}
                    },
                    KeyCode::Enter => return Ok(default_yes),
                    _ => {}
                }
            }
        }
    })();
    let _ = terminal::disable_raw_mode();
    let _ = result.clone().map(|_| println!());
    result
}

/// Simple line prompt; returns trimmed answer.
pub fn prompt_line(question: &str) -> Result<String, String> {
    use std::io::BufRead;
    print!("{question}");
    io::stdout().flush().ok();
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim().to_string())
}

use std::collections::HashMap;
