//! What Nexon's server has for an install: which patches are published, and which of them
//! this install can take.
//!
//! The server is asked through a [`Shelf`], so that the choice of a patch can be tried on a
//! made-up shelf of patches, without a network.
//!
//! How the version of a folder is proven from the folder's own files is in [`proof`].

mod proof;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mfp_engine::container::PatchFile;
use mfp_engine::detect::{self, Checksums};
use mfp_engine::preflight::{self, Problem};
use mfp_engine::{Error, download, plan, stage};
use mfp_kms::Client;
use tauri::AppHandle;

use self::proof::{Proof, prove};
use crate::ready::unreadable_journal;
use crate::records::{
    at_final, install_dir, install_mark, installed_version, known_minor, latest_seen, patch_cache, record_minor,
    run_target, see_latest, settle_version, version_proven,
};
use crate::state::{Availability, Found, Patcher, Site, Target};
use crate::window::notify;
use crate::wording::{PUT_BACK_FIRST, describe_check_error, describe_problem, drive_letter};

/// What a check asks of the server a client's patch files are published on.
pub(crate) trait Shelf {
    /// How large the file at `url` is; `None` when the server says it has no such file.
    /// Only the server saying so counts as "no": a failed connection is an error, not a
    /// missing patch.
    fn size_of(&self, url: &str) -> Result<Option<u64>, Error>;

    /// The checksums of the original files the patch file at `url` was made from; see
    /// [`detect::remote_originals`]. `None` when the patch file has no such table.
    fn originals_of(&self, url: &str) -> Result<Option<BTreeMap<String, u32>>, Error>;

    /// Fetches the patch from minor `from` to `to` of main version `version` and reads
    /// what it does.
    fn minor(&self, version: u32, from: u32, to: u32) -> Result<Box<dyn Minor>, Error>;
}

/// A minor patch file that has been fetched and read.
pub(crate) trait Minor {
    fn size(&self) -> u64;

    /// Whether everything this patch writes is already in the install folder.
    fn in_place(&self, install: &Path, stop: &AtomicBool) -> Result<bool, Error>;

    /// Whether the patch names files it starts from. A run of such a patch compares the
    /// folder with them before it changes anything, so the patch cannot be laid over a
    /// folder it was not made for. A patch that only holds whole files, folders to make
    /// and things to delete names none, and fits any folder.
    fn checks_folder(&self) -> bool;
}

/// The server the patch files of a site's client are published on: Nexon's, or what a
/// build made for trying the app out puts in its place.
pub(crate) struct Published<'a> {
    site: &'a Site,
}

impl<'a> Published<'a> {
    pub(crate) fn of(site: &'a Site) -> Published<'a> {
        Published { site }
    }
}

impl Shelf for Published<'_> {
    /// A check makes a handful of these questions in a row. Whether it was told to give up
    /// is looked at before each, so that it does so at the next question instead of asking
    /// them all.
    fn size_of(&self, url: &str) -> Result<Option<u64>, Error> {
        if self.site.check_stop.load(Ordering::SeqCst) {
            return Err(Error::Stopped);
        }
        match download::probe(url) {
            Ok(remote) => Ok(Some(remote.len)),
            Err(Error::NotPublished) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// The start of the file is fetched and read in memory: nothing is written for it.
    fn originals_of(&self, url: &str) -> Result<Option<BTreeMap<String, u32>>, Error> {
        detect::remote_originals(url)
    }

    fn minor(&self, version: u32, from: u32, to: u32) -> Result<Box<dyn Minor>, Error> {
        Ok(Box::new(MinorStep::read(self.site, version, from, to, &self.site.check_stop)?))
    }
}

/// Turns down a download into the cache that the cache's drive has no room for.
///
/// The cache is on the system drive whatever drive the game is on. When that drive is full,
/// the check says so, with the drive, instead of failing without a reason anyone can act on.
fn cache_room(cache: &Path, bytes: u64) -> Result<(), Error> {
    match preflight::free_space(cache) {
        Ok(available) if available < bytes => Err(Error::Blocked(vec![Problem::NotEnoughSpace {
            volume: drive_letter(cache).unwrap_or_default(),
            needed: bytes,
            available,
        }])),
        // Not being able to tell is no reason to refuse: a download that does not fit fails
        // by itself.
        _ => Ok(()),
    }
}

/// A version of `client` as it is shown, written as Nexon's notices write it: `Ver 1.2.422`.
pub(crate) fn version_name(client: &Client, version: u32) -> String {
    format!("Ver {}", client.version_label(version))
}

/// A version of `client` with its minor version, which is left out while it is still the
/// first. It is written as Nexon's notices write it: `Ver 1.2.421 마이너버전(4)`.
pub(crate) fn full_label(client: &Client, version: u32, minor: u32) -> String {
    let label = version_name(client, version);
    if minor > 1 { format!("{label} 마이너버전({minor})") } else { label }
}

/// Minor versions are two digits in the patch addresses.
const LAST_MINOR: u32 = 99;
/// How many main versions above the one it starts from the newest is looked for.
const MAINS_AHEAD: u32 = 20;
/// How many main versions a patch that skips versions leads over at most: for each new
/// version Nexon publishes one from each of the five before it. A folder's version is
/// looked for that far back from the newest, and a folder that is further behind is taken
/// that far on at a time.
const REACH: u32 = 5;

/// The newest minor version the server has a patch to, for main version `version`.
fn newest_minor(site: &Site, version: u32, shelf: &dyn Shelf) -> Result<u32, Error> {
    let mut newest = 1;
    while newest < LAST_MINOR && shelf.size_of(&site.minor_patch_url(version, newest, newest + 1))?.is_some() {
        newest += 1;
    }
    Ok(newest)
}

/// The newest main version on the server, going up from `version` and no further than
/// `most`: main patches are followed for as long as there is a next one.
fn newest_main(site: &Site, version: u32, most: u32, shelf: &dyn Shelf) -> Result<u32, Error> {
    let mut newest = version;
    while newest < most && shelf.size_of(&site.patch_url(newest, newest + 1))?.is_some() {
        newest += 1;
    }
    Ok(newest)
}

/// The main patch that leads on from `version`, where `newest` is the newest main version
/// the server has and `step` the size of the patch to the version after `version`, if the
/// server has that one.
///
/// Nexon publishes patches that skip versions next to the single steps: one to each new
/// version from each of the few before it; see [`REACH`]. A folder that is more than one
/// version behind takes the one that leads furthest: straight to the newest where that is
/// within reach, and as far towards it as the server has a patch for otherwise. It is
/// rewritten once instead of once for every version in between. Which patch it is goes by
/// what the server has and by nothing in the folder: reading the folder's files to choose
/// would take minutes, and the patch that is chosen names what it starts from, which its
/// run holds the folder against before it changes anything. The server is asked for the
/// furthest first, so that it is one question where Nexon has published as it does.
///
/// A patch that skips versions starts where the single step does: from the last minor of
/// `version`. Minor patches of `version` that the folder still needs come before either.
fn onward(
    site: &Site,
    version: u32,
    newest: u32,
    step: Option<u64>,
    shelf: &dyn Shelf,
) -> Result<Option<Target>, Error> {
    let main_to = |to: u32, size: u64| Target {
        label: version_name(site.client, to),
        url: site.patch_url(version, to),
        file_name: site.client.patch_file_name(version, to),
        size,
        version: to,
        minor: None,
    };
    for to in (version + 2..=newest.min(version + REACH)).rev() {
        if let Some(size) = shelf.size_of(&site.patch_url(version, to))? {
            return Ok(Some(main_to(to, size)));
        }
    }
    Ok(step.map(|size| main_to(version + 1, size)))
}

/// How a check ended, which is what the time of the next one goes by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckEnd {
    /// It said what there is to say: what the server has for this install, that the install
    /// fits nothing the server has, or that there is nothing to ask about.
    Answered,
    /// The server could not be asked. What was found before stands, with the reason.
    Failed,
    /// The folder is being put back after an apply that was cut off. Until it is, nothing
    /// can be said of it, and that is the answer for now.
    NotYet,
    /// It gave up before it had an answer: it was told to, or a run began.
    Stopped,
}

/// What a check found, and how it ended.
pub(crate) struct Checked {
    pub(crate) answer: Availability,
    pub(crate) end: CheckEnd,
}

impl Checked {
    fn answered(answer: Availability) -> Checked {
        Checked { answer, end: CheckEnd::Answered }
    }

    fn failed(answer: Availability) -> Checked {
        Checked { answer, end: CheckEnd::Failed }
    }

    fn stopped(answer: Availability) -> Checked {
        Checked { answer, end: CheckEnd::Stopped }
    }
}

/// Asks the server what is published for the client of `site` and works out which patch
/// its install can take.
///
/// Asking is cheap and happens every time. Working out the target can mean downloading a
/// minor patch, so it is only redone when the server offers something new.
pub(crate) fn availability(app: &AppHandle, patcher: &Patcher, site: &Site) -> Checked {
    let mut appeared = None;
    let checked = check(patcher, site, &Published::of(site), &mut appeared);
    // Tell the user when a patch appears while the app sits waiting for it. Not on the first
    // look: then the ready screen already shows it.
    if let Some(target) = appeared {
        notify(app, site, &registered(&target));
    }
    checked
}

/// What is said of a patch that has appeared on the server: the version it leads to and
/// which kind of patch it is, as in `Ver 1.2.422 클라이언트 패치가 등록되었습니다.` A minor
/// version says in its name that it is one, as Nexon's notices write it:
/// `Ver 1.2.421 마이너버전(4) 패치가 등록되었습니다.`
fn registered(target: &Target) -> String {
    match target.minor {
        Some(_) => format!("{} 패치가 등록되었습니다.", target.label),
        None => format!("{} {}가 등록되었습니다.", target.label, target.kind()),
    }
}

/// What the footer says of a folder whose version nothing proves.
const UNPROVEN: &str = "현재 버전 확인 불가";

/// The check behind [`availability`], with the server asked through `shelf`. `appeared` is
/// set to the patch for the install that was not there at the check before.
fn check(patcher: &Patcher, site: &Site, shelf: &dyn Shelf, appeared: &mut Option<Target>) -> Checked {
    let nothing = |error: Option<String>| Availability { error, ..Availability::nothing() };
    let Some(install) = install_dir(site) else { return Checked::answered(nothing(None)) };
    // An apply was cut off in the folder, the record it kept cannot be read, and the folder
    // could not be put back without it either. It is never undone, and nothing is worked
    // out of a folder in that state. That is the answer for as long as what the run left
    // is there, and nothing is waited for.
    if let Some(problem) = unreadable_journal(&install) {
        return Checked::answered(nothing(Some(describe_problem(&problem))));
    }
    // A run that replaced game files as it built them was cut off, and the folder is partly
    // new. What this app remembers about a folder (its version, its minor, what the last
    // check found) goes by files that are no longer the ones it knew, so nothing about the
    // folder is worked out in this state. The patch to offer is the one that run was
    // applying. A run by an address the user gave is offered again by the status instead.
    //
    // This comes before the look for an apply to undo: such a folder is not put back as
    // it was, and nothing on the screen is to read as if it were.
    if mfp_engine::run::half_patched(&install) {
        return Checked::answered(match run_target(site, &install) {
            Some(target) => Availability { latest: Some(target.label.clone()), target: Some(target), error: None },
            None => nothing(Some("이전 패치 미완료".to_owned())),
        });
    }
    // An apply that was cut off is being undone, or could not be yet. Until the folder is
    // back as it was, nothing about it can be worked out; least of all "no patch needed".
    if mfp_engine::run::cut_off(&install) {
        return Checked { answer: nothing(Some(PUT_BACK_FIRST.to_owned())), end: CheckEnd::NotYet };
    }
    // A folder that is not a game folder has no version to work out.
    if !site.programs().check_install_dir(&install).is_empty() {
        return Checked::answered(nothing(None));
    }
    // What was found before stands while a run is at work on the same files.
    let during_run = || Checked::stopped(site.found_for(&install).map_or(nothing(None), |found| found.availability()));
    if patcher.running_for(site) {
        return during_run();
    }
    // One check at a time. A second one waits here and then finds the first one's answer.
    let _busy = site.busy.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if patcher.running_for(site) {
        return during_run();
    }
    site.check_stop.store(false, Ordering::SeqCst);
    // A run that begins tells every check to give up, this client's and the other's alike,
    // and may have done so the moment before the line above: its word would be lost there,
    // and this check would go on asking the server and reading its folder beside the run.
    // A run is at work before it gives that word (see `patching::begin`), so whichever of
    // the two came first, the run is seen here or its word stands. Seen here, the word is
    // put back for whoever else goes by it.
    if patcher.running().is_some() {
        site.check_stop.store(true, Ordering::SeqCst);
        return during_run();
    }
    // The checksums read so far are of the folder the last check was about. Those of a
    // folder the user has left would stay for as long as the app is open, to no use.
    {
        let mut checksums = site.checksums.lock().unwrap();
        if checksums.0 != install {
            *checksums = (install.clone(), Checksums::default());
        }
    }

    let failed = |err: &Error| {
        let failure = format!("check failed: {err}");
        let mut written = site.check_failure.lock().unwrap();
        if written.as_deref() != Some(failure.as_str()) {
            site.log(&failure);
            *written = Some(failure);
        }
        describe_check_error(err)
    };
    // A check that was told to give up has no answer of its own; what was found before, if
    // anything, still stands.
    let given_up = || {
        let stands = site.found_for(&install).map(|found| found.availability());
        Checked::stopped(stands.unwrap_or_else(|| nothing(Some(describe_check_error(&Error::Stopped)))))
    };
    match settle_version(site, &install, shelf) {
        Ok(()) => {}
        Err(Error::Stopped) => return given_up(),
        Err(err) => return Checked::failed(nothing(Some(failed(&err)))),
    }
    let known = site.found_for(&install);
    // When the server cannot be asked, what was found before is still the best answer.
    let unreachable = |err: &Error| {
        if matches!(err, Error::Stopped) {
            return given_up();
        }
        let mut answer = known.as_ref().map_or(nothing(None), |found| found.availability());
        answer.error = Some(failed(err));
        Checked::failed(answer)
    };
    let mut version = match installed_version(site, &install) {
        Some(version) => version,
        // Nothing on this PC says which version the folder is at, and this app has no
        // record of it: the folder's own files have to prove it.
        None => match prove(site, &install, shelf, None) {
            Ok(Proof::Found(version)) => version,
            // Without a proof nothing is said of the folder, and nothing is offered for it.
            Ok(Proof::None { latest }) => {
                return Checked::answered(Availability { target: None, latest, error: Some(UNPROVEN.to_owned()) });
            }
            Err(err) => return unreachable(&err),
        },
    };

    // What the registry says of the version is where this starts, and may be wrong. Where
    // it matters that it is right, a proof is looked for, once, and the check is made
    // again from the version that was proven.
    let mut looked_for_proof = false;
    let (main, newest_minor, newest, mark, latest, target, error) = loop {
        let offered = (|| -> Result<_, Error> {
            Ok((shelf.size_of(&site.patch_url(version, version + 1))?, newest_minor(site, version, shelf)?))
        })();
        let (main, newest_minor) = match offered {
            Ok(offered) => offered,
            Err(err) => return unreachable(&err),
        };

        // The answer holds while neither the server nor the folder has changed. The folder
        // changes when something else patches or repairs the game.
        let mark = install_mark(site, &install);
        let same = |found: &&Found| {
            (found.version, found.main, found.newest_minor, &found.mark)
                == (version, main.is_some(), newest_minor, &mark)
        };
        if let Some(found) = known.as_ref().filter(same) {
            if found.error.is_none() {
                note_check_worked(site);
            }
            return Checked::answered(found.availability());
        }
        // The newest version on the server: main patches are followed for as long as there
        // is a next one, then the minors of the last.
        let latest = (|| -> Result<_, Error> {
            match main {
                Some(_) => {
                    let newest = newest_main(site, version + 1, version + MAINS_AHEAD, shelf)?;
                    Ok((newest, self::newest_minor(site, newest, shelf)?))
                }
                None => Ok((version, newest_minor)),
            }
        })();
        let (newest, minor_of_newest) = match latest {
            Ok(latest) => latest,
            Err(err) => return unreachable(&err),
        };
        see_latest(site, newest);
        // The version of a folder that no hint speaks for was proven against the newest
        // version the server was seen to have. Where the single steps from the folder's
        // version do not lead all the way there, that version is still the newest there is.
        let ahead = latest_seen(site).filter(|seen| site.version_guess().is_none() && *seen > newest);
        let latest = match ahead {
            Some(seen) => version_name(site.client, seen),
            None => full_label(site.client, newest, minor_of_newest),
        };

        // The main patch the folder takes once it is at the last minor of its version: the
        // one that leads straight to the newest version where the server has it, and the
        // step to the next version otherwise.
        let onward = match onward(site, version, ahead.unwrap_or(newest), main, shelf) {
            Ok(onward) => onward,
            Err(err) => return unreachable(&err),
        };

        let (target, error) = if ahead.is_some() && onward.is_none() {
            // With no patch to take from the folder's version, there is no way from it to
            // the newest that this app takes, and the folder is not called up to date for it.
            (None, Some(failed(&Error::NotPublished)))
        } else {
            match pending_minor(site, &install, version, newest_minor, shelf) {
                Ok(Pending::Patch(minor)) => (Some(minor), None),
                Ok(Pending::Nothing) => (onward, None),
                // The patch there is fits any folder, and the folder is only said to be at
                // this version. Its own files have to prove which version it is at.
                Ok(Pending::Unproven) if !looked_for_proof => {
                    looked_for_proof = true;
                    match prove(site, &install, shelf, Some(version)) {
                        Ok(Proof::Found(proven)) => {
                            version = installed_version(site, &install).unwrap_or(proven);
                            continue;
                        }
                        Ok(Proof::None { .. }) => (None, Some(UNPROVEN.to_owned())),
                        Err(err) => return unreachable(&err),
                    }
                }
                // Nothing is offered on a guess. This too stays true until the server or
                // the folder changes, and is remembered.
                Ok(Pending::Unproven) => (None, Some(UNPROVEN.to_owned())),
                // The install matches nothing the server offers, or what the server offers
                // is in a form this app cannot read. That stays true until the server or
                // the folder changes, so it is remembered like any other answer instead of
                // fetching the same patch files again on every check.
                Err(err @ (Error::OriginalMismatch(_) | Error::UnknownForm(_))) => (None, Some(failed(&err))),
                Err(Error::Stopped) => return given_up(),
                // Anything else may pass; the next check tries again.
                Err(err) => {
                    let answer = Availability { target: None, latest: Some(latest), error: Some(failed(&err)) };
                    return Checked::failed(answer);
                }
            }
        };
        let newest = ahead.is_none().then_some((newest, minor_of_newest));
        break (main, newest_minor, newest, mark, latest, target, error);
    };

    // The user may have picked another folder while this check was at work. Its answer is
    // then about the wrong folder and is dropped.
    if install_dir(site).as_deref() != Some(install.as_path()) {
        return Checked::stopped(nothing(Some(describe_check_error(&Error::Stopped))));
    }
    let found = Found {
        install: install.clone(),
        mark,
        version,
        main: main.is_some(),
        newest_minor,
        newest,
        target,
        latest,
        error,
    };
    if found.error.is_none() {
        note_check_worked(site);
    }
    tidy_cache(site, found.target.as_ref());
    if let (Some(target), Some(before)) = (&found.target, &known)
        && before.target.is_none()
    {
        *appeared = Some(target.clone());
    }
    let answer = found.availability();
    *site.found.lock().unwrap() = Some(found);
    Checked::answered(answer)
}

/// Writes to the log that checks work again, once, after a failure was written there.
fn note_check_worked(site: &Site) {
    if site.check_failure.lock().unwrap().take().is_some() {
        site.log("check works again");
    }
}

/// Empties the cache of patch files, once a check has made its use of them.
///
/// Everything in it was fetched only to tell which patch an install needs, or was left by
/// an earlier version of this app, which kept its downloads here. One file stays: a patch
/// that was read in full and found to be the one the install needs. The run that applies
/// it takes it from here (see [`adopt_checked`]), and until then the next start of the app
/// does not fetch it again to find out the same.
fn tidy_cache(site: &Site, target: Option<&Target>) {
    let Ok(files) = std::fs::read_dir(patch_cache(site)) else { return };
    let wanted = target.map(|target| target.file_name.as_str());
    for file in files.flatten() {
        let name = file.file_name().to_string_lossy().into_owned();
        let rest = wanted.and_then(|wanted| name.strip_prefix(wanted));
        if name.contains("-minor-") && matches!(rest, Some("" | ".tag")) {
            continue;
        }
        let _ = std::fs::remove_file(file.path());
    }
}

/// Hands a run the patch file a check has already downloaded, when it is the one the run
/// is about to download to `dest`. Says whether it did.
///
/// A check sometimes has to read a minor patch in full to tell that the install needs it,
/// and keeps it in the cache. The run looks for its patch file in the install folder, so
/// without this the same file would be downloaded twice. It is put there together with its
/// tag, which says which file on the server it is: the run then compares the tag with the
/// server, as for any download it takes up again, and finds this one done.
///
/// Nothing here has to work. Whatever is not there afterwards, the run downloads.
pub(crate) fn adopt_checked(site: &Site, dest: &Path) -> bool {
    let beside = |file: &Path, ending: &str| {
        let mut name = file.as_os_str().to_owned();
        name.push(ending);
        PathBuf::from(name)
    };
    let (Some(name), Some(folder)) = (dest.file_name(), dest.parent()) else { return false };
    let checked = patch_cache(site).join(name);
    // Only into a folder that has nothing of this download yet. Part of one that is there
    // belongs to the tag that is there, and must not come under another.
    if dest.exists() || download::part_path(dest).exists() || !checked.is_file() || !beside(&checked, ".tag").is_file()
    {
        return false;
    }
    // The tag goes first. A tag without its file only means a download from the beginning.
    if std::fs::create_dir_all(folder)
        .and_then(|()| std::fs::copy(beside(&checked, ".tag"), beside(dest, ".tag")))
        .is_err()
    {
        return false;
    }
    // Within one drive the file is moved, which takes neither time nor room.
    if std::fs::rename(&checked, dest).is_ok() {
        let _ = std::fs::remove_file(beside(&checked, ".tag"));
        return true;
    }
    // To another drive it is copied, under another name until all of it is there: a file
    // under the name the run looks for must never be half of one.
    let copying = beside(dest, ".copy");
    let copied = std::fs::copy(&checked, &copying).and_then(|_| std::fs::rename(&copying, dest)).is_ok();
    if !copied {
        let _ = std::fs::remove_file(&copying);
    }
    copied
}

/// A minor patch file that has been downloaded and read.
struct MinorStep {
    size: u64,
    plan: plan::Plan,
}

impl MinorStep {
    /// Downloads the patch from minor `from` to `to` and reads what it does.
    fn read(site: &Site, version: u32, from: u32, to: u32, stop: &AtomicBool) -> Result<MinorStep, Error> {
        let url = site.minor_patch_url(version, from, to);
        let file_name = site.client.minor_patch_file_name(version, from, to);
        let cache = patch_cache(site);
        let path = cache.join(&file_name);
        // The cache's drive has to have room for what of the file is still to come.
        let remote = download::probe_patiently(&url, download::Patience::default(), stop)?;
        cache_room(&cache, remote.len.saturating_sub(download::bytes_on_disk(&path, &remote)))?;
        let size = download::fetch(&url, &path, stop, &mut |_, _| {})?.len;
        // A download that arrived damaged is thrown away, so the next check fetches it
        // afresh. A file that arrived whole but cannot be read stays: fetching it again
        // would bring the same file, every thirty seconds.
        let patch = PatchFile::open(&path).and_then(|patch| patch.verify(stop, |_| {}).map(|()| patch));
        let patch = patch.inspect_err(|err| {
            if matches!(err, Error::CorruptPatch(_) | Error::NotAPatchFile) {
                download::discard(&path);
            }
        })?;
        // A file that passed its checksum is what its maker made. If it still cannot be
        // read, the trouble is the form it is in, and a run would say the same of it.
        let plan = plan::scan(&patch, stop).map_err(|err| match err {
            Error::CorruptPatch(detail) => Error::UnknownForm(detail),
            other => other,
        })?;
        Ok(MinorStep { size, plan })
    }
}

impl Minor for MinorStep {
    fn size(&self) -> u64 {
        self.size
    }

    fn in_place(&self, install: &Path, stop: &AtomicBool) -> Result<bool, Error> {
        match stage::check_originals(&self.plan, install, stop, &|_, _| {}) {
            Ok(checked) => Ok(!stage::changes_install(&self.plan, &checked, install)),
            // The install is not at the minor this patch starts from.
            Err(Error::OriginalMismatch(_)) => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Every entry that builds a file from one that is there is held against the checksum
    /// the patch file gives for that file.
    fn checks_folder(&self) -> bool {
        self.plan.entries.iter().any(|entry| matches!(entry.action, plan::Action::Rebuild { .. }))
    }
}

/// What an install still needs of the minor patches of the version it is taken to be at.
enum Pending {
    /// Nothing: it is at the newest minor, or the version has none.
    Nothing,
    Patch(Target),
    /// The patch that would be its next does not say which files it starts from, and
    /// nothing proves that the folder is at the version the patch is for.
    Unproven,
}

/// The minor patch this install still needs, if any.
///
/// The game records its minor version nowhere readable, so the newest minor patch is read in
/// full and compared with the install: if every file it writes is already in place, the
/// install is up to date. That answer is remembered per install folder and minor version.
///
/// `version` may be no more than what the registry says, which is a guess. On a guess a
/// patch is only offered when it names the files it starts from: the run then compares the
/// folder with them, and that is the proof. A patch that names none is for a folder that
/// is known to be at `version` (see [`version_proven`]), and for no other.
fn pending_minor(site: &Site, install: &Path, version: u32, newest: u32, shelf: &dyn Shelf) -> Result<Pending, Error> {
    let known = known_minor(site, install, version);
    if newest <= 1 || known == Some(newest) || at_final(site, install, version) {
        return Ok(Pending::Nothing);
    }
    let mismatch = || Err(Error::OriginalMismatch(Vec::new()));
    let jump = |from: u32, size: u64| Target {
        label: full_label(site.client, version, newest),
        url: site.minor_patch_url(version, from, newest),
        file_name: site.client.minor_patch_file_name(version, from, newest),
        size,
        version,
        minor: Some(newest),
    };
    // The minor this install is at was worked out before and its files have not changed
    // since. The patch from there is the one; nothing has to be downloaded to know that.
    if let Some(at) = known.filter(|at| *at < newest) {
        return match shelf.size_of(&site.minor_patch_url(version, at, newest))? {
            Some(size) => Ok(Pending::Patch(jump(at, size))),
            None => mismatch(),
        };
    }

    let stop: &AtomicBool = &site.check_stop;
    let last_step = shelf.minor(version, newest - 1, newest)?;
    if last_step.in_place(install, stop)? {
        record_minor(site, install, version, newest);
        return Ok(Pending::Nothing);
    }

    // Patches that jump several minors usually state which files they start from; use the
    // first one whose starting files are the ones on disk.
    let mut ruled_out = Vec::new();
    {
        let mut checksums = site.checksums.lock().unwrap();
        for from in 1..newest - 1 {
            let url = site.minor_patch_url(version, from, newest);
            match shelf.originals_of(&url) {
                Ok(Some(originals)) if !originals.is_empty() => {
                    if detect::fits(install, &originals, &mut checksums.1, stop)? {
                        return Ok(Pending::Patch(jump(from, shelf.size_of(&url)?.ok_or(Error::NotPublished)?)));
                    }
                    ruled_out.push(from);
                }
                // No such jump on the server, or one that does not say what it starts from.
                Ok(_) | Err(Error::NotPublished) => {}
                Err(err) => return Err(err),
            }
        }
    }

    // What is left are patches that replace whole files without saying what they start
    // from. The single steps tell which minor the install is at: it is at minor k when
    // everything the step to k writes is in place.
    let mut found_at = None;
    for to in (2..newest).rev() {
        let step = shelf.minor(version, to - 1, to)?;
        if step.in_place(install, stop)? {
            record_minor(site, install, version, to);
            found_at = Some(to);
            break;
        }
    }
    // With no step in place the install is at the first minor of its version, if it is at
    // that version at all. The files of a step show that it is. So does a record of this
    // app's, or a proof. The registry's word alone does not: a folder at another version
    // has none of the steps in place either.
    let proven = found_at.is_some() || version_proven(site, install, version);
    let at = found_at.unwrap_or(1);
    if at == newest - 1 {
        let offered = proven || last_step.checks_folder();
        return Ok(if offered { Pending::Patch(jump(at, last_step.size())) } else { Pending::Unproven });
    }
    // A jump that names its starting files was already compared with the install above.
    if ruled_out.contains(&at) {
        return mismatch();
    }
    // So the jump from here is one that names none.
    match shelf.size_of(&site.minor_patch_url(version, at, newest))? {
        Some(size) if proven => Ok(Pending::Patch(jump(at, size))),
        Some(_) => Ok(Pending::Unproven),
        None => mismatch(),
    }
}

/// Whether a run of `target`, the patch a check found for `install`, has anything to go by
/// that the folder is the one the patch was made for: the patch names the files it starts
/// from, or the folder is known to be at the version the patch starts from.
///
/// A minor patch was looked into before it was offered; see [`pending_minor`]. A main patch
/// is not read before it is fetched. For a folder whose version is known, it need not be.
/// For any other, the start of the file is fetched here, to see that it names what it
/// starts from. One that names nothing would be laid over whatever version the folder is
/// really at: it is no patch for such a folder, and what the check offered is withdrawn
/// with that for its reason, until the server or the folder changes.
pub(crate) fn checks_its_folder(
    site: &Site,
    install: &Path,
    target: &Target,
    shelf: &dyn Shelf,
) -> Result<bool, Error> {
    if target.minor.is_some() {
        return Ok(true);
    }
    let names_any = given_checks_its_folder(site, install, &target.url, starts_from(target), shelf)?;
    if !names_any {
        let mut found = site.found.lock().unwrap();
        let offered = found.as_mut().filter(|found| found.install == install && found.target.as_ref() == Some(target));
        if let Some(found) = offered {
            found.target = None;
            found.error = Some(describe_check_error(&Error::UnknownForm(String::new())));
        }
    }
    Ok(names_any)
}

/// The same question as [`checks_its_folder`] asks, of the patch at `url`, an address the
/// user gave. `from` is the main version the patch starts from, where its address says.
///
/// An address proves nothing of the folder it is given for, and no check has looked into
/// what is there. So whatever the patch is, it has to name the files it starts from, unless
/// the folder is known to be at the version it starts from.
pub(crate) fn given_checks_its_folder(
    site: &Site,
    install: &Path,
    url: &str,
    from: Option<u32>,
    shelf: &dyn Shelf,
) -> Result<bool, Error> {
    if from.is_some_and(|from| version_proven(site, install, from)) {
        return Ok(true);
    }
    Ok(shelf.originals_of(url)?.is_some_and(|originals| !originals.is_empty()))
}

/// The main version an install has to be at for `target` to be its next patch: the one a
/// main patch leads on from, and the one a minor patch is a patch of.
pub(crate) fn starts_from(target: &Target) -> Option<u32> {
    match target.minor {
        Some(_) => Some(target.version),
        None => versions_in_name(Path::new(&target.file_name)).map(|(from, _)| from),
    }
}

/// The main versions a patch file leads from and to, read from a name such as
/// `00421to00422.patch`.
fn versions_in_name(file: &Path) -> Option<(u32, u32)> {
    let name = file.file_stem()?.to_str()?;
    let (from, to) = name.split_once("to")?;
    let number = |text: &str| if text.len() == 5 { text.parse::<u32>().ok() } else { None };
    Some((number(from)?, number(to)?))
}
