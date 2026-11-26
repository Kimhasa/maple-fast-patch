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

use crate::records::{
    at_final, install_dir, install_mark, installed_version, known_minor, latest_seen, patch_cache, record_final,
    record_minor, record_proven, run_target, see_latest, settle_version,
};
use crate::state::{Availability, Found, Patcher, Site, Target, Unproven};
use crate::window::notify;
use crate::wording::{describe_check_error, drive_letter};

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
    if let Some(label) = appeared {
        notify(app, site, &format!("{label} 패치가 등록되었습니다."));
    }
    checked
}

/// What the footer says of a folder whose version nothing proves.
const UNPROVEN: &str = "현재 버전 확인 불가";

/// The check behind [`availability`], with the server asked through `shelf`. `appeared` is
/// set to the version a patch leads to that was not there at the check before.
fn check(patcher: &Patcher, site: &Site, shelf: &dyn Shelf, appeared: &mut Option<String>) -> Checked {
    let nothing = |error: Option<String>| Availability { error, ..Availability::nothing() };
    let Some(install) = install_dir(site) else { return Checked::answered(nothing(None)) };
    // A run that replaced game files as it built them was cut off, and the folder is partly
    // new. What this app remembers about a folder (its version, its minor, what the last
    // check found) goes by files that are no longer the ones it knew, so nothing about the
    // folder is worked out in this state. The patch to offer is the one that run was
    // applying. A run of a file the user picked is offered again by the status instead.
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
    if stage::Dirs::new(&install).journal().exists() {
        return Checked { answer: nothing(Some("이전 패치 복구 대기".to_owned())), end: CheckEnd::NotYet };
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
    let version = match installed_version(site, &install) {
        Some(version) => version,
        // Nothing on this PC says which version the folder is at, and this app has no
        // record of it: the folder's own files have to prove it.
        None => match prove(site, &install, shelf) {
            Ok(Proof::Found(version)) => version,
            // Without a proof nothing is said of the folder, and nothing is offered for it.
            Ok(Proof::None { latest }) => {
                return Checked::answered(Availability { target: None, latest, error: Some(UNPROVEN.to_owned()) });
            }
            Err(err) => return unreachable(&err),
        },
    };

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
        (found.version, found.main, found.newest_minor, &found.mark) == (version, main.is_some(), newest_minor, &mark)
    };
    if let Some(found) = known.as_ref().filter(same) {
        if found.error.is_none() {
            note_check_worked(site);
        }
        return Checked::answered(found.availability());
    }
    // The newest version on the server: main patches are followed for as long as there is
    // a next one, then the minors of the last.
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
    // version the server was seen to have. Where the single steps from the folder's version
    // do not lead all the way there, that version is still the newest there is.
    let ahead = latest_seen(site).filter(|seen| site.version_guess().is_none() && *seen > newest);
    let latest = match ahead {
        Some(seen) => site.client.version_label(seen),
        None => full_label(site.client, newest, minor_of_newest),
    };

    let (target, error) = if ahead.is_some() && main.is_none() {
        // With no step to take from the folder's version, there is no way from it to the
        // newest that this app takes, and the folder is not called up to date for it.
        (None, Some(failed(&Error::NotPublished)))
    } else {
        match pending_minor(site, &install, version, newest_minor, shelf) {
            Ok(Some(minor)) => (Some(minor), None),
            Ok(None) => {
                let main = main.map(|size| Target {
                    label: site.client.version_label(version + 1),
                    url: site.patch_url(version, version + 1),
                    file_name: site.client.patch_file_name(version, version + 1),
                    size,
                    version: version + 1,
                    minor: None,
                });
                (main, None)
            }
            // The install matches nothing the server offers, or what the server offers is in
            // a form this app cannot read. That stays true until the server or the folder
            // changes, so it is remembered like any other answer instead of fetching the same
            // patch files again on every check.
            Err(err @ (Error::OriginalMismatch(_) | Error::UnknownForm(_))) => (None, Some(failed(&err))),
            Err(Error::Stopped) => return given_up(),
            // Anything else may pass; the next check tries again.
            Err(err) => {
                let answer = Availability { target: None, latest: Some(latest), error: Some(failed(&err)) };
                return Checked::failed(answer);
            }
        }
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
        *appeared = Some(target.label.clone());
    }
    let answer = found.availability();
    *site.found.lock().unwrap() = Some(found);
    Checked::answered(answer)
}

/// How many main versions back from the newest a folder's version is looked for.
const PROOF_REACH: u32 = 5;

/// What looking for a proof of a folder's version came to.
enum Proof {
    /// The folder is at this main version. What proves it is on record, with the minor it
    /// is at where the proof says that too.
    Found(u32),
    /// Nothing proves which version the folder is at. `latest` is the newest version the
    /// server has, as far as that could be made out.
    None { latest: Option<String> },
}

/// Proves the main version of an install folder that nothing on this PC speaks for, from
/// the folder's own files, and writes down what was proven.
///
/// The newest version on the server is looked for upwards from the highest version this
/// client is known to have had: the newest the site has seen the server have, or the one
/// that is known of the client from the start. Against that newest version N the folder's
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
fn prove(site: &Site, install: &Path, shelf: &dyn Shelf) -> Result<Proof, Error> {
    let Some(from) = latest_seen(site).max(site.client.known_version()) else {
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
    if site.unproven.lock().unwrap().as_ref() == Some(&went_by) {
        return Ok(Proof::None { latest });
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
    for version in (newest.saturating_sub(PROOF_REACH)..newest).rev() {
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
}

/// The minor patch this install still needs, if any.
///
/// The game records its minor version nowhere readable, so the newest minor patch is read in
/// full and compared with the install: if every file it writes is already in place, the
/// install is up to date. That answer is remembered per install folder and minor version.
fn pending_minor(
    site: &Site,
    install: &Path,
    version: u32,
    newest: u32,
    shelf: &dyn Shelf,
) -> Result<Option<Target>, Error> {
    let known = known_minor(site, install, version);
    if newest <= 1 || known == Some(newest) || at_final(site, install, version) {
        return Ok(None);
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
            Some(size) => Ok(Some(jump(at, size))),
            None => mismatch(),
        };
    }

    let stop: &AtomicBool = &site.check_stop;
    let last_step = shelf.minor(version, newest - 1, newest)?;
    if last_step.in_place(install, stop)? {
        record_minor(site, install, version, newest);
        return Ok(None);
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
                        return Ok(Some(jump(from, shelf.size_of(&url)?.ok_or(Error::NotPublished)?)));
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
    // everything the step to k writes is in place. With no step in place it is at the first.
    let mut at = 1;
    for to in (2..newest).rev() {
        let step = shelf.minor(version, to - 1, to)?;
        if step.in_place(install, stop)? {
            record_minor(site, install, version, to);
            at = to;
            break;
        }
    }
    if at == newest - 1 {
        return Ok(Some(jump(at, last_step.size())));
    }
    // A jump that names its starting files was already compared with the install above.
    if ruled_out.contains(&at) {
        return mismatch();
    }
    match shelf.size_of(&site.minor_patch_url(version, at, newest))? {
        Some(size) => Ok(Some(jump(at, size))),
        None => mismatch(),
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

/// The main version a patch file leads to, going by its name, when it starts from
/// `installed`.
pub(crate) fn version_after(file: &Path, installed: u32) -> Option<u32> {
    versions_in_name(file).filter(|(from, _)| *from == installed).map(|(_, to)| to)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::harness::StandIns;
    use crate::records::{choose_dir, patched_before, record_version};
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
        assert_eq!(versions_in_name(Path::new(r"D:\patches\00420to00421.patch")), Some((420, 421)));
        assert_eq!(version_after(Path::new("00420to00421.patch"), 420), Some(421));
        // A file that starts from another version says nothing about where this install ends up.
        assert_eq!(version_after(Path::new("00420to00421.patch"), 419), None);
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
            Ok(Box::new(Read { size: file.size, writes: file.writes.clone() }))
        }
    }

    /// A minor patch of the made-up shelf, read.
    struct Read {
        size: u64,
        writes: BTreeMap<String, u32>,
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
            let scratch = Scratch::new(name);
            let install = scratch.0.join("game");
            std::fs::create_dir_all(install.join("Data")).unwrap();
            std::fs::write(install.join(client.game_program), b"the game").unwrap();
            let data = scratch.0.join("data");
            let site = Site::new(client, data.join(client.server.key()), stand_ins);
            choose_dir(&site, Some(&install.to_string_lossy())).unwrap();
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
    /// from like any other; the way from there is the single steps.
    #[test]
    fn a_patch_that_skips_versions_proves_the_one_it_starts_from() {
        let mut made = Made::test_world("proof-skip");
        let base = made.holds("Base.wz", b"base of 1.2.204");
        made.main_patch(1204, 1208, std::slice::from_ref(&base));
        // No step leads on from 1.2.204: the folder is behind, and nothing is offered.
        let answer = made.check();
        assert_eq!(installed_version(made.site(), &made.install), Some(1204));
        assert_eq!((answer.target, answer.latest.as_deref()), (None, Some("1.2.208")));
        assert_eq!(answer.error.as_deref(), Some("패치 확인 불가"));

        // With the step to the next version on the shelf, that step is the patch.
        made.main_patch(1204, 1205, &[base]);
        let answer = made.check();
        let target = answer.target.expect("the step to the next version");
        assert_eq!((target.label.as_str(), target.version), ("1.2.205", 1205));
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("1.2.208"), None));
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
    /// registry's guess is what the folder is taken to be at.
    #[test]
    fn a_folder_the_registry_speaks_for_is_checked_as_before() {
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

        // A minor patch that says nothing of its folder is offered on the strength of the
        // guess, as it has always been for the live game.
        made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);
        let target = made.check().target.expect("the minor patch");
        assert_eq!((target.label.as_str(), target.minor), ("1.2.900 (2)", Some(2)));
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
}
