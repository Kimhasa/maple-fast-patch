//! Checks that run before a patch touches the install folder.
//!
//! Every check is read-only except [`check_writable`], which creates and removes one empty
//! file. Each returns the problems it found; an empty result means the check passed.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::apply;
use crate::stage::Dirs;
#[cfg(windows)]
use crate::wide::{from_wide, to_wide};

/// The programs of the client a folder is for.
///
/// The engine does not know the games by name: whoever asks for a check or a run says which
/// client is meant. Two clients can be installed side by side, and what one client's folder
/// holds and what runs from it says nothing about the other's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Programs {
    /// The game's own program. A folder without it is not an install folder of this client.
    pub game: &'static str,
    /// The program that starts the game. It lives in the install folder too.
    pub launcher: &'static str,
}

/// The folder of game data that every install folder has.
pub const DATA_DIR_NAME: &str = "Data";

/// Folder inside the install folder that holds rebuilt files and the backups of replaced
/// files. A fixed name, so a later run can find what an interrupted run left behind.
pub const WORK_DIR_NAME: &str = ".maple-fast-patch";

/// The file beside the work folder that a run holds open for as long as it works on the
/// install folder. Its name is the work folder's with `.lock` behind it.
pub const LOCK_FILE_NAME: &str = ".maple-fast-patch.lock";

/// Folder inside the work folder where the app keeps the patch files it downloads, so that
/// they are on the drive the game is on. They are no work of a run: a work folder that holds
/// nothing else counts as no work folder at all.
pub(crate) const DOWNLOAD_DIR_NAME: &str = "download";

/// Space that must stay free on a volume at the patch's peak usage. A chosen safety
/// cushion, not a measured requirement.
pub const SPACE_MARGIN: u64 = 1 << 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The folder does not look like a MapleStory install.
    NotAnInstallDir { dir: PathBuf, missing: &'static str },
    /// The free space of a volume could not be read, so it is unknown whether the patch fits.
    SpaceUnknown { volume: String, reason: String },
    /// A volume cannot hold everything the patch will write to it.
    NotEnoughSpace { volume: String, needed: u64, available: u64 },
    /// Files cannot be created in the folder.
    NotWritable { dir: PathBuf, reason: String },
    /// Programs started from the install folder are still running.
    GameRunning { names: Vec<String> },
    /// Files the patch will replace are held open by another program.
    FilesLocked { files: Vec<PathBuf> },
    /// Another run is already working on this install folder.
    AlreadyRunning,
    /// A folder the patch writes into is really somewhere else (reached through a junction
    /// or symlink): outside the install folder, possibly on another drive.
    OutsideInstall { dir: PathBuf },
    /// An earlier run stopped partway and left its work folder behind.
    LeftoverWork { dir: PathBuf },
    /// An earlier run was cut off while it applied, and what that apply did cannot be
    /// undone. The journal that says how far it had come cannot be read, or is gone: a line
    /// of it is not one this version writes, or names a place outside the install folder,
    /// or the file is not text. And the folder could not be put back without the journal
    /// either, from what the apply had moved aside: see [`crate::run::recover`] for when
    /// that is so. Nothing may go on over such a folder. `dir` is the work folder;
    /// `detail` says what could not be read and why the folder could not be put back, for
    /// a log.
    ///
    /// The install folder may be a mix of old and new files. It stays like that until the
    /// work folder is removed by [`crate::run::discard_work_as_it_is`], which says what is
    /// then left.
    UnreadableJournal { dir: PathBuf, detail: String },
    /// The player chose the immediate way (see [`crate::immediate`]), and it is not begun
    /// on this drive because the drive is not NTFS: on `file_system` it is not known that a
    /// file moved over another survives a power cut whole. The run is refused for this
    /// alone, before anything is fetched or read. It is not sent the safe way: that would
    /// be another way than the one chosen.
    ImmediateNeedsNtfs { file_system: String },
}

/// Bytes the patch will write under `dir` at its peak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceNeed {
    pub dir: PathBuf,
    pub bytes: u64,
}

impl Programs {
    /// The folder must hold this client's game program and its `Data` folder. The folder of
    /// another client fails here: its game program has another name.
    pub fn check_install_dir(&self, dir: &Path) -> Vec<Problem> {
        let missing = if !dir.join(self.game).is_file() {
            Some(self.game)
        } else if !dir.join(DATA_DIR_NAME).is_dir() {
            Some(DATA_DIR_NAME)
        } else {
            None
        };
        missing.map(|missing| Problem::NotAnInstallDir { dir: dir.to_path_buf(), missing }).into_iter().collect()
    }

    /// Finds running programs whose executable lives inside the install folder.
    ///
    /// Matching by location instead of by name also catches helper programs (launcher,
    /// patcher, restarter) without keeping a list of their names up to date, and leaves out
    /// the other client's game, which runs from its own folder. This program itself is
    /// never one of them. A copy of the app kept inside the game folder, or a game installed
    /// at the top of the drive the app is on, would otherwise count as the game running for
    /// as long as the app is open.
    pub fn check_running(&self, install_dir: &Path) -> Vec<Problem> {
        let names = self.running_inside(install_dir, std::process::id());
        if names.is_empty() { Vec::new() } else { vec![Problem::GameRunning { names }] }
    }

    /// Names of the running programs that live inside `install_dir`, leaving out the
    /// process numbered `except`.
    fn running_inside(&self, install_dir: &Path, except: u32) -> Vec<String> {
        self.running_among(processes(), install_dir, except)
    }

    /// [`Programs::running_inside`] for a list of programs that is given.
    fn running_among(&self, processes: Vec<Process>, install_dir: &Path, except: u32) -> Vec<String> {
        // If the folder or the process list cannot be read, report nothing here; the lock
        // check before the apply step still catches any program that holds a file open.
        let Ok(install_dir) = install_dir.canonicalize() else {
            return Vec::new();
        };
        let mut names: Vec<String> = processes
            .into_iter()
            .filter(|process| process.id != except)
            .filter(|process| match &process.exe {
                Some(exe) => exe.canonicalize().is_ok_and(|exe| exe.starts_with(&install_dir)),
                // Windows does not say where every program lives: one that is guarded, or
                // run by someone else, may not be asked. This client's own programs are
                // then recognised by name; which install they belong to cannot be told, so
                // they count.
                None => [self.game, self.launcher].iter().any(|name| process.name.eq_ignore_ascii_case(name)),
            })
            .map(|process| process.name)
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// Compares what the patch will write against the free space of each volume involved.
///
/// Needs on the same volume are added together, because the download, the rebuilt files and
/// anything else stored there all exist at once at the peak.
pub fn check_space(needs: &[SpaceNeed]) -> Vec<Problem> {
    check_space_with(needs, SPACE_MARGIN, free_space)
}

/// [`check_space`] with a chosen margin and a chosen way to ask a drive what is free.
pub(crate) fn check_space_with(
    needs: &[SpaceNeed],
    margin: u64,
    available: impl Fn(&Path) -> io::Result<u64>,
) -> Vec<Problem> {
    let mut per_volume: BTreeMap<String, (u64, &Path)> = BTreeMap::new();
    for need in needs {
        let entry = per_volume.entry(volume_of(&need.dir)).or_insert((0, need.dir.as_path()));
        entry.0 = entry.0.saturating_add(need.bytes);
    }

    let mut problems = Vec::new();
    for (volume, (bytes, dir)) in per_volume {
        let needed = bytes.saturating_add(margin);
        // Unknown free space blocks the patch just like a shortage does: starting a
        // multi-gigabyte write without knowing it fits is what this check exists to prevent.
        let available = match available(dir) {
            Ok(available) => available,
            Err(err) => {
                problems.push(Problem::SpaceUnknown { volume, reason: err.to_string() });
                continue;
            }
        };
        if available < needed {
            problems.push(Problem::NotEnoughSpace { volume, needed, available });
        }
    }
    problems
}

/// Free bytes on the drive that holds `dir`.
pub fn free_space(dir: &Path) -> io::Result<u64> {
    fs4::available_space(existing_ancestor(dir))
}

/// Creates and removes an empty file to prove the folder accepts writes.
pub fn check_writable(dir: &Path) -> Vec<Problem> {
    let probe = dir.join(format!(".maple-fast-patch-write-test-{}", std::process::id()));
    let result = OpenOptions::new().write(true).create_new(true).open(&probe).map(drop);
    let _ = fs::remove_file(&probe);
    match result {
        Ok(()) => Vec::new(),
        Err(err) => vec![Problem::NotWritable { dir: dir.to_path_buf(), reason: err.to_string() }],
    }
}

/// A running program: its number, the name of its executable, and where that executable
/// is, if Windows would say.
struct Process {
    id: u32,
    name: String,
    exe: Option<PathBuf>,
}

/// Lists the running programs.
///
/// Nothing is asked of a program beyond where its executable is, and the right that takes
/// (`PROCESS_QUERY_LIMITED_INFORMATION`) lets nobody read or change anything inside the
/// program. That is why Windows is asked directly here. The app looks every few seconds,
/// also while the game runs under its guard against cheating, and the library that used to
/// do this opened every program on the PC with the right to read its memory. A patcher has
/// no business asking for that, whatever comes of it.
#[cfg(windows)]
fn processes() -> Vec<Process> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };

    let mut found = Vec::new();
    // SAFETY: the call takes no pointers. The handle it returns is closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return found;
    }
    // The longest path Windows hands out has 32,767 units.
    let mut path = vec![0u16; 32_768];
    let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    // SAFETY: `snapshot` is an open snapshot of the process list, and `entry` says its own
    // size, as both calls on it require.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        found.push(Process {
            id: entry.th32ProcessID,
            name: from_wide(&entry.szExeFile),
            exe: executable_of(entry.th32ProcessID, &mut path),
        });
        // SAFETY: as for the first entry.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: the snapshot is open and is not used after this.
    unsafe { CloseHandle(snapshot) };
    found
}

#[cfg(not(windows))]
fn processes() -> Vec<Process> {
    Vec::new()
}

/// Where the executable of the process numbered `id` is, or `None` when Windows will not
/// say. `buffer` is room for the answer.
#[cfg(windows)]
fn executable_of(id: u32, buffer: &mut [u16]) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    // SAFETY: the call takes no pointers. A handle it returns is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id) };
    if process.is_null() {
        return None;
    }
    let mut len = buffer.len() as u32;
    // SAFETY: `process` is open, `buffer` has room for `len` units, and the call writes to
    // `len` how many of them it filled in.
    let found = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut len) } != 0;
    // SAFETY: the process handle is open and is not used after this.
    unsafe { CloseHandle(process) };
    let path = buffer.get(..len as usize).filter(|path| found && !path.is_empty())?;
    Some(PathBuf::from(std::ffi::OsString::from_wide(path)))
}

/// How often, and how far apart, a file found held is looked at again before it is reported:
/// about two seconds in all.
const LOCK_LOOKS: u32 = 10;
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(200);

/// Finds files that another program holds open in a way that would make replacing them fail.
///
/// Run this right before the apply step as well: a lock can appear after the first check.
pub(crate) fn check_locks(files: &[PathBuf]) -> Vec<Problem> {
    let mut locked: Vec<PathBuf> = files.iter().filter(|file| is_locked(file)).cloned().collect();
    // Virus scanners and the search indexer open a file for a moment, most of all right
    // after files near it were written. Only a file that stays held is in the way.
    for _ in 0..LOCK_LOOKS {
        if locked.is_empty() {
            break;
        }
        std::thread::sleep(LOCK_WAIT);
        locked.retain(|file| is_locked(file));
    }
    if locked.is_empty() { Vec::new() } else { vec![Problem::FilesLocked { files: locked }] }
}

#[cfg(windows)]
fn is_locked(file: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE};

    // A file is replaced by renaming, and a rename needs the right to delete and nothing
    // else. Asking for just that, and sharing everything, fails exactly when someone holds
    // the file open without letting others delete it. A virus scanner or the search indexer
    // that reads a file and shares all of it does not stop a rename, and is not reported:
    // it would stop a run that has built everything for something that is not in its way.
    let sharing = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    match OpenOptions::new().access_mode(DELETE).share_mode(sharing).open(file) {
        Ok(_) => false,
        Err(err) => crate::busy::held(&err),
    }
}

#[cfg(not(windows))]
fn is_locked(_file: &Path) -> bool {
    false
}

/// Confirms that every folder the patch writes into really lies inside the install folder.
///
/// A folder relocated with a junction or symlink would make the patch change files somewhere
/// else, and on another drive it would break the apply step halfway (files are moved into
/// place by renaming, which only works within one drive). It is refused before anything is built.
pub(crate) fn check_inside_install(install: &Path, dirs: &[PathBuf]) -> Vec<Problem> {
    let Ok(home) = install.canonicalize() else {
        return Vec::new();
    };
    dirs.iter()
        .filter(|dir| !dir.canonicalize().is_ok_and(|real| real.starts_with(&home)))
        .map(|dir| Problem::OutsideInstall { dir: dir.clone() })
        .collect()
}

/// Reports the work folder of an earlier run that did not finish.
///
/// Where that run was cut off while applying and cannot be undone, that is what is
/// reported, as [`Problem::UnreadableJournal`], and nothing else: such a folder holds no
/// work that a run could go on with.
///
/// A journal that cannot be read does not make a folder one of those. Such a folder is put
/// back from what the apply moved aside, by [`crate::run::recover`] and by the start of a
/// run, and whether that works shows only when it is tried. So this reports it once it was
/// tried and could not be done, and before that only where it is certain without trying:
/// what was moved aside carries no names, because an earlier version moved it. A folder
/// that has not been tried yet is reported as work that is left, like one whose journal can
/// be read. Whoever wants the answer to be final asks [`crate::run::recover`] first.
///
/// Nothing is changed by looking.
pub fn check_leftovers(install_dir: &Path) -> Vec<Problem> {
    let dirs = Dirs::new(install_dir);
    if let Some(unreadable) = apply::unreadable_journal(&dirs) {
        return vec![unreadable];
    }
    let dir = dirs.work();
    if holds_work(&dir) { vec![Problem::LeftoverWork { dir }] } else { Vec::new() }
}

/// Whether a run left anything behind in the work folder `work`. Downloaded patch files are
/// not work of a run, so a folder that holds only those, or nothing, does not count.
pub(crate) fn holds_work(work: &Path) -> bool {
    match fs::read_dir(work) {
        Ok(children) => children.flatten().any(|child| !child.file_name().eq_ignore_ascii_case(DOWNLOAD_DIR_NAME)),
        // Something of that name that cannot be listed is not a folder the engine made.
        Err(_) => work.exists(),
    }
}

/// What kind of PC this is, for a report of how long a patch took. Nothing in it names the
/// PC or the person using it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    /// The processor as it names itself, such as `AMD Ryzen 7 5800X 8-Core Processor`.
    pub cpu: String,
    /// Physical cores.
    pub cores: usize,
    /// Logical processors.
    pub threads: usize,
    pub ram_bytes: u64,
    /// The Windows edition and build, such as `Windows 11 Pro (26200)`.
    pub os: String,
}

/// Reads what [`Machine`] holds. Whatever cannot be read stays empty or zero. Windows is
/// asked for each fact directly; no other program is looked at.
#[cfg(windows)]
pub fn machine() -> Machine {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::{ALL_PROCESSOR_GROUPS, GetActiveProcessorCount};

    const WINDOWS: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build = registry_text(WINDOWS, "CurrentBuildNumber").unwrap_or_default();
    let mut os = registry_text(WINDOWS, "ProductName").unwrap_or_default();
    // Windows 11 still gives its name as Windows 10 there. The build number tells them apart.
    if build.parse::<u32>().is_ok_and(|build| build >= 22_000) {
        os = os.replacen("Windows 10 ", "Windows 11 ", 1);
    }
    if !os.is_empty() && !build.is_empty() {
        os = format!("{os} ({build})");
    }

    let mut memory = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `memory` says its own size, as the call requires of what it fills in.
    let memory_read = unsafe { GlobalMemoryStatusEx(&mut memory) } != 0;
    Machine {
        cpu: registry_text(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString")
            .unwrap_or_default(),
        cores: physical_cores(),
        // SAFETY: the call takes no pointers.
        threads: unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) } as usize,
        ram_bytes: if memory_read { memory.ullTotalPhys } else { 0 },
        os,
    }
}

/// Reads what [`Machine`] holds. Away from Windows that is the number of logical processors
/// and nothing else.
#[cfg(not(windows))]
pub fn machine() -> Machine {
    let threads = std::thread::available_parallelism().map_or(0, |n| n.get());
    Machine { cpu: String::new(), cores: 0, threads, ram_bytes: 0, os: String::new() }
}

/// A text value from the part of the registry that describes the PC, without spaces around
/// it. `None` when it is not there or is no text.
#[cfg(windows)]
fn registry_text(key: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};

    let (key, value) = (to_wide(key), to_wide(value));
    let mut text = [0u16; 256];
    let mut bytes = std::mem::size_of_val(&text) as u32;
    // SAFETY: both names end in a zero, `text` has room for `bytes` bytes, which the call
    // fills with text and its ending zero, and the kind of value is not asked back.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            text.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if status != 0 {
        return None;
    }
    Some(from_wide(text.get(..bytes as usize / 2)?).trim().to_owned())
}

/// How many processor cores the PC has, each counted once however many threads it runs.
/// Zero when Windows does not say.
#[cfg(windows)]
fn physical_cores() -> usize {
    use windows_sys::Win32::System::SystemInformation::{GetLogicalProcessorInformationEx, RelationProcessorCore};

    // Asked with no room at all, the call says how much room its answer takes.
    let mut bytes = 0u32;
    // SAFETY: with a length of zero nothing is written through the null pointer.
    unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut bytes) };
    // The answer is a row of records of differing lengths. A buffer of 64-bit numbers is
    // aligned for any of them.
    let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
    // SAFETY: `buffer` has room for `bytes` bytes and is aligned for the records.
    let filled =
        unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, buffer.as_mut_ptr().cast(), &mut bytes) } != 0;
    if !filled {
        return 0;
    }

    let (mut cores, mut at) = (0, 0usize);
    while at + 8 <= (bytes as usize).min(buffer.len() * 8) {
        // Every record starts with what it describes and how long it is, four bytes each.
        // SAFETY: the eight bytes at `at` lie inside the buffer, as was just checked, and
        // they are read without anything being assumed about where they start.
        let (relationship, size) = unsafe {
            let record = buffer.as_ptr().cast::<u8>().add(at);
            (record.cast::<i32>().read_unaligned(), record.add(4).cast::<u32>().read_unaligned())
        };
        if size == 0 {
            break;
        }
        if relationship == RelationProcessorCore {
            cores += 1;
        }
        at += size as usize;
    }
    cores
}

/// The drive or share a path is on, used to group space needs. Paths reached through a
/// junction or symlink are grouped by where they appear to be, not where they resolve.
fn volume_of(path: &Path) -> String {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => prefix.as_os_str().to_string_lossy().to_uppercase(),
        _ => "/".to_owned(),
    }
}

/// The nearest folder that exists, since free space can only be asked of a real path.
fn existing_ancestor(path: &Path) -> &Path {
    path.ancestors().find(|p| p.exists()).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn need(dir: &str, bytes: u64) -> SpaceNeed {
        SpaceNeed { dir: PathBuf::from(dir), bytes }
    }

    /// The programs of the two clients that can be installed side by side.
    const LIVE: Programs = Programs { game: "MapleStory.exe", launcher: "GameLauncher.exe" };
    const TEST: Programs = Programs { game: "MapleStoryT.exe", launcher: "GameLauncherT.exe" };

    /// Makes `parent\name` a folder that passes as the install folder of a client.
    fn install_of(programs: &Programs, parent: &Path, name: &str) -> PathBuf {
        let install = parent.join(name);
        fs::create_dir_all(install.join(DATA_DIR_NAME)).unwrap();
        fs::write(install.join(programs.game), b"").unwrap();
        install
    }

    #[test]
    fn install_folder_of_the_other_client_is_not_this_clients() {
        let dir = tempfile::tempdir().unwrap();
        let (live, test) = (install_of(&LIVE, dir.path(), "Maple"), install_of(&TEST, dir.path(), "MapleT"));
        assert!(LIVE.check_install_dir(&live).is_empty() && TEST.check_install_dir(&test).is_empty());
        assert_eq!(
            TEST.check_install_dir(&live),
            [Problem::NotAnInstallDir { dir: live.clone(), missing: "MapleStoryT.exe" }]
        );
        assert_eq!(
            LIVE.check_install_dir(&test),
            [Problem::NotAnInstallDir { dir: test.clone(), missing: "MapleStory.exe" }]
        );
        // The data folder is asked for whichever client it is.
        fs::remove_dir(test.join("Data")).unwrap();
        assert_eq!(TEST.check_install_dir(&test), [Problem::NotAnInstallDir { dir: test, missing: "Data" }]);
    }

    #[test]
    fn install_dir_needs_exe_and_data() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            LIVE.check_install_dir(dir.path()),
            [Problem::NotAnInstallDir { dir: dir.path().to_path_buf(), missing: "MapleStory.exe" }]
        );

        fs::write(dir.path().join("MapleStory.exe"), b"").unwrap();
        assert_eq!(
            LIVE.check_install_dir(dir.path()),
            [Problem::NotAnInstallDir { dir: dir.path().to_path_buf(), missing: "Data" }]
        );

        fs::create_dir(dir.path().join("Data")).unwrap();
        assert!(LIVE.check_install_dir(dir.path()).is_empty());
    }

    #[test]
    fn space_on_one_volume_is_added_together() {
        let needs = [need(r"C:\Nexon\Maple", 3 * GIB), need(r"c:\Users\me\Downloads", 2 * GIB)];

        // 5 GiB of writes plus the 1 GiB margin fits exactly in 6 GiB.
        assert!(check_space_with(&needs, GIB, |_| Ok(6 * GIB)).is_empty());

        assert_eq!(
            check_space_with(&needs, GIB, |_| Ok(6 * GIB - 1)),
            [Problem::NotEnoughSpace { volume: "C:".into(), needed: 6 * GIB, available: 6 * GIB - 1 }]
        );
    }

    #[test]
    fn space_is_checked_per_volume() {
        let needs = [need(r"C:\Nexon\Maple", 3 * GIB), need(r"D:\Downloads", 2 * GIB)];
        let available = |dir: &Path| Ok(if dir.starts_with(r"C:\") { 10 * GIB } else { GIB });

        assert_eq!(
            check_space_with(&needs, GIB, available),
            [Problem::NotEnoughSpace { volume: "D:".into(), needed: 3 * GIB, available: GIB }]
        );
    }

    #[test]
    fn unreadable_free_space_blocks_the_patch() {
        let needs = [need(r"C:\Nexon\Maple", GIB)];
        let problems = check_space_with(&needs, GIB, |_| Err(io::Error::other("no such volume")));
        assert_eq!(problems, [Problem::SpaceUnknown { volume: "C:".into(), reason: "no such volume".into() }]);
    }

    #[test]
    fn real_volume_reports_free_space() {
        let dir = tempfile::tempdir().unwrap();
        // A path that does not exist yet is measured through its nearest existing parent.
        let needs = [SpaceNeed { dir: dir.path().join("not-created-yet"), bytes: 1 }];
        assert!(check_space_with(&needs, 0, |d| fs4::available_space(existing_ancestor(d))).is_empty());
    }

    #[test]
    fn writable_folder_passes_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_writable(dir.path()).is_empty());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn missing_folder_is_not_writable() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone");
        assert!(matches!(check_writable(&gone)[..], [Problem::NotWritable { .. }]));
    }

    #[cfg(windows)]
    #[test]
    fn file_held_open_by_someone_else_is_locked() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let held = dir.path().join("Skill_000.wz");
        let free = dir.path().join("Mob_000.wz");
        fs::write(&held, b"x").unwrap();
        fs::write(&free, b"x").unwrap();

        let handle = OpenOptions::new().read(true).share_mode(0).open(&held).unwrap();
        let files = [held.clone(), free.clone(), dir.path().join("missing.wz")];
        assert_eq!(check_locks(&files), [Problem::FilesLocked { files: vec![held.clone()] }]);

        drop(handle);
        assert!(check_locks(&files).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn only_a_handle_that_would_stop_a_rename_counts_as_a_lock() {
        use std::os::windows::fs::OpenOptionsExt;
        // Share modes: others may read, write, delete.
        const READ: u32 = 1;
        const WRITE: u32 = 2;
        const DELETE: u32 = 4;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Skill_000.wz");
        fs::write(&file, b"x").unwrap();
        let files = [file.clone()];

        // The way a scanner or the indexer reads: everything is shared, and the file can
        // be renamed under it, as the two renames show.
        let shared = OpenOptions::new().read(true).share_mode(READ | WRITE | DELETE).open(&file).unwrap();
        assert!(check_locks(&files).is_empty(), "a handle that shares everything was taken for a lock");
        let moved = dir.path().join("moved.wz");
        fs::rename(&file, &moved).unwrap();
        fs::rename(&moved, &file).unwrap();
        drop(shared);

        // Held without letting others delete, whatever else is shared: a rename fails, and
        // the check says so.
        for sharing in [0, READ | WRITE] {
            let held = OpenOptions::new().read(true).share_mode(sharing).open(&file).unwrap();
            assert_eq!(check_locks(&files), [Problem::FilesLocked { files: vec![file.clone()] }], "sharing {sharing}");
            assert!(fs::rename(&file, &moved).is_err(), "sharing {sharing}: the rename went through");
            drop(held);
        }
        assert!(check_locks(&files).is_empty());
    }

    #[test]
    fn nothing_runs_from_an_empty_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(LIVE.check_running(dir.path()).is_empty());
    }

    /// Stands in for a running game: waits until it is killed. Only ever started by the
    /// test below, from a copy of this test program.
    #[cfg(windows)]
    #[test]
    #[ignore = "started as a child process by the test below"]
    fn stand_in_for_a_running_game() {
        let _ = io::stdin().read_line(&mut String::new());
    }

    /// Starts a copy of this test program at `exe`, where it stands in for a running game:
    /// it waits on its input until it is killed.
    #[cfg(windows)]
    fn stand_in(exe: &Path) -> std::process::Child {
        use std::process::{Command, Stdio};

        fs::copy(std::env::current_exe().unwrap(), exe).unwrap();
        Command::new(exe)
            .args(["--exact", "preflight::tests::stand_in_for_a_running_game", "--ignored", "--nocapture"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn running_program_inside_the_folder_is_found_and_this_one_is_left_out() {
        // A copy of this test program plays the game. It runs from the folder that plays
        // the install folder, waiting on its input, until it is killed.
        let dir = tempfile::tempdir().unwrap();
        let mut child = stand_in(&dir.path().join("Game.exe"));

        let found = LIVE.running_inside(dir.path(), std::process::id());
        // The process that asks is not reported, here shown with the child in that role.
        let without_itself = LIVE.running_inside(dir.path(), child.id());
        let problems = LIVE.check_running(dir.path());
        child.kill().unwrap();
        child.wait().unwrap();

        assert_eq!(found, ["Game.exe"]);
        assert!(without_itself.is_empty(), "{without_itself:?}");
        assert_eq!(problems, [Problem::GameRunning { names: vec!["Game.exe".to_owned()] }]);
    }

    #[cfg(windows)]
    #[test]
    fn game_of_one_client_running_does_not_count_for_the_folder_of_the_other() {
        // The two clients side by side, each in its own folder. The test client's game runs.
        let dir = tempfile::tempdir().unwrap();
        let (live, test) = (install_of(&LIVE, dir.path(), "Maple"), install_of(&TEST, dir.path(), "MapleT"));
        let mut child = stand_in(&test.join(TEST.game));

        let (asked_of_test, asked_of_live) = (TEST.check_running(&test), LIVE.check_running(&live));
        // Where Windows says what folder a program runs from, the folder decides and the
        // names do not: whoever asks about that folder finds it.
        let live_names_test_folder = LIVE.check_running(&test);
        child.kill().unwrap();
        child.wait().unwrap();

        let running = [Problem::GameRunning { names: vec!["MapleStoryT.exe".to_owned()] }];
        assert_eq!(asked_of_test, running);
        assert!(asked_of_live.is_empty(), "{asked_of_live:?}");
        assert_eq!(live_names_test_folder, running);
    }

    #[test]
    fn program_whose_place_is_hidden_counts_only_under_the_names_asked_for() {
        // Windows does not say where a guarded game runs from. Such a program is told by
        // its name alone, and that is where the two clients must not be mixed up.
        let dir = tempfile::tempdir().unwrap();
        let inside = dir.path().join("Helper.exe");
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("MapleStoryT.exe");
        for file in [&inside, &outside] {
            fs::write(file, b"").unwrap();
        }
        let hidden = |id: u32, name: &str| Process { id, name: name.to_owned(), exe: None };
        let listed = || {
            vec![
                hidden(1, "MapleStoryT.exe"),
                hidden(2, "maplestory.EXE"),
                hidden(3, "GameLauncherT.exe"),
                hidden(4, "GameLauncher.exe"),
                hidden(5, "Other.exe"),
                // One whose place is known is judged by its place, whatever it is called.
                Process { id: 6, name: "MapleStoryT.exe".to_owned(), exe: Some(outside.clone()) },
                Process { id: 7, name: "Helper.exe".to_owned(), exe: Some(inside.clone()) },
            ]
        };

        assert_eq!(TEST.running_among(listed(), dir.path(), 0), ["GameLauncherT.exe", "Helper.exe", "MapleStoryT.exe"]);
        assert_eq!(LIVE.running_among(listed(), dir.path(), 0), ["GameLauncher.exe", "Helper.exe", "maplestory.EXE"]);
        // The program that asks is left out by its number, not by its name.
        assert_eq!(TEST.running_among(listed(), dir.path(), 1), ["GameLauncherT.exe", "Helper.exe"]);
    }

    #[cfg(windows)]
    #[test]
    fn every_running_program_is_listed_with_its_name_and_this_one_with_its_place() {
        let listed = processes();
        assert!(listed.len() > 5, "only {} programs were listed", listed.len());
        assert!(listed.iter().all(|process| !process.name.is_empty()));
        // This program may always be asked where it is, and the answer is where it is.
        let me = listed.iter().find(|process| process.id == std::process::id()).expect("this program is not listed");
        let here = std::env::current_exe().unwrap();
        assert_eq!(me.exe.as_ref().unwrap().canonicalize().unwrap(), here.canonicalize().unwrap());
        assert_eq!(std::ffi::OsStr::new(&me.name), here.file_name().unwrap());
        // What cannot be asked is listed all the same, without a place. Number 0 is such a
        // one: it stands for the processors having nothing to do, and is no program at all.
        let idle = listed.iter().find(|process| process.id == 0).expect("the idle process is not listed");
        assert_eq!(idle.exe, None);
    }

    #[test]
    fn folders_inside_the_install_folder_pass() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Data");
        fs::create_dir(&data).unwrap();
        let outside = tempfile::tempdir().unwrap();
        assert!(check_inside_install(dir.path(), &[data]).is_empty());
        let dirs = [outside.path().to_path_buf()];
        assert_eq!(check_inside_install(dir.path(), &dirs), [Problem::OutsideInstall { dir: dirs[0].clone() }]);
    }

    #[test]
    fn leftover_work_folder_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_leftovers(dir.path()).is_empty());

        let work = dir.path().join(WORK_DIR_NAME);
        fs::create_dir_all(work.join("new")).unwrap();
        assert_eq!(check_leftovers(dir.path()), [Problem::LeftoverWork { dir: work }]);
    }

    #[test]
    fn work_folder_with_nothing_but_downloads_is_no_leftover() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join(WORK_DIR_NAME);
        // With nothing in it, and with nothing but downloaded patch files in it.
        fs::create_dir(&work).unwrap();
        assert!(check_leftovers(dir.path()).is_empty());
        fs::create_dir(work.join(DOWNLOAD_DIR_NAME)).unwrap();
        fs::write(work.join(DOWNLOAD_DIR_NAME).join("00001to00002.patch.part"), b"half a patch file").unwrap();
        assert!(check_leftovers(dir.path()).is_empty());

        // Anything beside the downloads is work of a run.
        fs::write(work.join("patch.id"), b"an id").unwrap();
        assert_eq!(check_leftovers(dir.path()), [Problem::LeftoverWork { dir: work.clone() }]);

        // And so is something of that name that is not a folder at all.
        fs::remove_dir_all(&work).unwrap();
        fs::write(&work, b"not a folder").unwrap();
        assert_eq!(check_leftovers(dir.path()), [Problem::LeftoverWork { dir: work }]);
    }

    #[cfg(windows)]
    #[test]
    fn machine_facts_are_read() {
        let found = machine();
        assert!(found.cores >= 1 && found.threads >= found.cores, "{found:?}");
        assert!(found.threads >= std::thread::available_parallelism().unwrap().get(), "{found:?}");
        assert!(found.ram_bytes > 1 << 30, "{found:?}");
        assert!(!found.cpu.is_empty() && found.cpu == found.cpu.trim(), "{found:?}");
        assert!(found.os.starts_with("Windows") && found.os.ends_with(')'), "{found:?}");
        // The same every time it is asked.
        assert_eq!(machine(), found);
    }
}
