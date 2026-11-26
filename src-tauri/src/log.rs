//! The log file, and the word the program leaves there and on the screen when it fails.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

#[cfg(windows)]
use crate::window::message_box;
use crate::wording::drive_letter;

/// The log file may grow to this size. Then it is put aside and a new one begun.
const LOG_MOST: u64 = 1 << 20;

/// Where the log file is, from the moment the app knows its data folder. There is one log
/// for the whole app, and whatever has a line for it finds it here: a run, a watch, and
/// what speaks up when the program fails.
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Says where the log is kept: in `data`, the app's data folder. Said once, as the app
/// starts; nothing is written before.
pub(crate) fn keep_log_in(data: &Path) {
    let _ = LOG_PATH.set(data.join("maple-fast-patch.log"));
    meet(data, Role::Data);
}

/// Opens the log file to add lines to it. One that has grown past [`LOG_MOST`] is first put
/// aside as the one older log that is kept, in place of the one kept before.
pub(crate) fn open_log() -> Option<std::fs::File> {
    let path = LOG_PATH.get()?;
    let dir = path.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > LOG_MOST) {
        let _ = std::fs::rename(path, dir.join("maple-fast-patch.log.1"));
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Makes the program say so when it fails by itself: a line in the log, and one box on the
/// screen. Without this it would only be gone, since the program as it is given out has no
/// console to say anything on.
pub(crate) fn speak_up_on_failure() {
    static SAID: AtomicBool = AtomicBool::new(false);
    let usual = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |failure| {
        usual(failure);
        // Where it happened is a file on the PC the program was built on. Its name and the
        // line are of use; the folders above it are that PC's own and are left out.
        let place = failure.location().map(|at| {
            let file = at.file().rsplit(['/', '\\']).next().unwrap_or_default();
            format!(" at {file}:{}", at.line())
        });
        let message = failure.payload_as_str().unwrap_or("no message");
        // The log is known once the app has started. A failure before that has only the box.
        let log = LOG_PATH.get().and_then(|path| std::fs::OpenOptions::new().append(true).open(path).ok());
        if let Some(mut log) = log {
            write_line(&mut log, &format!("failed: {message}{}", place.unwrap_or_default()));
        }
        // One box, however many threads fail. A thread that fails while the box is up waits
        // here; the program ends when the box of the first one has been read.
        if SAID.swap(true, Ordering::SeqCst) {
            loop {
                std::thread::park();
            }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MB_SETFOREGROUND};
            message_box("프로그램에 문제가 발생하여 종료합니다.", MB_OK | MB_ICONERROR | MB_SETFOREGROUND);
        }
        // The program as it is given out ends by itself after any failure of this kind. A
        // build made for trying things out would only end the thread that failed, and leave
        // a run that is no longer at work marked as running. It ends here instead, as the
        // box says.
        #[cfg(panic = "unwind")]
        {
            std::process::exit(1);
        }
    }));
}

/// What a folder or file of this PC is to the app. The log calls it by that, and never by
/// its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// A client's install folder: `C:<install>`. The drive it is on stays, since which
    /// drive a game is on matters to whoever reads the log for what went wrong.
    Install,
    /// The app's own data folder: `<data>`.
    Data,
}

/// The folders and files the app works with, as far as it has met them since it was
/// started, each with what it is to the app. What [`write_line`] writes is looked through
/// for them.
static PLACES: Mutex<Vec<(PathBuf, Role)>> = Mutex::new(Vec::new());

/// How many places are kept. The app has two install folders and one data folder; the
/// rest is for folders the user picked along the way.
const PLACES_MOST: usize = 24;

fn meet(path: &Path, role: Role) {
    let mut places = PLACES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if places.iter().any(|(known, _)| known == path) {
        return;
    }
    // The oldest that is not the data folder makes room.
    if places.len() >= PLACES_MOST
        && let Some(oldest) = places.iter().position(|(_, role)| *role != Role::Data)
    {
        places.remove(oldest);
    }
    places.push((path.to_path_buf(), role));
}

/// Says that `dir` is a client's install folder: the log names it as that from here on.
pub(crate) fn meet_install(dir: &Path) {
    meet(dir, Role::Install);
}

/// What the log writes in the place of `path`, which is `role` to the app.
fn called(path: &Path, role: Role) -> String {
    match role {
        Role::Install => format!("{}<install>", drive_letter(path).unwrap_or_default()),
        Role::Data => "<data>".to_owned(),
    }
}

/// `places`, each as it is written and as the log calls it, the longest first: a folder
/// inside another one is looked for before the one it is in.
fn named(places: &[(PathBuf, Role)]) -> Vec<(String, String)> {
    let mut named: Vec<(String, String)> =
        places.iter().map(|(path, role)| (written(path), called(path, *role))).collect();
    named.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
    named
}

/// A path as a line has it: without the separator a folder may be written with at its end,
/// which a line that goes on into the folder has only once.
fn written(path: &Path) -> String {
    path.to_string_lossy().trim_end_matches(['\\', '/']).to_owned()
}

/// Adds a line to the log.
pub(crate) fn write_line(log: &mut std::fs::File, text: &str) {
    use std::io::Write;
    let profile = std::env::var_os("USERPROFILE").map(|folder| folder.to_string_lossy().into_owned());
    let places = named(&PLACES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
    let text = without_places(text, &places, profile.as_deref());
    // The line is put together first and handed over in one piece. A run and the rest of
    // the app each have the file open, and a line written in parts could end up with
    // another one in its middle.
    let line = format!("{} {text}\n", local_time());
    let _ = log.write_all(line.as_bytes());
}

/// A path as it stands in a line: as it is, with every backslash doubled, where the engine
/// prints a path as part of a larger value, and with forward slashes.
fn spellings(path: &str) -> [String; 3] {
    [path.replace('\\', "\\\\"), path.replace('\\', "/"), path.to_owned()]
}

/// `text` as it goes into the log: one line, with nothing in it that tells whose PC this
/// is or how its folders are laid out.
///
/// The log is a file people are asked to send along when something went wrong. A path
/// gives away the Windows user name, a folder that another computer shares gives away
/// that computer's name, and neither is anybody's business. So a line names the places
/// the app works with by what they are to it (`places`, each as it is written and as the
/// log calls it, the longest first), and what is left of paths after that is masked: the
/// name of a computer and of what it shares, the user's own folder (`profile`) the way
/// Windows itself abbreviates it, and any other path down to its drive.
///
/// This is done to the finished line, whoever put it together: most paths come in the
/// words of an error, which this app did not write.
fn without_places(text: &str, places: &[(String, String)], profile: Option<&str>) -> String {
    // One event, one line: a file name from a patch file must not be able to make up lines
    // of its own.
    let mut text = text.replace(|c: char| c.is_control(), " ");
    // Windows gives the path a folder really has with a mark in front of it. Without the
    // mark it is the path as it is written everywhere else.
    for (marked, plain) in [(r"\\?\UNC\", r"\\"), (r"\\?\", "")] {
        for doubled in [true, false] {
            let spelt = |path: &str| if doubled { path.replace('\\', "\\\\") } else { path.to_owned() };
            text = replace_where(&text, &spelt(marked), &spelt(plain), &|_| true);
        }
    }
    for (path, called) in places {
        for written in spellings(path) {
            text = replace_where(&text, &written, called, &name_ends);
        }
    }
    text = without_shares(&text);
    if let Some(profile) = profile {
        for written in spellings(profile) {
            text = replace_where(&text, &written, "%USERPROFILE%", &folder_ends);
        }
    }
    without_other_paths(&text)
}

/// Whether a name of a file or folder ends where `after` begins, as opposed to going on
/// there: `C:\Games\Maple` is not the folder that `C:\Games\Maple2\Data` is in.
fn name_ends(after: &str) -> bool {
    let mut next = after.chars();
    match next.next() {
        None => true,
        // The dot that ends a sentence, or the one before the ending of a name.
        Some('.') => !next.next().is_some_and(char::is_alphanumeric),
        // A name can have a space in it. It goes on after one when the path does: a
        // separator follows, before anything that cannot stand in a name, at the end of
        // more of the name. A separator that follows a space begins another path.
        Some(' ') => {
            let could_be_name = &after[..after.find(ends_a_path).unwrap_or(after.len())];
            could_be_name.find(['\\', '/']).is_none_or(|goes_on| could_be_name[..goes_on].ends_with(' '))
        }
        Some(letter) => !(letter.is_alphanumeric() || matches!(letter, '_' | '-' | '~')),
    }
}

/// [`name_ends`] for the user's own folder. The folder of another user may be called the
/// same and something more after a space, and may be the last of a path: this one is only
/// taken to end where a path goes on into it, or where it cannot go on at all.
fn folder_ends(after: &str) -> bool {
    name_ends(after) && !after.starts_with(' ')
}

/// `text` with `with` in every place where `what` stands, in capitals or small letters,
/// and `ends` holds of what comes after it there. Windows does not tell capitals and small
/// letters apart in a path, and writes the same folder either way.
fn replace_where(text: &str, what: &str, with: &str, ends: &dyn Fn(&str) -> bool) -> String {
    // An empty `what` is found at every place, and replacing it would never end.
    if what.is_empty() {
        return text.to_owned();
    }
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    let mut replaced = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        // Compared letter by letter, since a letter and its other case need not be equally
        // long in bytes.
        let mut found = rest.char_indices();
        let stands = what.chars().all(|wanted| found.next().is_some_and(|(_, letter)| same(letter, wanted)));
        let after = stands.then(|| &rest[found.next().map_or(rest.len(), |(after, _)| after)..]);
        match after.filter(|after| ends(after)) {
            Some(after) => {
                replaced.push_str(with);
                rest = after;
            }
            None => {
                replaced.push(first);
                rest = &rest[first.len_utf8()..];
            }
        }
    }
    replaced
}

/// What cannot stand in the name of a file or folder, and so ends a path in a line.
fn ends_a_path(letter: char) -> bool {
    matches!(letter, '"' | '<' | '>' | '|' | '*' | '?' | ':' | ',' | ';')
}

/// `text` with the name of every computer that shares a folder, and of what it shares,
/// masked: `\\<host>\<share>`. What follows of the path stays.
fn without_shares(text: &str) -> String {
    let letters: Vec<char> = text.chars().collect();
    // A name of a computer or of what it shares, from `at` on: where it ends.
    let name =
        |at: usize| (at..letters.len()).find(|&end| matches!(letters[end], '\\' | '/') || ends_a_path(letters[end]));
    let slashes = |at: usize, many: usize| (at..at + many).all(|at| letters.get(at) == Some(&'\\'));
    let mut masked = String::with_capacity(text.len());
    let mut at = 0;
    while at < letters.len() {
        // Two backslashes begin it, or four where every backslash is doubled. Backslashes
        // further into a path, after a drive or a folder, begin nothing.
        let begins = at == 0 || !(letters[at - 1].is_alphanumeric() || matches!(letters[at - 1], '\\' | ':'));
        let share = [2, 1].into_iter().find_map(|each| {
            let host = at + 2 * each;
            let host_ends = name(host).filter(|&end| begins && slashes(at, 2 * each) && end > host)?;
            let share = host_ends + each;
            let share_ends = name(share).unwrap_or(letters.len());
            (slashes(host_ends, each) && share_ends > share).then_some((each, share_ends))
        });
        match share {
            Some((each, ends)) => {
                let slash = "\\".repeat(each);
                masked.push_str(&format!("{slash}{slash}<host>{slash}<share>"));
                at = ends;
            }
            None => {
                masked.push(letters[at]);
                at += 1;
            }
        }
    }
    masked
}

/// `text` with every path that still begins with a drive letter cut down to that:
/// `D:<path>`. Whatever folder it is, it is one this app has no name for, and how
/// somebody's drives are laid out is nobody's business either.
///
/// A path can have spaces in it, so where it ends in a line cannot be told for certain.
/// It is taken to go on to the next thing that cannot stand in a name, which may take a
/// word or two of the line with it. That is the lesser loss.
fn without_other_paths(text: &str) -> String {
    let letters: Vec<char> = text.chars().collect();
    let mut masked = String::with_capacity(text.len());
    let mut at = 0;
    while at < letters.len() {
        // A drive letter stands by itself: the `p` of `http://` is none.
        let alone = at == 0 || !letters[at - 1].is_alphanumeric();
        let drive = letters[at].is_ascii_alphabetic()
            && letters.get(at + 1) == Some(&':')
            && matches!(letters.get(at + 2), Some('\\' | '/'));
        if alone && drive {
            let mut ends = (at + 2..letters.len()).find(|&end| ends_a_path(letters[end])).unwrap_or(letters.len());
            while letters[ends - 1] == ' ' {
                ends -= 1;
            }
            masked.push(letters[at].to_ascii_uppercase());
            masked.push_str(":<path>");
            at = ends;
        } else {
            masked.push(letters[at]);
            at += 1;
        }
    }
    masked
}

/// Adds a line to the log file, for things that happen outside a run.
pub(crate) fn log_line(text: &str) {
    if let Some(mut log) = open_log() {
        write_line(&mut log, text);
    }
}

/// The date and time on this PC's clock, for a line in the log.
fn local_time() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        // SAFETY: the call only fills in the struct it is handed, and every bit pattern is a
        // valid `SYSTEMTIME`.
        let now = unsafe {
            let mut now: SYSTEMTIME = std::mem::zeroed();
            GetLocalTime(&mut now);
            now
        };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
        )
    }
    #[cfg(not(windows))]
    {
        let since_1970 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        format!("{}", since_1970.as_secs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The profile of the made-up user whose PC the lines below are from.
    const PROFILE: &str = r"C:\Users\kim";

    /// A line as the log has it, on a PC with the game on `C:`, a copy of it on `D:` under
    /// a folder that has the user's name in it, and the app's data in its usual place.
    fn logged(line: &str) -> String {
        let places = [
            (PathBuf::from(r"C:\Nexon\Maple"), Role::Install),
            (PathBuf::from(r"D:\Users\kim\Games\Maple Story"), Role::Install),
            (PathBuf::from(r"C:\Users\kim\AppData\Local\com.maplefastpatch.app"), Role::Data),
        ];
        without_places(line, &named(&places), Some(PROFILE))
    }

    /// Nothing of the PC is left in `line`: no name of the user, and no folder of theirs.
    fn says_nothing_of_the_pc(line: &str) {
        let small = line.to_lowercase();
        for given_away in ["kim", "users", "nexon\\", "nexon/", "downloads", "appdata", "games", "desktop-", "nas"] {
            assert!(!small.contains(given_away), "{given_away:?} is left in {line:?}");
        }
    }

    // The lines are the ones the app writes: what a run says as it starts and ends, what a
    // check and a look say when they fail, and what is said when the program itself fails.

    #[test]
    fn a_run_names_its_folder_by_what_it_is_and_its_patch_by_the_address_it_is_fetched_from() {
        let address = "https://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch";
        let found = logged(&format!(r"start 1.2.422 from {address} in C:\Nexon\Maple"));
        assert_eq!(found, format!("start 1.2.422 from {address} in C:<install>"));
        // An address is Nexon's and says nothing of the PC: it is written as it is.
        let elsewhere = "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch";
        let given =
            logged(&format!(r"[test] start 01207to01208.patch from {elsewhere} in D:\Users\kim\Games\Maple Story"));
        assert_eq!(given, format!("[test] start 01207to01208.patch from {elsewhere} in D:<install>"));
        assert_eq!(logged(r"discarded the work folder in C:\Nexon\Maple"), "discarded the work folder in C:<install>");
        // A folder written with the separator at its end is the same folder.
        assert_eq!(
            logged(r"discarded the work folder in C:\Nexon\Maple\"),
            r"discarded the work folder in C:<install>\"
        );
        for line in [found, given] {
            says_nothing_of_the_pc(&line);
        }
    }

    /// The engine says which file it could not work on, and prints what a check found as
    /// the value it is, with every backslash doubled.
    #[test]
    fn what_the_engine_says_of_a_file_names_the_place_it_is_in() {
        let denied = logged(r"ended: write C:\Nexon\Maple\Data\Map\Map001.wz: Access is denied. (os error 5)");
        assert_eq!(denied, r"ended: write C:<install>\Data\Map\Map001.wz: Access is denied. (os error 5)");

        let not_one = r#"ended: blocked by 1 check(s): [NotAnInstallDir { dir: "D:\\Users\\kim\\Games\\Maple Story", missing: "MapleStory.exe" }]"#;
        let expected =
            r#"ended: blocked by 1 check(s): [NotAnInstallDir { dir: "D:<install>", missing: "MapleStory.exe" }]"#;
        assert_eq!(logged(not_one), expected);

        let held = r#"ended: blocked by 2 check(s): [FilesLocked { files: ["C:\\Nexon\\Maple\\Data\\Base.wz", "C:\\Nexon\\Maple\\MapleStory.exe"] }, OutsideInstall { dir: "C:\\Nexon\\Maple\\Data\\Sound" }]"#;
        let expected = r#"ended: blocked by 2 check(s): [FilesLocked { files: ["C:<install>\\Data\\Base.wz", "C:<install>\\MapleStory.exe"] }, OutsideInstall { dir: "C:<install>\\Data\\Sound" }]"#;
        assert_eq!(logged(held), expected);

        // A work folder that leads elsewhere, as an apply that could not be undone names it.
        let linked = r#"could not undo an interrupted apply: blocked by 1 check(s): [OutsideInstall { dir: "C:\\Nexon\\Maple\\.maple-fast-patch\\staged" }]"#;
        let expected = r#"could not undo an interrupted apply: blocked by 1 check(s): [OutsideInstall { dir: "C:<install>\\.maple-fast-patch\\staged" }]"#;
        assert_eq!(logged(linked), expected);

        // The record of an apply that cannot be read, as a run that is turned down for it
        // names it. What stood in the record is quoted, and is of the game's own files.
        let unreadable = r#"ended: blocked by 1 check(s): [UnreadableJournal { dir: "C:\Nexon\Maple\.maple-fast-patch", detail: "line 3: "B\t12\tData\\Base.wz"" }]"#;
        let expected = r#"ended: blocked by 1 check(s): [UnreadableJournal { dir: "C:<install>\.maple-fast-patch", detail: "line 3: "B\t12\tData\\Base.wz"" }]"#;
        assert_eq!(logged(unreadable), expected);
        let looked = r#"could not undo an interrupted apply: its record cannot be read (line 1: "D:\kim\notes.txt")"#;
        assert_eq!(
            logged(looked),
            r#"could not undo an interrupted apply: its record cannot be read (line 1: "D:<path>")"#
        );

        // A check works in the app's own folder.
        let full = r"check failed: write C:\Users\kim\AppData\Local\com.maplefastpatch.app\patches\peek-4120-0.tmp: There is not enough space on the disk. (os error 112)";
        let expected = r"check failed: write <data>\patches\peek-4120-0.tmp: There is not enough space on the disk. (os error 112)";
        assert_eq!(logged(full), expected);
        let of_test = r"[test] check failed: create C:\Users\kim\AppData\Local\com.maplefastpatch.app\test\patches: Access is denied. (os error 5)";
        assert_eq!(logged(of_test), r"[test] check failed: create <data>\test\patches: Access is denied. (os error 5)");

        // The patch file a run fetches is kept in the install folder.
        let kept = r"ended: write C:\Nexon\Maple\.maple-fast-patch\download\00421to00422.patch.part: There is not enough space on the disk. (os error 112)";
        let expected = r"ended: write C:<install>\.maple-fast-patch\download\00421to00422.patch.part: There is not enough space on the disk. (os error 112)";
        assert_eq!(logged(kept), expected);

        // What a start that was turned down says, as the screen words it.
        let outside =
            r"did not start the patch by itself: 설치 경로 밖으로 연결된 폴더가 있습니다. (C:\Nexon\Maple\Data\Sound)";
        let expected =
            r"did not start the patch by itself: 설치 경로 밖으로 연결된 폴더가 있습니다. (C:<install>\Data\Sound)";
        assert_eq!(logged(outside), expected);

        for line in [not_one, held, full, of_test, kept] {
            says_nothing_of_the_pc(&logged(line));
        }
    }

    /// The same folder is written in more than one way: as Windows gives the path a folder
    /// really has, with forward slashes, and in capitals or small letters.
    #[test]
    fn a_place_is_known_however_its_path_is_written() {
        assert_eq!(
            logged(r"could not undo an interrupted apply: rename \\?\C:\Nexon\Maple\Data\Base.wz: x"),
            r"could not undo an interrupted apply: rename C:<install>\Data\Base.wz: x"
        );
        assert_eq!(logged(r#"dir: "\\\\?\\C:\\Nexon\\Maple""#), r#"dir: "C:<install>""#);
        assert_eq!(logged("open C:/Nexon/Maple/Data/Base.wz: x"), "open C:<install>/Data/Base.wz: x");
        assert_eq!(logged(r"open c:\NEXON\maple\Data\Base.wz: x"), r"open C:<install>\Data\Base.wz: x");
        assert_eq!(
            logged(r"open C:\USERS\KIM\appdata\local\COM.MAPLEFASTPATCH.APP\settings: x"),
            r"open <data>\settings: x"
        );
        // A folder inside another one that is known is the one that is meant.
        assert_eq!(
            logged(r"open C:\Users\kim\AppData\Local\com.maplefastpatch.app\maple-fast-patch.log: x"),
            r"open <data>\maple-fast-patch.log: x"
        );
    }

    /// A folder whose name another one begins with is not that other folder.
    #[test]
    fn a_folder_is_not_taken_for_one_whose_name_begins_the_same() {
        // The reviewer's case: the profile of `kim` and the folder of `kimchi`.
        let other = logged(r"ended: open C:\Users\kimchi\Downloads\a.patch: Access is denied. (os error 5)");
        assert_eq!(other, "ended: open C:<path>: Access is denied. (os error 5)");
        let spaced = logged(r"ended: open C:\Users\kim lee\Downloads\a.patch: Access is denied. (os error 5)");
        assert_eq!(spaced, "ended: open C:<path>: Access is denied. (os error 5)");
        let beside = logged(r"ended: open C:\Nexon\Maple2\Data\Base.wz: x");
        assert_eq!(beside, "ended: open C:<path>: x");
        let dotted = logged(r"ended: open C:\Nexon\Maple.old\Data\Base.wz: x");
        assert_eq!(dotted, "ended: open C:<path>: x");
        // Nor is a folder that is called the same and something more after a space.
        let longer = logged(r"ended: open C:\Nexon\Maple Backup\Data\Base.wz: x");
        assert_eq!(longer, "ended: open C:<path>: x");
        // The dot that ends a sentence is no part of a name, and neither are the words
        // that follow a path in a line.
        assert_eq!(logged(r"it is in C:\Nexon\Maple."), "it is in C:<install>.");
        assert_eq!(logged(r"it stays in C:\Nexon\Maple for now"), "it stays in C:<install> for now");
        for line in [other, spaced] {
            says_nothing_of_the_pc(&line);
        }
    }

    /// Whatever else lies in the user's own folder is written the way Windows itself
    /// abbreviates that folder.
    #[test]
    fn the_users_own_folder_is_written_as_windows_abbreviates_it() {
        let line = logged(r"failed: could not read C:\Users\kim\Documents\notes.txt at lib.rs:12");
        assert_eq!(line, r"failed: could not read %USERPROFILE%\Documents\notes.txt at lib.rs:12");
        assert_eq!(logged(r#"path: "C:\\Users\\KIM\\Documents""#), r#"path: "%USERPROFILE%\\Documents""#);
        assert_eq!(logged("open c:/users/kim/Documents/x: y"), "open %USERPROFILE%/Documents/x: y");
        assert_eq!(logged(r#"dir: "C:\\Users\\kim""#), r#"dir: "%USERPROFILE%""#);
    }

    /// A folder that another computer shares has that computer's name in its path.
    #[test]
    fn the_name_of_a_computer_that_shares_a_folder_is_not_written() {
        let shared = logged(r"ended: open \\DESKTOP-KIM\patches\old\a.patch: Access is denied. (os error 5)");
        assert_eq!(shared, r"ended: open \\<host>\<share>\old\a.patch: Access is denied. (os error 5)");
        let as_a_value = logged(r#"OutsideInstall { dir: "\\\\nas\\games\\Maple\\Data" }"#);
        assert_eq!(as_a_value, r#"OutsideInstall { dir: "\\\\<host>\\<share>\\Maple\\Data" }"#);
        let real = logged(r"ended: read \\?\UNC\nas\games\Maple\Data\Base.wz: x");
        assert_eq!(real, r"ended: read \\<host>\<share>\Maple\Data\Base.wz: x");
        for line in [shared, as_a_value, real] {
            says_nothing_of_the_pc(&line);
        }

        // An install folder on a share is the place it is, with no drive to name.
        let places = [(PathBuf::from(r"\\DESKTOP-KIM\Games\Maple"), Role::Install)];
        let line = r"discarded the work folder in \\DESKTOP-KIM\Games\Maple";
        assert_eq!(without_places(line, &named(&places), Some(PROFILE)), "discarded the work folder in <install>");
        let inside = r"ended: write \\DESKTOP-KIM\Games\Maple\Data\Base.wz: x";
        assert_eq!(without_places(inside, &named(&places), Some(PROFILE)), r"ended: write <install>\Data\Base.wz: x");

        // The backslashes inside a path that begins with a drive begin no share, and
        // neither do the slashes of an address.
        let held = r#"FilesLocked { files: ["E:\\backup\\Maple\\Base.wz"] }"#;
        assert_eq!(logged(held), r#"FilesLocked { files: ["E:<path>"] }"#);
        let stands_in =
            "live: patch files from http://127.0.0.1:18421/Patch, notice board at http://127.0.0.1:18421/board";
        assert_eq!(logged(stands_in), stands_in);
    }

    /// A path this app has no name for is cut down to its drive: a folder on another
    /// drive with the user's name in it, or one written the short way Windows also knows.
    #[test]
    fn any_other_path_is_cut_down_to_its_drive() {
        assert_eq!(logged(r"ended: open E:\kim\Maple Backup\Data\Base.wz: x"), "ended: open E:<path>: x");
        assert_eq!(logged(r"ended: open C:\PROGRA~1\Nexon\Maple\Data\Base.wz: x"), "ended: open C:<path>: x");
        assert_eq!(logged(r"ended: open C:\Users\KIMHAS~1\AppData\Local\Temp\x.tmp: x"), "ended: open C:<path>: x");
        assert_eq!(
            logged(r#"NotWritable { dir: "d:\\kim\\Maple", reason: "denied" }"#),
            r#"NotWritable { dir: "D:<path>", reason: "denied" }"#
        );
        assert_eq!(logged("ended: open E:/kim/Maple/Base.wz: x"), "ended: open E:<path>: x");
    }

    /// Most lines name no place, and are written as they are.
    #[test]
    fn a_line_that_names_no_place_is_written_as_it_is() {
        let lines = [
            "started 0.1.0",
            "step VerifyInstall",
            "done, 718 file(s) changed, replaced as they were built",
            "space needed 37151318016 bytes, available 16965120000 bytes",
            "storage NTFS, seek 0.12 ms, fast: 8 readers, 4 checkers, buffered",
            "maintenance underway until 10:00: 11/26(목) ver1.2.422 클라이언트 패치(04:00~10:00)",
            "[test] the folder is at 1208, minor 2: it holds what a minor patch starts from",
            "check failed: download failed (HTTP 503): unexpected answer",
            "could not read the maintenance information from the notice board: download failed: io: timeout",
            "did not start the patch by itself: 디스크 공간이 부족합니다. C: 여유 공간을 34.6 GB 이상 확보해 주세요. (현재 15.8 GB)",
            "ended: 3 file(s) differ from the original: Data\\Base.wz, Data\\Map\\Map001.wz, MapleStory.exe",
            "ended: patch file names a path outside the install folder: ..\\..\\Windows\\evil.dll",
        ];
        for line in lines {
            assert_eq!(logged(line), line);
        }
    }

    /// One event, one line: a name from a patch file cannot make up lines of its own.
    #[test]
    fn a_line_stays_one_line() {
        assert_eq!(
            logged("ended: rebuilt file has the wrong checksum: a.wz\n2026-11-26 04:00:00 done"),
            "ended: rebuilt file has the wrong checksum: a.wz 2026-11-26 04:00:00 done"
        );
        assert_eq!(logged("a\r\n\tb\u{0}c"), "a   b c");
    }

    #[test]
    fn a_place_is_called_by_what_it_is_to_the_app() {
        assert_eq!(called(Path::new(r"C:\Nexon\Maple"), Role::Install), "C:<install>");
        assert_eq!(called(Path::new(r"d:\Games\Maple"), Role::Install), "D:<install>");
        assert_eq!(called(Path::new(r"\\nas\games\Maple"), Role::Install), "<install>");
        assert_eq!(called(Path::new(r"C:\Users\kim\AppData\Local\com.maplefastpatch.app"), Role::Data), "<data>");
    }

    /// The same through the one door every line goes through: a folder the app has met is
    /// written by what it is, wherever on the PC it lies.
    #[test]
    fn a_line_goes_into_the_log_without_the_places_the_app_has_met() {
        let scratch = crate::scratch::Scratch::new("log-line");
        let install = scratch.0.join("Maple Story");
        meet_install(&install);
        let kept_in = scratch.0.join("log");
        let mut log = std::fs::File::create(&kept_in).unwrap();
        let address = "https://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch";
        write_line(&mut log, &format!("start 1.2.422 from {address} in {}", install.display()));
        write_line(&mut log, &format!("ended: write {}: denied", install.join("Data").join("Base.wz").display()));

        let written = std::fs::read_to_string(&kept_in).unwrap();
        // Each line begins with the day and the time it was written at.
        let said: Vec<&str> =
            written.lines().filter_map(|line| Some(line.split_once(' ')?.1.split_once(' ')?.1)).collect();
        let drive = drive_letter(&install).unwrap_or_default();
        let expected = [
            format!("start 1.2.422 from {address} in {drive}<install>"),
            format!(r"ended: write {drive}<install>\Data\Base.wz: denied"),
        ];
        assert_eq!(said, expected);
    }
}
