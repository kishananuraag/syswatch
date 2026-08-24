use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crate::app::AppState;
use crate::data;

/// Returns Ok(true) if the app should quit.
pub fn handle_events(state: &mut AppState) -> std::io::Result<bool> {
    if let Event::Key(key) = event::read()? {
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(true);
            }
            KeyCode::Char('p') | KeyCode::Char('P') => state.toggle_sort(),
            KeyCode::Char('r') | KeyCode::Char('R') => data::refresh(state),
            _ => {}
        }
    }
    Ok(false)
}
