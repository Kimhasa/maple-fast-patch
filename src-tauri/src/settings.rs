//! What the user has set, and the file it is kept in.

use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::desk::tell_app;
use crate::state::Patcher;
use crate::window::show_tray;

/// What the user has set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Settings {
    /// Apply a patch as soon as one is found, without waiting for the button.
    pub(crate) auto_patch: bool,
    /// Say so when the game's maintenance is over or has been extended.
    pub(crate) maintenance_alert: bool,
    /// Start the game by itself once a patch is done and the game can be played.
    pub(crate) auto_start: bool,
    /// Close this app when it has started the game.
    pub(crate) exit_after_start: bool,
    /// Closing the window only puts it away: the app stays at work, next to the clock.
    pub(crate) background: bool,
    /// Replace game files as they are built when the drive has no room to build them all
    /// first. With the room, a patch goes the usual way; the engine decides which it is.
    pub(crate) immediate: bool,
    /// What kind of drive the game is to be taken to be on. No switch sets this: it is a
    /// line in the settings file, for trying out by hand how a patch goes either way.
    #[serde(skip)]
    pub(crate) storage: DriveKind,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            auto_patch: false,
            maintenance_alert: true,
            auto_start: false,
            exit_after_start: false,
            background: false,
            immediate: false,
            storage: DriveKind::Auto,
        }
    }
}

impl Settings {
    /// The switches as the settings file names them, one to a line: `auto-start on`.
    fn switches(&mut self) -> [(&'static str, &mut bool); 6] {
        [
            ("auto-patch", &mut self.auto_patch),
            ("maintenance-alert", &mut self.maintenance_alert),
            ("auto-start", &mut self.auto_start),
            ("exit-after-start", &mut self.exit_after_start),
            ("background", &mut self.background),
            ("immediate", &mut self.immediate),
        ]
    }

    /// What the settings file in `data`, the app's data folder, says. What it does not say
    /// stays as it is without one.
    pub(crate) fn load(data: &Path) -> Settings {
        let mut settings = Settings::default();
        let file = std::fs::read_to_string(data.join(FILE)).ok();
        for line in file.as_deref().unwrap_or_default().lines() {
            for (name, switch) in settings.switches() {
                match line.split_once(' ') {
                    Some((named, "on")) if named == name => *switch = true,
                    Some((named, "off")) if named == name => *switch = false,
                    _ => {}
                }
            }
            if let Some(kind) = line.strip_prefix("storage ").and_then(DriveKind::named) {
                settings.storage = kind;
            }
        }
        settings
    }

    fn save(mut self, data: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(data)?;
        let mut lines: Vec<String> = self
            .switches()
            .into_iter()
            .map(|(name, on)| format!("{name} {}", if *on { "on" } else { "off" }))
            .collect();
        lines.push(format!("storage {}", self.storage.word()));
        std::fs::write(data.join(FILE), lines.join("\n"))
    }
}

/// What the settings file is called.
const FILE: &str = "settings";

/// What kind of drive the engine is to take the game to be on: find out by itself, or be
/// told.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DriveKind {
    #[default]
    Auto,
    Fast,
    Slow,
}

impl DriveKind {
    /// The kind a line of the settings file names: `storage slow`.
    fn named(word: &str) -> Option<DriveKind> {
        match word.trim() {
            "auto" => Some(DriveKind::Auto),
            "fast" => Some(DriveKind::Fast),
            "slow" => Some(DriveKind::Slow),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            DriveKind::Auto => "auto",
            DriveKind::Fast => "fast",
            DriveKind::Slow => "slow",
        }
    }

    pub(crate) fn choice(self) -> mfp_engine::storage::Choice {
        use mfp_engine::storage::Choice;
        match self {
            DriveKind::Auto => Choice::Auto,
            DriveKind::Fast => Choice::Fast,
            DriveKind::Slow => Choice::Slow,
        }
    }
}

#[tauri::command]
pub(crate) fn set_settings(app: AppHandle, patcher: State<Patcher>, settings: Settings) -> Result<(), String> {
    // The screen knows the switches only. What the settings file says beyond them stays.
    let storage = patcher.settings.lock().unwrap().storage;
    let settings = Settings { storage, ..settings };
    settings.save(&patcher.data).map_err(|_| "설정을 저장할 수 없습니다.".to_owned())?;
    let before = std::mem::replace(&mut *patcher.settings.lock().unwrap(), settings);
    if before.background != settings.background {
        show_tray(&app, settings.background);
    }
    // The page shows what is set, and the desks go by it: a patch that was waiting for the
    // user may be the app's to start from here on.
    tell_app(&app, &patcher);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    /// The settings file as the release before the sites wrote it, with nothing changed.
    const AS_INSTALLED: &str = "auto-patch off\nmaintenance-alert on\nauto-start off\nexit-after-start off\n\
                                background off\nimmediate off\nstorage auto";

    #[test]
    fn the_settings_file_is_written_as_before() {
        let scratch = Scratch::new("settings-written");
        // The app's data folder is made by whatever is first written to it.
        let data = scratch.0.join("data");
        Settings::default().save(&data).unwrap();
        assert_eq!(std::fs::read_to_string(data.join("settings")).unwrap(), AS_INSTALLED);
        assert_eq!(Settings::load(&data), Settings::default());
    }

    #[test]
    fn the_settings_file_is_read_as_before() {
        let scratch = Scratch::new("settings-read");
        let file = "auto-patch on\nmaintenance-alert off\nauto-start on\nexit-after-start on\nbackground on\n\
                    immediate on\nstorage slow";
        std::fs::write(scratch.0.join("settings"), file).unwrap();
        let read = Settings::load(&scratch.0);
        let all_on = Settings {
            auto_patch: true,
            maintenance_alert: false,
            auto_start: true,
            exit_after_start: true,
            background: true,
            immediate: true,
            storage: DriveKind::Slow,
        };
        assert_eq!(read, all_on);
        // What was read is written back the same.
        read.save(&scratch.0).unwrap();
        assert_eq!(std::fs::read_to_string(scratch.0.join("settings")).unwrap(), file);
    }

    /// A file from an older release names fewer switches, and one edited by hand may hold
    /// anything. What is not said stays as it is without a file.
    #[test]
    fn what_the_settings_file_does_not_say_stays_as_it_is_without_one() {
        let scratch = Scratch::new("settings-partial");
        assert_eq!(Settings::load(&scratch.0), Settings::default());
        std::fs::write(scratch.0.join("settings"), "auto-start on\r\nbackground maybe\nno such switch\nstorage fast\n")
            .unwrap();
        let read = Settings::load(&scratch.0);
        assert_eq!(read, Settings { auto_start: true, storage: DriveKind::Fast, ..Settings::default() });
    }
}
