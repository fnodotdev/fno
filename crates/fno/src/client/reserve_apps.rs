//! The settings row that edits `agents.reserve_apps`: the apps whose running
//! makes the fleet hold off, so a recording or a stream keeps the machine.
//! One text field, comma-separated, written to the config fno-agents reads.

use super::input_field::{back_row, InputField};
use super::*;

const MAX_INPUT: usize = 256;
const KEY: &str = "agents.reserve_apps";

/// The configured list, read from the mux's cwd ladder.
pub(super) fn current() -> Vec<String> {
    crate::config_defaults::lookup_list(KEY).unwrap_or_else(|| {
        fno_mail::reserve::DEFAULT_APPS
            .iter()
            .map(|s| s.to_string())
            .collect()
    })
}

/// What the row shows beside its label.
pub(super) fn summary() -> String {
    let apps = current();
    if apps.is_empty() {
        return "none".into();
    }
    let plain: Vec<String> = apps
        .iter()
        .take(2)
        .map(|a| match a.split_once('=') {
            Some((name, mode)) if mode.trim().eq_ignore_ascii_case("stream") => {
                format!("{} (streaming)", name.trim())
            }
            Some((name, _)) => name.trim().to_string(),
            None => a.clone(),
        })
        .collect();
    match apps.len().saturating_sub(2) {
        0 => plain.join(", "),
        more => format!("{}, +{more} more", plain.join(", ")),
    }
}

/// The apps a typed line names: comma-separated, blanks dropped.
fn parse(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub(super) fn open(view: &mut View) {
    view.reserve_apps_edit = Some(
        InputField::new("OBS=stream, Final Cut Pro, ...", MAX_INPUT)
            .with_text(&current().join(", ")),
    );
    view.reopen_settings_keeping_sel();
}

pub(super) fn cancel(view: &mut View) -> bool {
    view.reserve_apps_edit.take().is_some()
}

/// The page while the field is open.
pub(super) fn rows(view: &View) -> Option<(Vec<PopupRow>, Vec<AuxAction>)> {
    let field = view.reserve_apps_edit.as_ref()?;
    let inert = |label: &str| PopupRow::Entry {
        glyph: " ".into(),
        label: label.into(),
        hint: String::new(),
        enabled: false,
    };
    let rows = vec![
        back_row(),
        field.row("apps"),
        inert("the fleet holds off while any of these runs"),
        inert("add =stream to hold spawns as well as tests: OBS=stream"),
        inert("enter saves \u{b7} esc cancels"),
    ];
    Some((rows, vec![AuxAction::SettingsBack]))
}

/// Save the typed list. Returns true so the field loop stops feeding.
pub(super) fn submit(view: &mut View, input: String) -> bool {
    view.reserve_apps_edit = None;
    let apps = parse(&input);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let notice = match crate::spawn_floor::set_agents_list("reserve_apps", &apps, &cwd) {
        Ok(path) => format!("{KEY} saved in {}", path.display()),
        Err(e) => format!("apps not saved: {}", e.message),
    };
    view.set_notice(notice);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_on_commas_and_drops_blanks() {
        assert_eq!(
            parse(" OBS=stream, ,Final Cut Pro,"),
            vec!["OBS=stream", "Final Cut Pro"]
        );
        assert!(parse(" , ").is_empty());
    }
}
