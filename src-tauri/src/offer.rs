//! What Nexon's server has for an install: which patches are published, and which of them
//! this install can take.
//!
//! The server is asked through a [`Shelf`], so that the choice of a patch can be tried on a
//! made-up shelf of patches, without a network.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mfp_engine::container::PatchFile;
use mfp_engine::detect::{self, Checksums};
use mfp_engine::preflight::{self, Problem};
use mfp_engine::{Error, download, plan, stage};
use mfp_kms::Client;
use tauri::AppHandle;

use crate::ready::unreadable_journal;
use crate::records::{
    at_final, install_dir, install_mark, installed_version, known_minor, latest_seen, patch_cache, record_final,
    record_minor, record_proven, run_target, see_latest, settle_version, version_proven,
};
use crate::state::{Availability, Found, Patcher, Site, Target, Unproven};
use crate::window::notify;
use crate::wording::{PUT_BACK_FIRST, describe_check_error, describe_problem, drive_letter};

/// The most [`detect::remote_originals`] writes to the cache while it looks at the start of
/// a patch file.
const PEEK_MOST: u64 = 16 << 20;

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

    /// The start of the file is fetched into the cache for it.
    fn originals_of(&self, url: &str) -> Result<Option<BTreeMap<String, u32>>, Error> {
        let cache = patch_cache(self.site);
        cache_room(&cache, PEEK_MOST)?;
        detect::remote_originals(url, &cache)
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

/// A version of `client` with its minor version, which is left out while it is still the
/// first.
pub(crate) fn full_label(client: &Client, version: u32, minor: u32) -> String {
    let label = client.version_label(version);
    if minor > 1 { format!("{label} ({minor})") } else { label }
}

/// Minor versions are two digits in the patch addresses.
const LAST_MINOR: u32 = 99;
/// How many main versions above the one it starts from the newest is looked for.
const MAINS_AHEAD: u32 = 20;

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
        label: site.client.version_label(to),
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
/// which kind of patch it is, as in `1.2.422 클라이언트 패치가 등록되었습니다.`
fn registered(target: &Target) -> String {
    format!("{} {}가 등록되었습니다.", target.label, target.kind())
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
    let (main, newest_minor, mark, latest, target, error) = loop {
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
            Some(seen) => site.client.version_label(seen),
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
        break (main, newest_minor, mark, latest, target, error);
    };

    // The user may have picked another folder while this check was at work. Its answer is
    // then about the wrong folder and is dropped.
    if install_dir(site).as_deref() != Some(install.as_path()) {
        return Checked::stopped(nothing(Some(describe_check_error(&Error::Stopped))));
    }
    let found =
        Found { install: install.clone(), mark, version, main: main.is_some(), newest_minor, target, latest, error };
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

/// How many main versions a patch that skips versions leads over at most: for each new
/// version Nexon publishes one from each of the five before it. A folder's version is
/// looked for that far back from the newest, and a folder that is further behind is taken
/// that far on at a time.
const REACH: u32 = 5;

/// What looking for a proof of a folder's version came to.
enum Proof {
    /// The folder is at this main version. What proves it is on record, with the minor it
    /// is at where the proof says that too.
    Found(u32),
    /// Nothing proves which version the folder is at. `latest` is the newest version the
    /// server has, as far as that could be made out.
    None { latest: Option<String> },
}

/// Proves the main version of an install folder from the folder's own files, and writes
/// down what was proven: for a folder that nothing on this PC speaks for, and for one that
/// only the registry speaks for (`hint` is what it says) when that is not enough to go by.
///
/// The newest version on the server is looked for upwards from the highest version there
/// is word of: the newest the site has seen the server have, the one that is known of the
/// client from the start, or the registry's. Against that newest version N the folder's
/// version counts as proven by one of these, and by nothing else:
///
/// - The folder holds every file a main patch to N starts from, for one of the versions
///   just below N. It is at the version that patch starts from, and at its last minor.
/// - The folder holds every file a minor patch of N starts from, for one that says what it
///   starts from. It is at N, and at the minor that patch starts from.
/// - Everything the newest minor patch of N writes is in the folder. It is at N, and at
///   the newest minor.
///
/// A patch file that does not say what it starts from proves nothing: it fits any folder,
/// the other client's included. Without a proof nothing is offered for the folder, least
/// of all such a patch.
fn prove(site: &Site, install: &Path, shelf: &dyn Shelf, hint: Option<u32>) -> Result<Proof, Error> {
    let Some(from) = latest_seen(site).max(site.client.known_version()).max(hint) else {
        return Ok(Proof::None { latest: None });
    };
    let newest = newest_main(site, from, from + MAINS_AHEAD, shelf)?;
    let minor = newest_minor(site, newest, shelf)?;
    see_latest(site, newest);
    let latest = Some(full_label(site.client, newest, minor));
    // Looking again at the same folder and the same shelf would find no proof again, and
    // reads the folder's files and fetches the patch files for nothing.
    let went_by =
        Unproven { install: install.to_path_buf(), mark: install_mark(site, install), newest, newest_minor: minor };
    {
        let mut unproven = site.unproven.lock().unwrap();
        if unproven.as_ref() == Some(&went_by) {
            return Ok(Proof::None { latest });
        }
        // What was found of the folder or the shelf as they were holds for neither as
        // they are now.
        *unproven = None;
    }

    let stop: &AtomicBool = &site.check_stop;
    let fits = |url: &str| -> Result<bool, Error> {
        match shelf.originals_of(url) {
            Ok(Some(originals)) if !originals.is_empty() => {
                detect::fits(install, &originals, &mut site.checksums.lock().unwrap().1, stop)
            }
            // No such patch on the server, or one that does not say what it starts from.
            Ok(_) | Err(Error::NotPublished) => Ok(false),
            Err(err) => Err(err),
        }
    };
    for version in (newest.saturating_sub(REACH)..newest).rev() {
        if fits(&site.patch_url(version, newest))? {
            site.log(&format!("the folder is at {version}: it holds what the main patch to {newest} starts from"));
            record_proven(site, install, version);
            // The main patch lists every file of the version it starts from. A folder that
            // holds them all is at that version's last minor.
            record_final(site, install, version);
            return Ok(Proof::Found(version));
        }
    }
    for to in (2..=minor).rev() {
        for at in 1..to {
            if fits(&site.minor_patch_url(newest, at, to))? {
                site.log(&format!("the folder is at {newest}, minor {at}: it holds what a minor patch starts from"));
                record_proven(site, install, newest);
                record_minor(site, install, newest, at);
                return Ok(Proof::Found(newest));
            }
        }
    }
    if minor > 1 && shelf.minor(newest, minor - 1, minor)?.in_place(install, stop)? {
        site.log(&format!("the folder is at {newest}, minor {minor}: it holds what the newest minor patch writes"));
        record_proven(site, install, newest);
        record_minor(site, install, newest, minor);
        return Ok(Proof::Found(newest));
    }
    site.log(&format!("nothing proves the version of the folder; the server has {newest}, minor {minor}"));
    *site.unproven.lock().unwrap() = Some(went_by);
    Ok(Proof::None { latest })
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
pub(crate) fn versions_in_name(file: &Path) -> Option<(u32, u32)> {
    let name = file.file_stem()?.to_str()?;
    let (from, to) = name.split_once("to")?;
    let number = |text: &str| if text.len() == 5 { text.parse::<u32>().ok() } else { None };
    Some((number(from)?, number(to)?))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::harness::StandIns;
    use crate::records::{choose_dir, patch_at, patched_before, record_run_target, record_version};
    use crate::scratch::Scratch;

    #[test]
    fn a_version_is_written_as_nexon_writes_it_for_the_client() {
        let (live, test) = (&mfp_kms::LIVE, &mfp_kms::TEST);
        assert_eq!(full_label(live, 421, 1), "1.2.421");
        assert_eq!(full_label(live, 421, 4), "1.2.421 (4)");
        // The test world's patch files count a thousand ahead of its versions.
        assert_eq!(full_label(test, 1208, 2), "1.2.208 (2)");
    }

    #[test]
    fn the_versions_of_a_patch_file_are_read_from_a_name_nexon_gave_it() {
        assert_eq!(versions_in_name(Path::new("00420to00421.patch")), Some((420, 421)));
        assert_eq!(versions_in_name(Path::new("00417to00421.patch")), Some((417, 421)));
        for other in ["03to04.patch", "00421-minor-03to04.patch", "patch.patch", "420to421.patch"] {
            assert_eq!(versions_in_name(Path::new(other)), None, "{other}");
        }
    }

    // From here on the check itself, on a made-up shelf of patches and a made-up install
    // folder: nothing is asked of a network.

    /// One patch file on a made-up shelf.
    #[derive(Clone, Default)]
    struct OnShelf {
        size: u64,
        /// The checksums of the files it starts from. An empty table is that of a patch file
        /// that only holds whole files, and says nothing of the folder it is for.
        originals: BTreeMap<String, u32>,
        /// For a minor patch: the files it writes, with their checksums once it is applied.
        writes: BTreeMap<String, u32>,
    }

    /// A made-up shelf of the patch files of one client, by their addresses. It notes what
    /// it is asked.
    struct Shelved {
        client: &'static Client,
        files: BTreeMap<String, OnShelf>,
        asked: RefCell<Vec<String>>,
    }

    impl Shelf for Shelved {
        fn size_of(&self, url: &str) -> Result<Option<u64>, Error> {
            self.asked.borrow_mut().push(format!("size {url}"));
            Ok(self.files.get(url).map(|file| file.size))
        }

        fn originals_of(&self, url: &str) -> Result<Option<BTreeMap<String, u32>>, Error> {
            self.asked.borrow_mut().push(format!("originals {url}"));
            self.files.get(url).map(|file| Some(file.originals.clone())).ok_or(Error::NotPublished)
        }

        fn minor(&self, version: u32, from: u32, to: u32) -> Result<Box<dyn Minor>, Error> {
            let url = self.client.minor_patch_url(version, from, to);
            self.asked.borrow_mut().push(format!("read {url}"));
            let file = self.files.get(&url).ok_or(Error::NotPublished)?;
            let checks = !file.originals.is_empty();
            Ok(Box::new(Read { size: file.size, writes: file.writes.clone(), checks }))
        }
    }

    /// A minor patch of the made-up shelf, read.
    struct Read {
        size: u64,
        writes: BTreeMap<String, u32>,
        /// It names files it starts from.
        checks: bool,
    }

    impl Minor for Read {
        fn size(&self) -> u64 {
            self.size
        }

        fn in_place(&self, install: &Path, _: &AtomicBool) -> Result<bool, Error> {
            let holds = |(name, sum): (&String, &u32)| {
                std::fs::read(install.join(name)).is_ok_and(|bytes| mfp_engine::crc::update(0, &bytes) == *sum)
            };
            Ok(self.writes.iter().all(holds))
        }

        fn checks_folder(&self) -> bool {
            self.checks
        }
    }

    /// A made-up install of one client that the registry knows nothing of, the site that
    /// looks after it, and the shelf its patches are on.
    struct Made {
        patcher: Patcher,
        install: PathBuf,
        shelf: Shelved,
        _scratch: Scratch,
    }

    impl Made {
        /// An install of the test world, of which nothing on a PC says which version it is.
        fn test_world(name: &str) -> Made {
            Made::of(&mfp_kms::TEST, name, StandIns::default())
        }

        fn of(client: &'static Client, name: &str, stand_ins: StandIns) -> Made {
            let made = Made::with(client, name, |_| stand_ins);
            choose_dir(made.site(), Some(&made.install.to_string_lossy())).unwrap();
            made
        }

        /// An install of the live game that is the one the Nexon installer registered, with
        /// `version` as what the registry gives for it: the folder nobody had to pick.
        fn registered(name: &str, version: u32) -> Made {
            Made::with(&mfp_kms::LIVE, name, |install| StandIns::registered_at(install, version))
        }

        /// A made-up install of `client` and its site, with what `stand_ins` makes of the
        /// install folder standing in for Nexon's side.
        fn with(client: &'static Client, name: &str, stand_ins: impl FnOnce(&Path) -> StandIns) -> Made {
            let scratch = Scratch::new(name);
            let install = scratch.0.join("game");
            std::fs::create_dir_all(install.join("Data")).unwrap();
            std::fs::write(install.join(client.game_program), b"the game").unwrap();
            let data = scratch.0.join("data");
            let site = Site::new(client, data.join(client.server.key()), stand_ins(&install));
            let shelf = Shelved { client, files: BTreeMap::new(), asked: RefCell::default() };
            Made { patcher: Patcher::of(data, vec![site]), install, shelf, _scratch: scratch }
        }

        fn site(&self) -> &Site {
            &self.patcher.sites[0]
        }

        /// Puts a file of the game into the install folder, and says what a patch file
        /// would list it as: its name there, and its checksum.
        fn holds(&self, name: &str, bytes: &[u8]) -> (String, u32) {
            std::fs::write(self.install.join("Data").join(name), bytes).unwrap();
            (format!(r"Data\{name}"), mfp_engine::crc::update(0, bytes))
        }

        /// Puts the main patch from `from` to `to` on the shelf, made from `originals`.
        fn main_patch(&mut self, from: u32, to: u32, originals: &[(String, u32)]) {
            let file = OnShelf { size: 1000, originals: originals.iter().cloned().collect(), ..OnShelf::default() };
            self.shelf.files.insert(self.shelf.client.patch_url(from, to), file);
        }

        /// Puts a minor patch of `version` on the shelf, made from `originals` and writing
        /// `writes`.
        fn minor_patch(
            &mut self,
            version: u32,
            minors: (u32, u32),
            originals: &[(String, u32)],
            writes: &[(String, u32)],
        ) {
            let file = OnShelf {
                size: 200,
                originals: originals.iter().cloned().collect(),
                writes: writes.iter().cloned().collect(),
            };
            self.shelf.files.insert(self.shelf.client.minor_patch_url(version, minors.0, minors.1), file);
        }

        fn check(&self) -> Availability {
            let checked = check(&self.patcher, self.site(), &self.shelf, &mut None);
            assert_eq!(checked.end, CheckEnd::Answered);
            checked.answer
        }

        /// What the shelf was asked since this was last asked, by the first word of each
        /// question.
        fn asked(&self, what: &str) -> usize {
            self.shelf.asked.borrow().iter().filter(|asked| asked.starts_with(what)).count()
        }
    }

    /// What a check says of a folder it has no proof for: nothing is offered, and the
    /// footer says that the version is not known.
    fn unproven(latest: &str) -> Availability {
        Availability {
            target: None, latest: Some(latest.to_owned()), error: Some("현재 버전 확인 불가".to_owned())
        }
    }

    #[test]
    fn nothing_is_offered_for_a_folder_whose_version_nothing_proves() {
        let mut made = Made::test_world("no-proof");
        let other = made.holds("Base.wz", b"what the folder holds");
        // Nothing is on the shelf. The newest version is the one the test world is known
        // to have had.
        assert_eq!(made.check(), unproven("1.2.208"));
        assert_eq!(installed_version(made.site(), &made.install), None);

        // A main patch that starts from other files than the folder's proves nothing, and
        // neither does one that starts from a file the folder does not have.
        made.site().forget_found();
        made.main_patch(1207, 1208, &[(other.0.clone(), other.1 ^ 1)]);
        made.main_patch(1206, 1208, &[(r"Data\Gone.wz".to_owned(), 7)]);
        assert_eq!(made.check(), unproven("1.2.208"));
        assert_eq!(installed_version(made.site(), &made.install), None);
    }

    /// The first proof: the folder holds every file the main patch to the newest version
    /// starts from.
    #[test]
    fn a_folder_that_holds_what_the_main_patch_starts_from_is_at_the_version_before() {
        let mut made = Made::test_world("proof-main");
        let base = made.holds("Base.wz", b"base of 1.2.207");
        let map = made.holds("Map.wz", b"map of 1.2.207");
        made.main_patch(1207, 1208, &[base, map]);

        let answer = made.check();
        let target = answer.target.expect("the main patch");
        assert_eq!((target.label.as_str(), target.version, target.minor), ("1.2.208", 1208, None));
        assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch");
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.208"), None));
        // What was proven is on record: the version, and that the folder is at its last
        // minor. It is no record of a patch by this app.
        assert_eq!(installed_version(made.site(), &made.install), Some(1207));
        assert!(at_final(made.site(), &made.install, 1207));
        assert!(!patched_before(made.site(), &made.install));
    }

    /// Nexon also publishes patches that skip versions. One proves the version it starts
    /// from like any other, and is the way on from there: no single step has to lead
    /// anywhere for it.
    #[test]
    fn a_patch_that_skips_versions_proves_the_one_it_starts_from_and_is_the_way_on_from_there() {
        let mut made = Made::test_world("proof-skip");
        let base = made.holds("Base.wz", b"base of 1.2.204");
        made.main_patch(1204, 1208, std::slice::from_ref(&base));
        // No step leads on from 1.2.204. The server is known to be at 1.2.208 all the same,
        // and has the patch from here to there.
        let answer = made.check();
        assert_eq!(installed_version(made.site(), &made.install), Some(1204));
        let target = answer.target.expect("the patch straight to the newest version");
        assert_eq!((target.label.as_str(), target.version, target.minor), ("1.2.208", 1208, None));
        assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01204to01208.patch");
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.208"), None));

        // The step to the next version on the shelf as well changes nothing of that.
        made.main_patch(1204, 1205, &[base]);
        let answer = made.check();
        assert_eq!((answer.target, answer.latest.as_deref(), answer.error), (Some(target), Some("1.2.208"), None));
    }

    /// The server is further on than anything it has for the folder's version leads to.
    #[test]
    fn a_folder_that_no_patch_on_the_server_leads_on_from_is_not_called_up_to_date() {
        let mut made = Made::test_world("no-way-on");
        record_version(made.site(), &made.install, 1204);
        made.main_patch(1207, 1208, &[]);
        see_latest(made.site(), 1208);
        let answer = made.check();
        assert_eq!((answer.target, answer.latest.as_deref()), (None, Some("1.2.208")));
        assert_eq!(answer.error.as_deref(), Some("패치 확인 불가"));
    }

    /// A folder that is more than one main version behind: a seat in a PC room, or a
    /// player who is back after months.
    #[test]
    fn a_folder_two_versions_behind_takes_the_patch_straight_to_the_newest_where_the_server_has_it() {
        let mut made = Made::registered("two-behind", 900);
        let base = made.holds("Base.wz", b"base of 1.2.900");
        made.main_patch(900, 901, std::slice::from_ref(&base));
        made.main_patch(901, 902, &[]);
        // The server has none that skips a version: the single step, as ever.
        let answer = made.check();
        let step = answer.target.expect("the step to the next version");
        assert_eq!(
            (step.label.as_str(), step.version, step.file_name.as_str()),
            ("1.2.901", 901, "00900to00901.patch")
        );
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.902"), None));

        // It has one: straight to the newest. Nothing of the folder is read to choose.
        made.main_patch(900, 902, std::slice::from_ref(&base));
        made.site().forget_found();
        made.shelf.asked.borrow_mut().clear();
        let answer = made.check();
        let direct = answer.target.expect("the patch straight to the newest version");
        assert_eq!((direct.label.as_str(), direct.version, direct.minor), ("1.2.902", 902, None));
        assert_eq!(direct.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00902/00900to00902.patch");
        assert_eq!((direct.file_name.as_str(), direct.size), ("00900to00902.patch", 1000));
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.902"), None));
        assert_eq!((made.asked("read"), made.asked("originals")), (0, 0));
        // It is a client patch like the step: on the screen, and in what is said of one
        // that appears.
        assert_eq!(direct.kind(), "클라이언트 패치");
        assert_eq!(registered(&direct), "1.2.902 클라이언트 패치가 등록되었습니다.");
        // Its run holds the folder against the version it starts from, as the step's does.
        assert_eq!(starts_from(&direct), Some(900));
        assert!(checks_its_folder(made.site(), &made.install, &direct, &made.shelf).unwrap());
    }

    /// A folder that is further behind than any patch to the newest version starts from:
    /// it is taken as far on as the server has a patch for, and from there again.
    #[test]
    fn a_folder_eight_versions_behind_takes_the_furthest_patch_the_server_has_each_time() {
        let mut made = Made::registered("eight-behind", 900);
        let base = made.holds("Base.wz", b"base of 1.2.900");
        for from in 900..908 {
            made.main_patch(from, from + 1, std::slice::from_ref(&base));
        }
        // As Nexon publishes: to each version from each of the five before it.
        for to in 902..=908u32 {
            for from in to.saturating_sub(5).max(900)..to - 1 {
                made.main_patch(from, to, std::slice::from_ref(&base));
            }
        }
        made.shelf.asked.borrow_mut().clear();
        let answer = made.check();
        let first = answer.target.expect("the patch five versions on");
        assert_eq!(
            (first.label.as_str(), first.version, first.file_name.as_str()),
            ("1.2.905", 905, "00900to00905.patch")
        );
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.908"), None));
        // One question finds it, and nothing of the folder is read.
        let jumps =
            |made: &Made| made.shelf.asked.borrow().iter().filter(|asked| asked.contains("/00900to0090")).count();
        assert_eq!((jumps(&made), made.asked("read"), made.asked("originals")), (2, 0, 0));

        // Applied: from 1.2.905 the newest is within reach, and the patch leads straight there.
        record_version(made.site(), &made.install, 905);
        made.site().forget_found();
        let answer = made.check();
        let second = answer.target.expect("the patch straight to the newest version");
        assert_eq!(
            (second.label.as_str(), second.version, second.file_name.as_str()),
            ("1.2.908", 908, "00905to00908.patch")
        );
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.908"), None));
    }

    /// The server has only some of the patches that skip versions: the furthest of those it
    /// has is the one, and with none of them the single step.
    #[test]
    fn of_the_patches_that_skip_versions_the_furthest_the_server_has_is_taken() {
        let mut made = Made::registered("some-jumps", 900);
        let base = made.holds("Base.wz", b"base of 1.2.900");
        for from in 900..908 {
            made.main_patch(from, from + 1, std::slice::from_ref(&base));
        }
        let offered = |made: &Made| {
            made.site().forget_found();
            made.shelf.asked.borrow_mut().clear();
            let answer = made.check();
            assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.908"), None));
            let jumps: Vec<String> = (902..=908u32)
                .filter(|to| {
                    made.shelf.asked.borrow().contains(&format!("size {}", made.shelf.client.patch_url(900, *to)))
                })
                .map(|to| to.to_string())
                .collect();
            (answer.target.expect("a patch").file_name, jumps.join(" "))
        };
        // None of them: the single step, after the four that were asked for.
        assert_eq!(offered(&made), ("00900to00901.patch".to_owned(), "902 903 904 905".to_owned()));
        // The one over two versions only.
        made.main_patch(900, 902, std::slice::from_ref(&base));
        assert_eq!(offered(&made), ("00900to00902.patch".to_owned(), "902 903 904 905".to_owned()));
        // One over four as well: that one, and nothing nearer is asked for.
        made.main_patch(900, 904, std::slice::from_ref(&base));
        assert_eq!(offered(&made), ("00900to00904.patch".to_owned(), "904 905".to_owned()));
        // A patch that leads further than any is published for is never asked for.
        made.main_patch(900, 906, std::slice::from_ref(&base));
        assert_eq!(offered(&made), ("00900to00904.patch".to_owned(), "904 905".to_owned()));
    }

    #[test]
    fn the_minor_patches_of_the_folders_own_version_come_before_the_patch_that_skips_versions() {
        let mut made = Made::registered("minor-then-direct", 900);
        let base = made.holds("Base.wz", b"base of 1.2.900");
        let string = made.holds("String.wz", b"string of 1.2.900");
        made.main_patch(900, 901, std::slice::from_ref(&base));
        made.main_patch(901, 902, &[]);
        made.main_patch(900, 902, std::slice::from_ref(&base));
        made.minor_patch(900, (1, 2), &[string], &[("Data/String.wz".to_owned(), 12345)]);
        let answer = made.check();
        let minor = answer.target.expect("the minor patch of the folder's version");
        assert_eq!((minor.label.as_str(), minor.version, minor.minor), ("1.2.900 (2)", 900, Some(2)));
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.902"), None));

        // Applied, which its run puts on record: the patch straight to the newest is next.
        record_version(made.site(), &made.install, 900);
        record_minor(made.site(), &made.install, 900, 2);
        made.site().forget_found();
        let direct = made.check().target.expect("the patch straight to the newest version");
        assert_eq!((direct.label.as_str(), direct.version, direct.minor), ("1.2.902", 902, None));

        // That one applied: the folder is at 1.2.902, at its first minor, and the minor
        // patches of 1.2.902 are what it is offered next.
        record_version(made.site(), &made.install, direct.version);
        made.site().forget_found();
        made.minor_patch(902, (1, 2), &[base], &[("Data/Base.wz".to_owned(), 54321)]);
        let answer = made.check();
        assert_eq!(installed_version(made.site(), &made.install), Some(902));
        let minor = answer.target.expect("the minor patch of the newest version");
        assert_eq!((minor.label.as_str(), minor.version, minor.minor), ("1.2.902 (2)", 902, Some(2)));
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.902 (2)"), None));
    }

    /// The test world's patch files count a thousand ahead of its versions. Its patch that
    /// skips versions is its own in every form the app keeps it in.
    #[test]
    fn the_test_worlds_patch_that_skips_versions_is_its_own_by_name_address_and_record() {
        let mut made = Made::test_world("direct-test-world");
        let base = made.holds("Base.wz", b"base of 1.2.206");
        made.main_patch(1206, 1207, std::slice::from_ref(&base));
        made.main_patch(1207, 1208, &[]);
        made.main_patch(1206, 1208, std::slice::from_ref(&base));
        let answer = made.check();
        let direct = answer.target.expect("the patch straight to the newest version");
        assert_eq!((direct.label.as_str(), direct.version, direct.minor), ("1.2.208", 1208, None));
        assert_eq!(direct.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01206to01208.patch");
        assert_eq!(direct.file_name, "01206to01208.patch");
        assert_eq!(starts_from(&direct), Some(1206));
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.208"), None));

        // What a run of it writes down of itself leads back to the same patch: a folder
        // that an immediate run of it left partly new is offered the rest of it.
        record_run_target(made.site(), &made.install, Some(&direct), &direct.url);
        assert_eq!(run_target(made.site(), &made.install), Some(direct.clone()));
        // Given by its address, it is the same patch for the test world's site, and none
        // of the live game's.
        let live = Site::new(&mfp_kms::LIVE, made.patcher.data.join("live"), StandIns::default());
        assert_eq!(patch_at(&live, &direct.url, direct.size), None);
        assert_eq!(patch_at(made.site(), &direct.url, direct.size), Some(direct));
    }

    /// The second proof: a minor patch that says what it starts from, and the folder holds it.
    #[test]
    fn a_folder_that_holds_what_a_minor_patch_starts_from_is_at_the_minor_it_starts_from() {
        let mut made = Made::test_world("proof-minor");
        let skill = made.holds("Skill.wz", b"skill of minor 2");
        made.minor_patch(1208, (1, 2), &[], &[("Data/String.wz".to_owned(), 1)]);
        made.minor_patch(1208, (2, 3), &[skill], &[("Data/Skill.wz".to_owned(), 2)]);
        made.minor_patch(1208, (1, 3), &[], &[("Data/Skill.wz".to_owned(), 2)]);

        let answer = made.check();
        let target = answer.target.expect("the minor patch");
        assert_eq!((target.label.as_str(), target.version, target.minor), ("1.2.208 (3)", 1208, Some(3)));
        assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/Minor/02to03.patch");
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.208 (3)"), None));
        assert_eq!(installed_version(made.site(), &made.install), Some(1208));
        assert_eq!(known_minor(made.site(), &made.install, 1208), Some(2));
    }

    /// The third proof: everything the newest minor patch writes is in the folder.
    #[test]
    fn a_folder_that_holds_what_the_newest_minor_patch_writes_is_at_the_newest_version() {
        let mut made = Made::test_world("proof-newest");
        let (_, string) = made.holds("String.wz", b"string of minor 2");
        made.minor_patch(1208, (1, 2), &[], &[("Data/String.wz".to_owned(), string)]);

        let answer = made.check();
        assert_eq!(answer, Availability { target: None, latest: Some("1.2.208 (2)".to_owned()), error: None });
        assert_eq!(installed_version(made.site(), &made.install), Some(1208));
        assert_eq!(known_minor(made.site(), &made.install, 1208), Some(2));
    }

    /// A minor patch that only holds whole files fits any folder, the other client's
    /// included. It is offered to a folder whose version is proven, and to no other.
    #[test]
    fn a_minor_patch_that_says_nothing_of_its_folder_is_not_offered_without_a_proof() {
        let mut made = Made::test_world("empty-table");
        made.holds("String.wz", b"string of who knows which version");
        made.minor_patch(1208, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);
        assert_eq!(made.check(), unproven("1.2.208 (2)"));
        // It was read, to see whether it is in place. Reading it is not offering it.
        assert_eq!(made.asked("read"), 1);

        // The same folder and the same shelf, once this app has patched the folder to
        // 1.2.208 itself: the folder is then at the first minor, and the patch is its next.
        record_version(made.site(), &made.install, 1208);
        made.site().forget_found();
        let target = made.check().target.expect("the minor patch");
        assert_eq!((target.label.as_str(), target.minor), ("1.2.208 (2)", Some(2)));
        assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/Minor/01to02.patch");
    }

    /// Looking for a proof reads the folder's files and fetches the starts of patch files.
    /// It is done again when the shelf or the folder has changed, and not before.
    #[test]
    fn a_proof_is_looked_for_again_only_when_the_shelf_or_the_folder_has_changed() {
        let mut made = Made::test_world("proof-again");
        let base = made.holds("Base.wz", b"base of 1.2.208");
        assert_eq!(made.check(), unproven("1.2.208"));
        assert_eq!(made.asked("originals"), 5);

        made.shelf.asked.borrow_mut().clear();
        assert_eq!(made.check(), unproven("1.2.208"));
        assert_eq!((made.asked("originals"), made.asked("read")), (0, 0));
        // What is asked every time is what a new patch would show by: the step from the
        // newest version, and its first minor.
        assert_eq!(made.asked("size"), 2);

        // The next version comes out. Its patch starts from what the folder holds.
        made.main_patch(1208, 1209, &[base]);
        let answer = made.check();
        assert_eq!(answer.target.map(|target| target.label), Some("1.2.209".to_owned()));
        assert_eq!(installed_version(made.site(), &made.install), Some(1208));
        // The newest version that was seen is where the next look for one begins.
        assert_eq!(latest_seen(made.site()), Some(1209));
    }

    /// The live game's check goes the way it always went, now through the shelf: the
    /// registry's guess is where the look for the folder's patches starts.
    #[test]
    fn a_folder_the_registry_speaks_for_is_checked_from_what_the_registry_says() {
        let mut made = Made::of(&mfp_kms::LIVE, "hinted", StandIns::at_version(900));
        made.holds("String.wz", b"string of 1.2.900");
        made.main_patch(900, 901, &[]);
        let answer = made.check();
        let target = answer.target.expect("the main patch");
        assert_eq!((target.label.as_str(), target.version), ("1.2.901", 901));
        assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00901/00900to00901.patch");
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.901"), None));
        // The one thing looked into is whether the folder is a version behind the guess.
        assert_eq!((made.asked("originals"), made.asked("read")), (1, 0));

        // A minor patch that says nothing of its folder is not offered on the strength of
        // the guess: nothing shows that the folder is at 1.2.900 and not at another version.
        made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);
        assert_eq!(made.check(), unproven("1.2.901"));
    }

    /// A patch that comes out while the app is open is announced with the version it leads
    /// to and its kind. One that is there at the first check is not: the screen shows it.
    #[test]
    fn a_patch_that_appears_is_announced_by_its_version_and_its_kind() {
        let checked = |made: &Made| {
            let mut appeared = None;
            check(&made.patcher, made.site(), &made.shelf, &mut appeared);
            appeared.as_ref().map(registered)
        };
        let mut made = Made::of(&mfp_kms::LIVE, "appeared-main", StandIns::at_version(900));
        assert_eq!(checked(&made), None);
        made.main_patch(900, 901, &[]);
        assert_eq!(checked(&made).as_deref(), Some("1.2.901 클라이언트 패치가 등록되었습니다."));
        // Said once: at the next check the patch is no news.
        assert_eq!(checked(&made), None);

        let mut made = Made::of(&mfp_kms::LIVE, "appeared-minor", StandIns::at_version(900));
        let string = made.holds("String.wz", b"string of 1.2.900");
        assert_eq!(checked(&made), None);
        made.minor_patch(900, (1, 2), &[string], &[("Data/String.wz".to_owned(), 12345)]);
        assert_eq!(checked(&made).as_deref(), Some("1.2.900 (2) 마이너 패치가 등록되었습니다."));

        // A patch that is there when the app first looks is on the screen, and not announced.
        let mut made = Made::of(&mfp_kms::LIVE, "appeared-first", StandIns::at_version(900));
        made.main_patch(900, 901, &[]);
        assert_eq!(checked(&made), None);
    }

    /// Most PCs have no test client. For a client that is not installed the check has
    /// its answer without asking anything, and leaves nothing behind.
    #[test]
    fn nothing_is_asked_or_written_for_a_client_that_is_not_installed() {
        let scratch = Scratch::new("not-installed-check");
        let data = scratch.0.join("data");
        // A build for trying the app out never takes a folder where the real games live,
        // so whatever is installed on this PC, nothing is found.
        let site = Site::new(&mfp_kms::TEST, data.join("test"), StandIns::of_a_session());
        let patcher = Patcher::of(data.clone(), vec![site]);
        let shelf = Shelved { client: &mfp_kms::TEST, files: BTreeMap::new(), asked: RefCell::default() };
        let checked = check(&patcher, &patcher.sites[0], &shelf, &mut None);
        assert_eq!((checked.answer, checked.end), (Availability::nothing(), CheckEnd::Answered));
        assert!(shelf.asked.borrow().is_empty());
        assert!(!data.exists());
    }

    /// A folder with the other client's program in it is none of this client's: nothing
    /// is asked about it.
    #[test]
    fn nothing_is_asked_about_a_folder_of_the_other_client() {
        let made = Made::test_world("other-client");
        std::fs::rename(made.install.join("MapleStoryT.exe"), made.install.join("MapleStory.exe")).unwrap();
        assert_eq!(made.check(), Availability::nothing());
        assert!(made.shelf.asked.borrow().is_empty());
    }

    // From here on: what is offered on the registry's word alone. That word is a guess at
    // the folder's version. A patch that names the files it starts from can be offered on
    // a guess, since a run of it holds the folder against them first. A patch that names
    // none is for a folder whose version is proven, and for no other.

    /// A folder the user picked, of which the registry says nothing. The version the
    /// registry gives for the registered install is all there is to go by, and the folder
    /// may as well be at another.
    #[test]
    fn a_picked_folder_at_another_version_is_not_offered_a_patch_that_fits_any_folder() {
        let mut made = Made::of(&mfp_kms::LIVE, "picked-other-version", StandIns::at_version(900));
        made.holds("String.wz", b"string of some other version");
        made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);
        assert_eq!(made.check(), unproven("1.2.900 (2)"));
        // Nothing is on record of the folder being at 1.2.900: it was looked into and not found.
        assert!(!version_proven(made.site(), &made.install, 900));
        assert!(crate::records::found_unproven(made.site(), &made.install));

        // That stands until the shelf or the folder changes, and is not worked out again:
        // only what a new patch would show by is asked.
        made.shelf.asked.borrow_mut().clear();
        assert_eq!(made.check(), unproven("1.2.900 (2)"));
        assert_eq!((made.asked("originals"), made.asked("read")), (0, 0));

        // The same folder and shelf once this app has patched the folder to 1.2.900 itself.
        record_version(made.site(), &made.install, 900);
        made.site().forget_found();
        let target = made.check().target.expect("the minor patch");
        assert_eq!((target.label.as_str(), target.minor), ("1.2.900 (2)", Some(2)));
    }

    /// The registry's value only catches up once the game has been started. An install
    /// that was patched to the next version by something else is behind it until then, and
    /// a patch of the version before, laid over it, would put old files among the new.
    #[test]
    fn a_registered_install_that_is_ahead_of_the_registry_is_not_offered_a_patch_of_the_version_before() {
        let mut made = Made::registered("registry-behind", 900);
        let (base, of_901) = made.holds("Base.wz", b"base of 1.2.901");
        made.holds("String.wz", b"string of 1.2.901");
        // The main patch starts from the base of 1.2.900, which the folder no longer has.
        made.main_patch(900, 901, &[(base, of_901 ^ 1)]);
        made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);

        let answer = made.check();
        assert_eq!(answer, unproven("1.2.901"));
        // The look for a proof went up from what the registry says, to the patches of the
        // version the folder is really at. None of them shows it.
        assert_eq!(latest_seen(made.site()), Some(901));
        assert_eq!(installed_version(made.site(), &made.install), Some(900));
    }

    /// The same install with the files the registry's version has: the proof that was
    /// looked for is found, and what the folder needs is worked out from it.
    #[test]
    fn a_proof_that_is_looked_for_and_found_settles_what_the_folder_is_offered() {
        // The folder holds what the main patch to the next version starts from. So it is
        // at the last minor of its version, and the minor patch is not its next.
        let mut made = Made::registered("proof-found-final", 900);
        let base = made.holds("Base.wz", b"base of 1.2.900");
        made.holds("String.wz", b"string of 1.2.900, minor 2");
        made.main_patch(900, 901, &[base]);
        made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);
        let answer = made.check();
        let target = answer.target.expect("the main patch");
        assert_eq!((target.label.as_str(), target.minor, answer.error), ("1.2.901", None, None));
        assert!(version_proven(made.site(), &made.install, 900) && at_final(made.site(), &made.install, 900));

        // The folder is a version behind what the registry says: it holds what the main
        // patch to that version starts from. That patch is its next.
        let mut made = Made::registered("proof-found-behind", 900);
        let base = made.holds("Base.wz", b"base of 1.2.899");
        made.main_patch(899, 900, &[base]);
        made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);
        let answer = made.check();
        let target = answer.target.expect("the main patch from the version before");
        assert_eq!((target.label.as_str(), target.version, answer.error), ("1.2.900", 900, None));
        assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00900/00899to00900.patch");
        assert_eq!(installed_version(made.site(), &made.install), Some(899));
    }

    /// The ordinary case: the install the Nexon installer registered, at the version the
    /// registry says, and patches as Nexon makes them, which name the files they start
    /// from. Nothing about it has changed: the same questions to the server in the same
    /// order, no proof looked for, and the patch offered on the registry's word.
    #[test]
    fn a_registered_install_with_patches_that_name_their_originals_is_checked_exactly_as_before() {
        let mut made = Made::registered("ordinary", 900);
        let base = made.holds("Base.wz", b"base of 1.2.900");
        let skill = made.holds("Skill.wz", b"skill of 1.2.900, minor 1");
        let of_minor = |minor: u32| (skill.0.clone(), mfp_engine::crc::update(0, format!("minor {minor}").as_bytes()));
        made.main_patch(900, 901, &[base, of_minor(3)]);
        made.minor_patch(900, (1, 2), std::slice::from_ref(&skill), &[("Data/Skill.wz".to_owned(), of_minor(2).1)]);
        made.minor_patch(900, (2, 3), &[of_minor(2)], &[("Data/Skill.wz".to_owned(), of_minor(3).1)]);
        made.minor_patch(900, (1, 3), std::slice::from_ref(&skill), &[("Data/Skill.wz".to_owned(), of_minor(3).1)]);

        let answer = made.check();
        let target = answer.target.expect("the minor patch from the first minor to the newest");
        assert_eq!((target.label.as_str(), target.version, target.minor), ("1.2.900 (3)", 900, Some(3)));
        assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00900/Minor/01to03.patch");
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.901"), None));
        let files = "https://maplestory.dn.nexoncdn.co.kr/Patch";
        let asked: Vec<String> = [
            // What the server has for the version the registry says.
            "size {files}/00901/00900to00901.patch",
            "size {files}/00900/Minor/01to02.patch",
            "size {files}/00900/Minor/02to03.patch",
            "size {files}/00900/Minor/03to04.patch",
            // The newest version there is.
            "size {files}/00902/00901to00902.patch",
            "size {files}/00901/Minor/01to02.patch",
            // Which minor patch the folder needs: the newest is read and is not in place,
            // and the one from the first minor starts from the files the folder has.
            "read {files}/00900/Minor/02to03.patch",
            "originals {files}/00900/Minor/01to03.patch",
            "size {files}/00900/Minor/01to03.patch",
        ]
        .iter()
        .map(|question| question.replace("{files}", files))
        .collect();
        assert_eq!(*made.shelf.asked.borrow(), asked);
        // No proof was looked for, and none is on record: the patch is its own.
        assert!(!version_proven(made.site(), &made.install, 900));
        assert!(!crate::records::found_unproven(made.site(), &made.install));
        assert_eq!(installed_version(made.site(), &made.install), Some(900));

        // At the next check, with nothing changed, only what a new patch would show by.
        made.shelf.asked.borrow_mut().clear();
        assert_eq!(made.check().target, Some(target));
        assert_eq!((made.asked("size"), made.asked("read"), made.asked("originals")), (4, 0, 0));
    }

    /// The ordinary case of a new version: the main patch is offered on the registry's
    /// word, and nothing of it is fetched to offer it.
    #[test]
    fn a_registered_install_is_offered_the_main_patch_without_anything_of_it_being_read() {
        let mut made = Made::registered("ordinary-main", 900);
        let base = made.holds("Base.wz", b"base of 1.2.900");
        made.main_patch(900, 901, std::slice::from_ref(&base));
        let answer = made.check();
        let target = answer.target.expect("the main patch");
        assert_eq!((target.label.as_str(), target.minor, answer.error), ("1.2.901", None, None));
        assert_eq!((made.asked("size"), made.asked("read"), made.asked("originals")), (4, 0, 0));

        // As its run begins, it is asked once whether the patch names what it starts from:
        // the folder's version is the registry's word, and the patch is what will prove it.
        made.shelf.asked.borrow_mut().clear();
        assert!(checks_its_folder(made.site(), &made.install, &target, &made.shelf).unwrap());
        assert_eq!(*made.shelf.asked.borrow(), [format!("originals {}", target.url)]);

        // Of a folder this app has patched to the version itself, nothing is asked.
        record_version(made.site(), &made.install, 900);
        made.shelf.asked.borrow_mut().clear();
        assert!(checks_its_folder(made.site(), &made.install, &target, &made.shelf).unwrap());
        assert!(made.shelf.asked.borrow().is_empty());
    }

    /// A main patch that names no file it starts from would be laid over whatever the
    /// folder holds. For a folder whose version nothing proves its run ends before it has
    /// fetched or changed anything, and the patch is no longer offered.
    #[test]
    fn a_main_patch_that_names_nothing_is_not_applied_on_the_registrys_word() {
        let mut made = Made::registered("main-names-nothing", 900);
        made.main_patch(900, 901, &[]);
        let target = made.check().target.expect("the main patch, of which nothing was read yet");
        assert!(!checks_its_folder(made.site(), &made.install, &target, &made.shelf).unwrap());
        let answer = made.check();
        assert_eq!((answer.target, answer.error.as_deref()), (None, Some("패치 형식 미지원")));

        // The same patch for a folder that is known to be at the version it starts from.
        record_version(made.site(), &made.install, 900);
        made.site().forget_found();
        let target = made.check().target.expect("the main patch");
        assert!(checks_its_folder(made.site(), &made.install, &target, &made.shelf).unwrap());
        // A minor patch was looked into before it was offered, and is not asked about again.
        let minor = Target { minor: Some(2), ..target };
        made.shelf.asked.borrow_mut().clear();
        assert!(checks_its_folder(made.site(), &made.install, &minor, &made.shelf).unwrap());
        assert!(made.shelf.asked.borrow().is_empty());
    }

    /// With the last step of the minors being one that names what it starts from, the
    /// registry's word is enough to offer it: the run compares the folder with it first.
    #[test]
    fn the_one_minor_patch_there_is_is_offered_on_the_registrys_word_when_it_names_its_originals() {
        let mut made = Made::registered("one-minor", 900);
        let skill = made.holds("Skill.wz", b"skill of 1.2.900, minor 1");
        made.minor_patch(900, (1, 2), std::slice::from_ref(&skill), &[("Data/Skill.wz".to_owned(), 7)]);
        let target = made.check().target.expect("the minor patch");
        assert_eq!((target.label.as_str(), target.minor), ("1.2.900 (2)", Some(2)));
        assert_eq!(made.asked("originals"), 0);

        // The same step as a patch of whole files only: not on the registry's word.
        let mut made = Made::registered("one-minor-whole-files", 900);
        made.holds("Skill.wz", b"skill of 1.2.900, minor 1");
        made.minor_patch(900, (1, 2), &[], &[("Data/Skill.wz".to_owned(), 7)]);
        assert_eq!(made.check(), unproven("1.2.900 (2)"));
    }

    /// A step that is found in place shows by the folder's own files which version and
    /// minor it is at. The patch from there is offered, whatever kind it is.
    #[test]
    fn a_minor_step_that_is_in_place_proves_the_folder_for_the_patch_from_there() {
        let mut made = Made::registered("step-in-place", 900);
        let (_, string) = made.holds("String.wz", b"string of 1.2.900, minor 2");
        made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), string)]);
        made.minor_patch(900, (2, 3), &[], &[("Data/Skill.wz".to_owned(), 7)]);
        made.minor_patch(900, (3, 4), &[], &[("Data/Skill.wz".to_owned(), 8)]);
        made.minor_patch(900, (2, 4), &[], &[("Data/Skill.wz".to_owned(), 8)]);
        let target = made.check().target.expect("the patch from the second minor");
        assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00900/Minor/02to04.patch");
        assert_eq!(known_minor(made.site(), &made.install, 900), Some(2));
    }

    /// An address the user gave proves nothing of the folder it is given for, and no check
    /// has looked into the patch there. Whichever patch it is, a run of it has to have
    /// something to hold the folder against: the files the patch names as what it starts
    /// from, or what is known of the folder's version.
    #[test]
    fn a_patch_given_by_its_address_is_applied_only_where_there_is_something_to_hold_the_folder_against() {
        let mut made = Made::registered("given-address", 900);
        let skill = made.holds("Skill.wz", b"skill of 1.2.900, minor 2");
        made.minor_patch(900, (2, 3), std::slice::from_ref(&skill), &[("Data/Skill.wz".to_owned(), 7)]);
        made.minor_patch(900, (1, 3), &[], &[("Data/Skill.wz".to_owned(), 7)]);
        made.main_patch(900, 901, &[]);
        let names_them = mfp_kms::LIVE.minor_patch_url(900, 2, 3);
        let names_none = mfp_kms::LIVE.minor_patch_url(900, 1, 3);
        let main_names_none = mfp_kms::LIVE.patch_url(900, 901);
        let given =
            |made: &Made, url: &str, from| given_checks_its_folder(made.site(), &made.install, url, from, &made.shelf);

        // The registry's word for the version is no more than that. Each patch is asked for
        // what it names, a minor one as well: no check has read it for this folder.
        assert!(given(&made, &names_them, Some(900)).unwrap());
        assert!(!given(&made, &names_none, Some(900)).unwrap());
        assert!(!given(&made, &main_names_none, Some(900)).unwrap());
        assert_eq!(made.asked("originals"), 3);

        // The folder is known to be at the version the patches start from: this app patched
        // it to that version. Nothing has to be asked.
        record_version(made.site(), &made.install, 900);
        made.shelf.asked.borrow_mut().clear();
        for url in [&names_them, &names_none, &main_names_none] {
            assert!(given(&made, url, Some(900)).unwrap(), "{url}");
        }
        assert!(made.shelf.asked.borrow().is_empty());
        // That says nothing for a patch that starts from another version, or for one whose
        // address does not say which version it starts from.
        assert!(!given(&made, &names_none, Some(899)).unwrap());
        assert!(!given(&made, &names_none, None).unwrap());
        assert!(given(&made, &names_them, None).unwrap());
    }

    #[test]
    fn a_patch_starts_from_the_version_its_main_patch_leads_on_from_or_its_minor_patch_is_of() {
        let main = crate::made::target("1.2.422");
        assert_eq!(starts_from(&main), Some(421));
        let skips = Target { file_name: "00417to00422.patch".to_owned(), ..main.clone() };
        assert_eq!(starts_from(&skips), Some(417));
        let minor = Target { file_name: "00421-minor-03to04.patch".to_owned(), version: 421, minor: Some(4), ..main };
        assert_eq!(starts_from(&minor), Some(421));
    }

    /// An apply was cut off in the folder and its record cannot be read. Until the folder
    /// has been put back, nothing is worked out of it and nothing is asked about it, and
    /// no word is said of a damaged record: putting it back is what comes first.
    #[test]
    fn nothing_is_worked_out_of_a_folder_that_is_still_to_be_put_back() {
        let mut made = Made::registered("check-cut-off", 900);
        made.main_patch(900, 901, &[]);
        let journal = stage::Dirs::new(&made.install).journal();
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        std::fs::write(&journal, "this is no line of a journal\n").unwrap();
        let checked = check(&made.patcher, made.site(), &made.shelf, &mut None);
        assert_eq!(checked.end, CheckEnd::NotYet);
        assert_eq!(
            checked.answer,
            Availability { target: None, latest: None, error: Some("이전 패치 복구 대기".to_owned()) }
        );
        assert!(made.shelf.asked.borrow().is_empty());

        // Put back, it is a folder to ask about like any other.
        crate::ready::put_back(made.site(), &made.install);
        assert!(made.check().target.is_some());
    }

    /// The folder cannot be put back without the record either: what the apply moved
    /// aside carries no names. Nothing is worked out of such a folder and nothing is asked
    /// about it: the answer is that, for as long as what the run left is there.
    #[test]
    fn nothing_is_worked_out_of_a_folder_whose_apply_cannot_be_undone() {
        let mut made = Made::registered("check-unreadable-journal", 900);
        made.main_patch(900, 901, &[]);
        let dirs = stage::Dirs::new(&made.install);
        std::fs::create_dir_all(dirs.backup()).unwrap();
        std::fs::write(dirs.backup().join("0"), b"an original, under no name").unwrap();
        std::fs::write(dirs.journal(), "this is no line of a journal\n").unwrap();
        let said = Availability { target: None, latest: None, error: Some("작업 기록 손상".to_owned()) };
        assert_eq!(made.check(), said);
        assert!(made.shelf.asked.borrow().is_empty());

        // With what the run left removed as it is, the folder is one to ask about again.
        mfp_engine::run::discard_work_as_it_is(&made.install).unwrap();
        assert!(made.check().target.is_some());
    }
}
