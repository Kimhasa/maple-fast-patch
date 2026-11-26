//! Which of the programs and drivers that run on this PC say something about the kind of
//! PC it is.
//!
//! Everything that runs is listed by Windows, and nearly all of it is thrown away at once:
//! what is kept is a name this crate knows as a PC room's program, a name that holds a word
//! such programs tend to hold, and the game itself. No other name leaves this module, and
//! nothing is asked of any program beyond the name of its executable.

use crate::facts::{Fact, Programs, Unknown};
use crate::wide::{from_wide, from_wide_ptr};

/// Programs that hand a seat without a disk its disks: K하드's `PFGameDisk` (the name of its
/// seat program; the name of its process is taken to begin the same way) and CCBoot's
/// client. A name counts when it begins with one of these.
const DISKLESS: [&str; 2] = ["pfgame", "ccboot"];
/// Programs that put a disk back as it was at every start: Deep Freeze (`DFServ.exe`, its
/// tray program and its driver) and Reboot Restore Rx (`ShdTray.exe`, `ShdServ.exe`).
const RESTORE: [&str; 5] = ["dfserv", "frzstate2k", "deepfrz", "shdtray", "shdserv"];
/// Programs a PC room manages its seats with: 피카's `pmclient.exe` and `pmAgent.exe`.
const SEAT: [&str; 2] = ["pmclient", "pmagent"];
/// Words in a name that few programs other than a PC room's hold. The names of 게토,
/// 지매니저 and 하드보안관 are not known, so this is what would catch them. A name with one
/// of these keeps a PC from being called an ordinary one.
const TELLING: [&str; 9] = ["iscsi", "vdisk", "restore", "freeze", "pica", "geto", "pfgame", "diskless", "nohard"];
/// Words such names tend to hold that plenty of other names hold too: Windows itself runs
/// `ahcache` and `vhdmp` on every PC. A name with one of these is listed and decides nothing.
const COMMON: [&str; 4] = ["vhd", "boot", "cache", "guard"];
/// Names that hold a telling word and are known to be something else.
const SOMETHING_ELSE: [&str; 2] = ["picasa", "systemrestore"];

/// A name without the ending that marks what kind of file it is, in small letters.
fn stem(name: &str) -> String {
    let lower = name.to_lowercase();
    match lower.rsplit_once('.') {
        Some((stem, "exe" | "sys" | "aye" | "dll")) => stem.to_owned(),
        _ => lower,
    }
}

/// Sorts the names of everything that runs into what the verdict goes by, and drops the
/// rest. `games` are the names of the game's own programs.
pub fn sort(names: impl IntoIterator<Item = String>, games: &[&str]) -> Programs {
    let mut found = Programs::default();
    for name in names {
        let stem = stem(&name);
        let begins = |list: &[&str]| list.iter().any(|known| stem.starts_with(known));
        let holds = |list: &[&str]| list.iter().any(|word| stem.contains(word));
        let list = if games.iter().any(|game| game.eq_ignore_ascii_case(&name)) {
            &mut found.game
        } else if begins(&DISKLESS) {
            &mut found.diskless
        } else if begins(&RESTORE) {
            &mut found.restore
        } else if begins(&SEAT) {
            &mut found.seat
        } else if holds(&TELLING) && !holds(&SOMETHING_ELSE) {
            &mut found.hints
        } else if holds(&TELLING) || holds(&COMMON) {
            &mut found.noted
        } else {
            continue;
        };
        if !list.iter().any(|listed: &String| listed.eq_ignore_ascii_case(&name)) {
            list.push(name);
        }
    }
    for list in
        [&mut found.diskless, &mut found.restore, &mut found.seat, &mut found.hints, &mut found.noted, &mut found.game]
    {
        list.sort_by_key(|name| name.to_lowercase());
    }
    found
}

/// The names of the executables of every running program.
pub fn processes() -> Fact<Vec<String>> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };

    // SAFETY: the call takes no pointers. The handle it returns is closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(super::last_error());
    }
    let mut names = Vec::new();
    let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    // SAFETY: `snapshot` is an open snapshot of the process list, and `entry` says its own
    // size, as both calls on it require.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        names.push(from_wide(&entry.szExeFile));
        // SAFETY: as for the first entry.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: the snapshot is open and is not used after this.
    unsafe { CloseHandle(snapshot) };
    Ok(names)
}

/// The names of the drivers that are running, as Windows' list of services has them.
///
/// The list of services is asked because any user may read it. The list of loaded drivers
/// itself is not handed to a program without an administrator's rights any more.
pub fn drivers() -> Fact<Vec<String>> {
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, ENUM_SERVICE_STATUS_PROCESSW, EnumServicesStatusExW, OpenSCManagerW, SC_ENUM_PROCESS_INFO,
        SC_MANAGER_ENUMERATE_SERVICE, SERVICE_ACTIVE, SERVICE_DRIVER,
    };

    const MORE_DATA: u32 = 234;
    // SAFETY: null names stand for this PC and its usual list of services.
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ENUMERATE_SERVICE) };
    if manager.is_null() {
        return Err(super::last_error());
    }
    let mut names = Vec::new();
    let mut resume = 0u32;
    let outcome = loop {
        // The answer is a row of records with pointers into the same buffer. A buffer of
        // 64-bit numbers is aligned for them.
        let mut buffer = vec![0u64; 32 * 1024];
        let (mut needed, mut returned) = (0u32, 0u32);
        // SAFETY: the manager is open, the buffer is passed with its true length in bytes,
        // and the three numbers are written to variables that outlive the call.
        let listed = unsafe {
            EnumServicesStatusExW(
                manager,
                SC_ENUM_PROCESS_INFO,
                SERVICE_DRIVER,
                SERVICE_ACTIVE,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * 8) as u32,
                &mut needed,
                &mut returned,
                &mut resume,
                std::ptr::null(),
            )
        } != 0;
        let why = if listed { Unknown::Skipped } else { super::last_error() };
        if !listed && why != Unknown::Windows(MORE_DATA) {
            break Err(why);
        }
        let records = buffer.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>();
        for at in 0..returned as usize {
            // SAFETY: the call wrote `returned` whole records at the start of the buffer,
            // each with a name that lies in the same buffer and ends in a zero.
            names.push(unsafe { from_wide_ptr((*records.add(at)).lpServiceName, 256) });
        }
        if listed {
            break Ok(());
        }
    };
    // SAFETY: the manager is open and is not used after this.
    unsafe { CloseServiceHandle(manager) };
    outcome.map(|()| names)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAMES: [&str; 4] = ["MapleStory.exe", "GameLauncher.exe", "MapleStoryT.exe", "GameLauncherT.exe"];

    fn sorted(names: &[&str]) -> Programs {
        sort(names.iter().map(|name| (*name).to_owned()), &GAMES)
    }

    #[test]
    fn only_names_that_matter_are_kept() {
        let found = sorted(&[
            "chrome.exe",
            "explorer.exe",
            "Discord.exe",
            "pmclient.exe",
            "pmAgent.exe",
            "DFServ.exe",
            "PFGameDiskClient.exe",
            "HDRestore.exe",
            "svchost.exe",
            "MAPLESTORY.EXE",
            "secret-project.exe",
        ]);
        assert_eq!(found.seat, ["pmAgent.exe", "pmclient.exe"]);
        assert_eq!(found.restore, ["DFServ.exe"]);
        assert_eq!(found.diskless, ["PFGameDiskClient.exe"]);
        assert_eq!(found.hints, ["HDRestore.exe"]);
        assert_eq!(found.game, ["MAPLESTORY.EXE"]);
        assert!(found.noted.is_empty());
        // Nothing else is anywhere in what comes back.
        let all = format!("{found:?}");
        for dropped in ["chrome", "explorer", "Discord", "svchost", "secret-project"] {
            assert!(!all.contains(dropped), "{dropped} was kept");
        }
    }

    #[test]
    fn name_with_a_telling_word_is_a_hint_and_one_with_a_common_word_is_only_noted() {
        let found = sorted(&[
            "iScsiPrt",
            "MyVDisk.sys",
            "GetoClient.exe",
            "FreezeGuard.exe",
            "vhdmp",
            "ahcache",
            "NVHDA",
            "PresentationFontCache.exe",
            "WdBoot",
            "WireGuard.exe",
            "Picasa3.exe",
        ]);
        assert_eq!(found.hints, ["FreezeGuard.exe", "GetoClient.exe", "iScsiPrt", "MyVDisk.sys"]);
        assert_eq!(
            found.noted,
            ["ahcache", "NVHDA", "Picasa3.exe", "PresentationFontCache.exe", "vhdmp", "WdBoot", "WireGuard.exe"]
        );
        assert!(found.diskless.is_empty() && found.restore.is_empty() && found.seat.is_empty());
    }

    #[test]
    fn name_is_kept_once_however_often_it_runs() {
        let found = sorted(&["pmclient.exe", "PMCLIENT.EXE", "pmclient.exe"]);
        assert_eq!(found.seat, ["pmclient.exe"]);
    }

    #[test]
    fn programs_and_drivers_of_this_pc_are_listed() {
        let processes = processes().expect("the running programs could not be listed");
        assert!(processes.len() > 5 && processes.iter().all(|name| !name.is_empty()));
        // This test program is one of them.
        let me = std::env::current_exe().unwrap();
        let me = me.file_name().unwrap().to_string_lossy();
        assert!(processes.iter().any(|name| name.eq_ignore_ascii_case(&me)));
        let drivers = drivers().expect("the running drivers could not be listed");
        assert!(drivers.len() > 20, "only {} drivers", drivers.len());
        // The driver of the file system Windows is on runs on every PC.
        assert!(drivers.iter().any(|name| name.eq_ignore_ascii_case("Ntfs")), "no file system driver in the list");
    }
}
