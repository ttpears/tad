//! Keyboard event handlers, one per `InputMode`. The main `handle_key`
//! is the global (mode = None) dispatcher; the others handle their
//! respective modals' typing semantics. All five mutate `&mut App` and
//! schedule post-exit dispatch via `app.open_after`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{snooze, tmux};

use super::dispatch::{kill_agent, project_enter_target, rename_agent_window, OpenTarget};
use super::format::short_name;
use super::{App, InputMode, NewSessionField, TextInput, View};

pub(super) fn handle_filter_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let filter_before = app.filter.as_str().to_string();
    match key.code {
        // Navigate the (filtered) list without leaving filter mode — the whole
        // point: see a match, arrow to it, Enter, done.
        KeyCode::Down => move_selection(app, 1),
        KeyCode::Up => move_selection(app, -1),
        KeyCode::PageDown => move_selection(app, 10),
        KeyCode::PageUp => move_selection(app, -10),
        KeyCode::Tab => app.view = app.view.next(),
        KeyCode::BackTab => app.view = app.view.prev(),
        KeyCode::Enter => {
            if let Some(name) = app.selected() {
                let target = match app.view {
                    View::Projects => project_enter_target(&app.data, &name),
                    View::Sessions => Some(OpenTarget::AttachExisting(name)),
                    View::Groups => Some(OpenTarget::Group(name)),
                    View::Hosts => Some(OpenTarget::Host(name)),
                    View::Agents => Some(OpenTarget::JumpToPane(name)),
                };
                if let Some(t) = target {
                    app.open_after = Some(t);
                    app.should_quit = true;
                }
            }
        }
        // Esc clears the filter and exits filter mode in one step.
        KeyCode::Esc => {
            app.filter.clear();
            app.input_mode = InputMode::None;
        }
        // Backspace on an empty filter exits filter mode (mirrors fzf).
        KeyCode::Backspace if app.filter.is_empty() => app.input_mode = InputMode::None,
        KeyCode::Backspace => app.filter.backspace(),
        KeyCode::Delete => app.filter.delete(),
        KeyCode::Left => app.filter.left(),
        KeyCode::Right => app.filter.right(),
        KeyCode::Home => app.filter.home(),
        KeyCode::End => app.filter.end(),
        KeyCode::Char('u') if ctrl => app.filter.clear(),
        KeyCode::Char('a') if ctrl => app.filter.home(),
        KeyCode::Char('e') if ctrl => app.filter.end(),
        KeyCode::Char('c') if ctrl => app.should_quit = true,
        KeyCode::Char(c) if !ctrl => app.filter.insert(c),
        _ => {}
    }
    let len = app.items().len();
    let filter_changed = app.filter.as_str() != filter_before;
    let state = app.list_state_mut();
    if filter_changed {
        // Typing narrows the list — snap to the top match so Enter just works.
        state.select(if len == 0 { None } else { Some(0) });
    } else {
        match state.selected() {
            Some(i) if i >= len => state.select(if len == 0 { None } else { Some(len - 1) }),
            None if len > 0 => state.select(Some(0)),
            _ => {}
        }
    }
}

pub(super) fn handle_rename_agent_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.input_mode = InputMode::None;
            app.rename_agent_text.clear();
            app.rename_agent_target = None;
        }
        KeyCode::Enter => {
            let new_name = app.rename_agent_text.as_str().trim().to_string();
            let target = app.rename_agent_target.take();
            app.rename_agent_text.clear();
            app.input_mode = InputMode::None;
            if let Some(t) = target {
                if !new_name.is_empty() {
                    let _ = rename_agent_window(&t, &new_name);
                    app.refresh();
                }
            }
        }
        KeyCode::Backspace => app.rename_agent_text.backspace(),
        KeyCode::Delete => app.rename_agent_text.delete(),
        KeyCode::Left => app.rename_agent_text.left(),
        KeyCode::Right => app.rename_agent_text.right(),
        KeyCode::Home => app.rename_agent_text.home(),
        KeyCode::End => app.rename_agent_text.end(),
        KeyCode::Char('u') if ctrl => app.rename_agent_text.clear(),
        KeyCode::Char('a') if ctrl => app.rename_agent_text.home(),
        KeyCode::Char('e') if ctrl => app.rename_agent_text.end(),
        KeyCode::Char(c) if !ctrl => app.rename_agent_text.insert(c),
        _ => {}
    }
}

pub(super) fn handle_new_agent_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.input_mode = InputMode::None;
            app.new_agent_prompt.clear();
            app.new_agent_project = None;
        }
        KeyCode::Enter => {
            if let Some(project) = app.new_agent_project.take() {
                let prompt = app.new_agent_prompt.as_str().trim().to_string();
                app.new_agent_prompt.clear();
                app.open_after = Some(OpenTarget::SpawnAgent {
                    project_name: project,
                    prompt: if prompt.is_empty() {
                        None
                    } else {
                        Some(prompt)
                    },
                });
                app.input_mode = InputMode::None;
                app.should_quit = true;
            }
        }
        KeyCode::Backspace => app.new_agent_prompt.backspace(),
        KeyCode::Delete => app.new_agent_prompt.delete(),
        KeyCode::Left => app.new_agent_prompt.left(),
        KeyCode::Right => app.new_agent_prompt.right(),
        KeyCode::Home => app.new_agent_prompt.home(),
        KeyCode::End => app.new_agent_prompt.end(),
        KeyCode::Char('u') if ctrl => app.new_agent_prompt.clear(),
        KeyCode::Char('a') if ctrl => app.new_agent_prompt.home(),
        KeyCode::Char('e') if ctrl => app.new_agent_prompt.end(),
        KeyCode::Char(c) if !ctrl => app.new_agent_prompt.insert(c),
        _ => {}
    }
}

pub(super) fn handle_snooze_key(app: &mut App, key: KeyEvent) {
    // Snooze intervals come from the per-refresh `data.ui` cache
    // (loaded once when AppData was built); no file read per keypress.
    let intervals = app.data.ui.snooze_intervals.clone();
    match key.code {
        KeyCode::Esc => app.input_mode = InputMode::None,
        KeyCode::Up | KeyCode::Char('k') => {
            if app.snooze_cursor > 0 {
                app.snooze_cursor -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.snooze_cursor + 1 < intervals.len() {
                app.snooze_cursor += 1;
            }
        }
        KeyCode::Enter => {
            if let (Some(target), Some(dur)) =
                (app.selected(), intervals.get(app.snooze_cursor).copied())
            {
                let _ = snooze::snooze(&target, dur);
                app.input_mode = InputMode::None;
                // If we got here from --select-agent (the auto-popup),
                // close the dashboard — the user's done responding to
                // the popup and wants to go back to whatever they were
                // doing. Otherwise just dismiss the modal and stay.
                if app.from_popup {
                    app.should_quit = true;
                }
            }
        }
        _ => {}
    }
}

pub(super) fn handle_new_session_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.input_mode = InputMode::None;
            app.new_session_name.clear();
            app.new_session_host.clear();
            app.new_session_field = NewSessionField::Name;
            return;
        }
        KeyCode::Enter => {
            let name = app.new_session_name.as_str().trim().to_string();
            if name.is_empty() {
                return;
            }
            let host = {
                let h = app.new_session_host.as_str().trim();
                if h.is_empty() {
                    None
                } else {
                    Some(h.to_string())
                }
            };
            app.input_mode = InputMode::None;
            app.new_session_name.clear();
            app.new_session_host.clear();
            app.new_session_field = NewSessionField::Name;
            app.open_after = Some(OpenTarget::CreateNew { name, host });
            app.should_quit = true;
            return;
        }
        KeyCode::Tab | KeyCode::Down | KeyCode::BackTab | KeyCode::Up => {
            app.new_session_field = match app.new_session_field {
                NewSessionField::Name => NewSessionField::Host,
                NewSessionField::Host => NewSessionField::Name,
            };
            return;
        }
        _ => {}
    }
    let field = match app.new_session_field {
        NewSessionField::Name => &mut app.new_session_name,
        NewSessionField::Host => &mut app.new_session_host,
    };
    match key.code {
        KeyCode::Left => field.left(),
        KeyCode::Right => field.right(),
        KeyCode::Home => field.home(),
        KeyCode::End => field.end(),
        KeyCode::Backspace => field.backspace(),
        KeyCode::Delete => field.delete(),
        KeyCode::Char('u') if ctrl => field.clear(),
        KeyCode::Char('a') if ctrl => field.home(),
        KeyCode::Char('e') if ctrl => field.end(),
        KeyCode::Char(c) if !ctrl => field.insert(c),
        _ => {}
    }
}

pub(super) fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Tab => app.view = app.view.next(),
        KeyCode::BackTab => app.view = app.view.prev(),
        KeyCode::Char('1') => app.view = View::Projects,
        KeyCode::Char('2') => app.view = View::Sessions,
        KeyCode::Char('3') => app.view = View::Groups,
        KeyCode::Char('4') => app.view = View::Hosts,
        KeyCode::Char('5') => app.view = View::Agents,
        KeyCode::Down | KeyCode::Char('j') => move_selection(app, 1),
        KeyCode::Up | KeyCode::Char('k') => move_selection(app, -1),
        KeyCode::PageDown | KeyCode::Char('J') => move_selection(app, 10),
        KeyCode::PageUp | KeyCode::Char('K') => move_selection(app, -10),
        KeyCode::Home | KeyCode::Char('g') => {
            let items = app.items();
            if !items.is_empty() {
                // First non-header (Agents view has interleaved
                // project headers; other views never do).
                let first = items
                    .iter()
                    .position(|i| !super::is_agent_header(i))
                    .unwrap_or(0);
                app.list_state_mut().select(Some(first));
            }
        }
        KeyCode::End | KeyCode::Char('G') => {
            let items = app.items();
            if !items.is_empty() {
                // Last non-header (Agents view: headers always
                // precede their agents, so the last row is an agent
                // unless the project list is pathological).
                let last = items
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, i)| !super::is_agent_header(i))
                    .map(|(i, _)| i)
                    .unwrap_or(items.len() - 1);
                app.list_state_mut().select(Some(last));
            }
        }
        KeyCode::Enter => {
            if let Some(name) = app.selected() {
                let target = match app.view {
                    View::Projects => project_enter_target(&app.data, &name),
                    View::Sessions => Some(OpenTarget::AttachExisting(name)),
                    View::Groups => Some(OpenTarget::Group(name)),
                    View::Hosts => Some(OpenTarget::Host(name)),
                    View::Agents => Some(OpenTarget::JumpToPane(name)),
                };
                if let Some(t) = target {
                    app.open_after = Some(t);
                    app.should_quit = true;
                }
            }
        }
        KeyCode::Char('d') => {
            // `d` semantics depend on view:
            //   * Sessions → tmux kill-session (heavy: drops every pane
            //     in the session)
            //   * Agents   → SIGINT to the agent's claude PID (gentle:
            //     claude flushes its transcript, pane stays open with
            //     its shell so you can verify what happened)
            match (app.view, app.selected()) {
                (View::Sessions, Some(name)) => {
                    tmux::kill_session(&name);
                    app.refresh();
                }
                (View::Agents, Some(target)) => {
                    if let Some(agent) = app.data.agents.iter().find(|a| a.target == target) {
                        let _ = kill_agent(agent.agent_pid);
                        app.refresh();
                    }
                }
                _ => {}
            }
        }
        KeyCode::Char('R') => {
            // Rename the tmux window containing the selected agent.
            // Only meaningful in the Agents view; in others it's a
            // no-op. Uppercase R so it doesn't collide with `r`
            // (manual refresh).
            if app.view == View::Agents {
                if let Some(target) = app.selected() {
                    if let Some(agent) = app.data.agents.iter().find(|a| a.target == target) {
                        app.rename_agent_target = Some(target.clone());
                        // Prefill with the current window name (pristine
                        // so the first keystroke replaces it cleanly,
                        // same UX as Hosts-view's `n`-prefilled SSH).
                        app.rename_agent_text = TextInput::pristine(agent.window_name.clone());
                        app.input_mode = InputMode::RenameAgent;
                    }
                }
            }
        }
        KeyCode::Char('s') => {
            // Snooze the selected agent. Only meaningful in the Agents
            // view — in others it's a no-op.
            if app.view == View::Agents && app.selected().is_some() {
                app.snooze_cursor = 0;
                app.input_mode = InputMode::SnoozeSelect;
            }
        }
        KeyCode::Char('S') => {
            // Clear an active snooze on the selected agent. Useful if you
            // snoozed the wrong row or changed your mind.
            if app.view == View::Agents {
                if let Some(target) = app.selected() {
                    let _ = snooze::clear(&target);
                }
            }
        }
        KeyCode::Char('/') => {
            app.input_mode = InputMode::Filter;
            app.filter.clear();
            let len = app.items().len();
            if len > 0 {
                app.list_state_mut().select(Some(0));
            }
        }
        KeyCode::Char('n') => {
            // `n` semantics depend on which view you're in:
            //   * Projects → spawn a new claude agent inside the selected
            //     project (optional initial prompt modal)
            //   * Hosts    → new tmux session prefilled with the host as
            //     the SSH target
            //   * others   → blank new tmux session
            match (app.view, app.selected()) {
                (View::Projects, Some(name)) => {
                    app.new_agent_project = Some(name);
                    app.new_agent_prompt = TextInput::new();
                    app.input_mode = InputMode::NewAgent;
                }
                (View::Hosts, Some(h)) => {
                    app.new_session_name = TextInput::pristine(short_name(&h));
                    app.new_session_host = TextInput::pristine(h);
                    app.new_session_field = NewSessionField::Name;
                    app.input_mode = InputMode::NewSession;
                }
                _ => {
                    app.new_session_name = TextInput::new();
                    app.new_session_host = TextInput::new();
                    app.new_session_field = NewSessionField::Name;
                    app.input_mode = InputMode::NewSession;
                }
            }
        }
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => app.should_quit = true,
        _ => {}
    }
}

fn move_selection(app: &mut App, delta: i32) {
    let items = app.items();
    let len = items.len() as i32;
    if len == 0 {
        return;
    }
    let cur = app.list_state_mut().selected().unwrap_or(0) as i32;
    let mut next = (cur + delta).rem_euclid(len);

    // The Agents view interleaves project-header rows (non-selectable
    // separators) with agent rows. Skip past any header we'd otherwise
    // land on, continuing in the same direction as `delta`. Wrap once;
    // if all rows are headers (shouldn't happen — we don't emit
    // headers for empty projects) we leave the cursor put.
    if app.view == View::Agents {
        let step: i32 = if delta >= 0 { 1 } else { -1 };
        let mut hops = 0;
        while super::is_agent_header(&items[next as usize]) && hops < len {
            next = (next + step).rem_euclid(len);
            hops += 1;
        }
    }
    app.list_state_mut().select(Some(next as usize));
}
