//! Which of the programs and drivers that run on this PC say something about the kind of
//! PC it is.
//!
//! Everything that runs is listed by Windows, and nearly all of it is thrown away at once:
//! what is kept is a name this crate knows as a PC room's program, the name of a driver that
//! says how the PC gets its disks, a name that holds a word such programs tend to hold, and
//! the game itself. No other name leaves this module, and
//! nothing is asked of any program beyond the name of its executable.

use crate::facts::{Fact, Programs, Unknown};
use crate::wide::{from_wide, from_wide_ptr};

/// Programs that hand a seat without a disk its disks: K하드's `PFGameDisk.exe` (the seat
/// program of K-Hard 4.0, by its release notes; a name that begins the same way is taken
/// for it) and CCBoot's client. A name counts when it begins with one of these.
const DISKLESS: [&str; 2] = ["pfgamedisk", "ccboot"];
/// Programs that put a disk back as it was at every start: Deep Freeze (`DFServ.exe`, its
/// tray program and its driver) and Reboot Restore Rx (`ShdTray.exe`, `ShdServ.exe`).
const RESTORE: [&str; 5] = ["dfserv", "frzstate2k", "deepfrz", "shdtray", "shdserv"];
/// Programs a PC room manages its seats with, and programs that only a PC room's seat
/// runs. A name counts when it is exactly one of these: several are short, and a name
/// that only begins like one of them is some other program's. Each has where it is from,
/// since the list grows with every report from a seat.
const SEAT: [&str; 16] = [
    // 피카's seat programs. `pmclient`, `PicaigreenClient` and `PicaVncSvc` ran on the
    // first seat this was run on (our seat report). `pmAgent` was in this list from its
    // start and has not been seen on a seat.
    "pmclient",
    "pmagent",
    "picaigreenclient",
    "picavncsvc",
    // 엔미디어플랫폼's 그린가드, the program that blocks content on a seat and ships for 게토
    // shops, as `PicaigreenClient` does for 피카's (the vendor's manual). It ran on the
    // second seat, a 게토 room's (our seat report).
    "greenguard",
    // 게토 `#`, the customer's side (file listing of the vendor's installer of 2026-08-24).
    "wmclt",
    "wmcltsvc",
    "wmcltlc",
    "wmcltus",
    // 게토 `G`, the customer's side (file listing of the vendor's installer of 2023-03-28).
    "dlx5",
    "dlxsvc",
    "drcsvc",
    "vcdgetosvc",
    // K-Hard's program that keeps the games on a seat's own disk in step with the server
    // (the vendor's release notes). It runs beside `PFGameDisk.exe`.
    "pfgamesyncmgr",
    // The launchers customers see: 게임팟's (the vendor's release notes) and 쎈피씨's (a
    // screen in the vendor's own material).
    "pfdesktopclient",
    "ssenpc_gamelauncher",
];
/// 게토's names among those above.
const GETO: [&str; 8] = ["wmclt", "wmcltsvc", "wmcltlc", "wmcltus", "dlx5", "dlxsvc", "drcsvc", "vcdgetosvc"];
/// Two more files of the installer of 게토 `G`, a program and a driver, whose names are too
/// plain to go by: `dlc` is what many programs call a downloader or added content, and a
/// driver called `PGuard` could be anybody's. Either counts as 게토's only where another of
/// 게토's names runs beside it.
const GETO_BESIDE_ANOTHER: [&str; 2] = ["dlc", "pguard"];
/// The driver of NetZone's NxD, which hands a seat its disks over the network: its service
/// is called `nxddsk` and its file `nxddsk22.sys`, with `NxD Virtual Disk Drive Driver`
/// for the name shown. These are from an older signed sample, built in 2014. The one seat
/// of that product this was run on showed the name of its disks, `NetZone NxDDisk`, and no
/// driver by either name was looked for then: whether a seat of today loads them is not
/// known. A name counts when it is exactly one of these.
const NETDISK: [&str; 2] = ["nxddsk", "nxddsk22"];
/// The port driver of the iSCSI initiator that comes with Windows. It is what reaches a
/// disk another computer hands over by iSCSI, and it is loaded on demand: on a PC without
/// such a disk it is not running. (Looked up on an ordinary Windows 11: the service is set
/// to start on demand and is stopped.)
const ISCSI_PORT: [&str; 1] = ["iscsiprt"];
/// Storage drivers that come with Windows for adapters only servers have: Chelsio, 3ware,
/// Areca, Adaptec, LSI MegaRAID, Dell PERC, Promise. Windows registers each to start with
/// the system and, once it has started without finding its adapter, turns it down to
/// start on demand (a `StartOverride` in the registry). So an ordinary PC loads none of
/// them, which was looked up on an ordinary Windows 11: all are registered, turned down,
/// and stopped. One that is loaded all the same belongs to a Windows that was prepared to
/// start on other PCs and was not left to settle. That says nothing about iSCSI, although
/// one of them, `cht4iscsi`, has it in its name. A name counts when it begins with one of
/// these.
const SPARE: [&str; 9] =
    ["cht4iscsi", "3ware", "arcsas", "adp80xx", "megasas", "megasr", "percsas", "stexstor", "mpi3drvi"];
/// Words in a name that few programs other than a PC room's hold. The names of 지매니저 and
/// 하드보안관 are not known, and those of other products only in part, so this is what
/// would catch them. A name with one of these keeps a PC from being called an ordinary one.
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
    let mut beside_another = Vec::new();
    for name in names {
        let stem = stem(&name);
        let begins = |list: &[&str]| list.iter().any(|known| stem.starts_with(known));
        let holds = |list: &[&str]| list.iter().any(|word| stem.contains(word));
        let is_one_of = |list: &[&str]| list.contains(&stem.as_str());
        let list = if games.iter().any(|game| game.eq_ignore_ascii_case(&name)) {
            &mut found.game
        } else if begins(&DISKLESS) {
            &mut found.diskless
        } else if begins(&RESTORE) {
            &mut found.restore
        } else if is_one_of(&SEAT) {
            &mut found.seat
        } else if is_one_of(&GETO_BESIDE_ANOTHER) {
            // Whose it is shows when every name was looked at.
            beside_another.push(name);
            continue;
        } else if is_one_of(&ISCSI_PORT) {
            &mut found.iscsi
        } else if is_one_of(&NETDISK) {
            &mut found.netdisk
        } else if begins(&SPARE) {
            &mut found.spare
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
    // A name too plain to go by is 게토's where another of 게토's names runs. Alone it is
    // dropped like any other name, or noted for the common word in it.
    let geto_runs = found.seat.iter().any(|name| GETO.contains(&stem(name).as_str()));
    for name in beside_another {
        let common = COMMON.iter().any(|word| stem(&name).contains(word));
        let list = match (geto_runs, common) {
            (true, _) => &mut found.seat,
            (false, true) => &mut found.noted,
            (false, false) => continue,
        };
        if !list.iter().any(|listed: &String| listed.eq_ignore_ascii_case(&name)) {
            list.push(name);
        }
    }
    for list in [
        &mut found.diskless,
        &mut found.restore,
        &mut found.seat,
        &mut found.iscsi,
        &mut found.netdisk,
        &mut found.spare,
        &mut found.hints,
        &mut found.noted,
        &mut found.game,
    ] {
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
            "msiscsiHelper.exe",
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
        assert_eq!(found.hints, ["FreezeGuard.exe", "GetoClient.exe", "msiscsiHelper.exe", "MyVDisk.sys"]);
        assert_eq!(
            found.noted,
            ["ahcache", "NVHDA", "Picasa3.exe", "PresentationFontCache.exe", "vhdmp", "WdBoot", "WireGuard.exe"]
        );
        assert!(found.diskless.is_empty() && found.restore.is_empty() && found.seat.is_empty());
        assert!(found.iscsi.is_empty() && found.spare.is_empty());
    }

    /// The names the research into the products of PC rooms brought: each is known for
    /// what it is by its exact name, and a name that only looks like one is not.
    #[test]
    fn names_from_the_vendors_own_files_are_known_by_their_exact_names() {
        // 게토 `#`.
        let sharp = sorted(&["WmClt.exe", "WmCltSvc.exe", "WmCltLC.exe", "WmCLTUS.exe", "explorer.exe"]);
        assert_eq!(sharp.seat, ["WmClt.exe", "WmCltLC.exe", "WmCltSvc.exe", "WmCLTUS.exe"]);
        // 게토 `G`, with its two plain names beside the others: the driver comes from the
        // list of drivers, which names it without an ending.
        let g = sorted(&["dlc.exe", "dlx5.exe", "DlxSvc.exe", "DRCSvc.exe", "VCDGetoSvc.exe", "PGuard"]);
        assert_eq!(g.seat, ["dlc.exe", "dlx5.exe", "DlxSvc.exe", "DRCSvc.exe", "PGuard", "VCDGetoSvc.exe"]);
        assert!(g.hints.is_empty() && g.noted.is_empty(), "{g:?}");
        // The two plain names without another of 게토's: a downloader of some game, and a
        // driver of somebody's. The first is dropped like any name, the second is noted
        // for the common word in it, and neither is a PC room's.
        let plain = sorted(&["dlc.exe", "PGuard.sys", "GreenGuard.exe", "pmclient.exe"]);
        assert_eq!(
            (plain.seat, plain.noted),
            (vec!["GreenGuard.exe".to_owned(), "pmclient.exe".to_owned()], vec!["PGuard.sys".to_owned()])
        );
        assert!(!format!("{:?}", sorted(&["dlc.exe"])).contains("dlc"));
        // K-Hard's two, the launchers, and the driver of NetZone's disks by its service
        // and by its file.
        let others = sorted(&[
            "PFGameDisk.exe",
            "PFGameSyncMgr.exe",
            "PFDesktopClient.exe",
            "SSENPC_GAMELAUNCHER.exe",
            "nxddsk",
            "nxddsk22.sys",
        ]);
        assert_eq!(others.diskless, ["PFGameDisk.exe"]);
        assert_eq!(others.seat, ["PFDesktopClient.exe", "PFGameSyncMgr.exe", "SSENPC_GAMELAUNCHER.exe"]);
        assert_eq!(others.netdisk, ["nxddsk", "nxddsk22.sys"]);
        // Names that only look like those: none is a PC room's program or a disk driver.
        let alike = sorted(&[
            "WmClient.exe",
            "WmCltHelper.exe",
            "dlcore.exe",
            "dlx.exe",
            "dlx50.exe",
            "DRCService.exe",
            "pmclient2.exe",
            "PicaVncViewer.exe",
            "GreenGuardian.exe",
            "PFDesktop.exe",
            "SSENPC.exe",
            "nxddskmon.exe",
            "nxd.sys",
        ]);
        assert!(alike.seat.is_empty() && alike.netdisk.is_empty() && alike.diskless.is_empty(), "{alike:?}");
        // Two of them hold a word that is worth a look all the same.
        assert_eq!(
            (alike.hints, alike.noted),
            (vec!["PicaVncViewer.exe".to_owned()], vec!["GreenGuardian.exe".to_owned()])
        );
    }

    /// What ran on the second PC room seat, a 게토 room's.
    #[test]
    fn names_seen_on_the_second_pc_room_seat_are_sorted_by_what_they_are() {
        let found = sorted(&["GreenGuard.exe", "ShaderCacheClient.exe", "ahcache", "iScsiPrt", "MapleStory.exe"]);
        // The seat program of that room, though its name holds a word many names hold.
        assert_eq!(found.seat, ["GreenGuard.exe"]);
        // What is not known stays among the names that are listed and decide nothing.
        assert_eq!(found.noted, ["ahcache", "ShaderCacheClient.exe"]);
        assert_eq!((found.iscsi, found.game), (vec!["iScsiPrt".to_owned()], vec!["MapleStory.exe".to_owned()]));
        assert!(found.hints.is_empty() && found.diskless.is_empty() && found.restore.is_empty());
    }

    /// What ran on the PC room seat this diagnostic was first run on.
    #[test]
    fn names_seen_on_a_pc_room_seat_are_sorted_by_what_they_are() {
        let found = sorted(&[
            "pmclient.exe",
            "PicaigreenClient.exe",
            "PicaVncSvc.exe",
            "iScsiPrt",
            "cht4iscsi",
            "ahcache",
            "NVHDA",
            "explorer.exe",
        ]);
        assert_eq!(found.seat, ["PicaigreenClient.exe", "PicaVncSvc.exe", "pmclient.exe"]);
        // The port driver of Windows' iSCSI initiator is its own kind, and the adapter
        // driver that only has iSCSI in its name is not taken for it.
        assert_eq!(found.iscsi, ["iScsiPrt"]);
        assert_eq!(found.spare, ["cht4iscsi"]);
        assert_eq!(found.noted, ["ahcache", "NVHDA"]);
        assert!(found.hints.is_empty() && found.diskless.is_empty() && found.restore.is_empty());
        // Other drivers for adapters a desktop does not have are of the same kind.
        assert_eq!(
            sorted(&["megasas35i", "3ware", "percsas3i", "storahci", "stornvme"]).spare,
            ["3ware", "megasas35i", "percsas3i"]
        );
        // Another program with the first letters of 피카's is not one of 피카's.
        let other = sorted(&["Picasa3.exe", "PicaOther.exe"]);
        assert_eq!(
            (other.seat.len(), other.hints, other.noted),
            (0, vec!["PicaOther.exe".to_owned()], vec!["Picasa3.exe".to_owned()])
        );
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
