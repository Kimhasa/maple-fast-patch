//! What the app writes down about an install folder, and where: which folder it is, the
//! version and minor it was found to be at, and the run that was left unfinished in it.
//!
//! All of it is kept in the data folder of the [`Site`] the folder belongs to, and nothing
//! here asks for more than that site.

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

use mfp_engine::{Error, detect};

use crate::address::file_at;
use crate::offer::{Shelf, full_label, version_name};
use crate::state::{Site, Target};

/// Where patch files are kept that are only fetched to tell which patch an install needs.
/// The patch that is applied is downloaded next to the game instead; see [`start_patch`](crate::patching::start_patch).
pub(crate) fn patch_cache(site: &Site) -> PathBuf {
    site.data.join("patches")
}

/// Size and time of the game's program file, which every patch replaces. A record made
/// under one mark says nothing about a folder that now shows another: the game was patched
/// by something else, reinstalled, or the folder was replaced.
pub(crate) fn install_mark(site: &Site, install: &Path) -> Option<String> {
    let meta = std::fs::metadata(install.join(site.client.game_program)).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{} {}", meta.len(), modified.as_secs()))
}

/// What the record `name` of `install` says, for as long as the folder's files are the ones
/// it was written about. Such a record is what it says on one line, and under it the mark
/// the folder showed then.
fn read_under_mark(site: &Site, install: &Path, name: &str) -> Option<String> {
    let record = std::fs::read_to_string(records(site, install).ok()?.join(name)).ok()?;
    let mut lines = record.lines();
    let said = lines.next()?.trim().to_owned();
    (lines.next().map(str::trim) == install_mark(site, install).as_deref()).then_some(said)
}

/// Writes `said` down as the record `name` of `install`, to hold for as long as the folder's
/// files stay as they are now.
fn write_under_mark(site: &Site, install: &Path, name: &str, said: &str) {
    if let (Ok(dir), Some(mark)) = (records(site, install), install_mark(site, install)) {
        let _ = std::fs::write(dir.join(name), format!("{said}\n{mark}"));
    }
}

/// The minor version `install` is known to be at, once a check or a patch run has found out.
pub(crate) fn known_minor(site: &Site, install: &Path, version: u32) -> Option<u32> {
    let record = read_under_mark(site, install, "minor-in-place")?;
    let (main, minor) = record.split_once(' ')?;
    if main.parse() == Ok(version) { minor.parse().ok() } else { None }
}

/// Remembers that `install` is at `minor` of `version`, for as long as its files stay as
/// they are now.
pub(crate) fn record_minor(site: &Site, install: &Path, version: u32, minor: u32) {
    write_under_mark(site, install, "minor-in-place", &format!("{version} {minor}"));
}

/// The install folder: the one the user picked, or the one the Nexon installer recorded.
pub(crate) fn install_dir(site: &Site) -> Option<PathBuf> {
    let dir = match chosen_dir(site) {
        // A picked folder that has gone missing (an unplugged drive, a deleted copy) is not
        // quietly replaced by another install: a patch meant for one folder must never land
        // in another.
        Some(dir) => dir.is_dir().then_some(dir),
        None => site.registered_dir(),
    };
    // A build made for trying the app out leaves the real game alone, wherever the folder
    // came from.
    dir.filter(|dir| !site.refuses(dir, || is_registered_install(site, dir)))
}

/// The file that holds the folder the user picked.
const CHOSEN: &str = "install-dir";

/// The folder the user picked as the install folder, whether or not it is still there.
pub(crate) fn chosen_dir(site: &Site) -> Option<PathBuf> {
    let chosen = std::fs::read_to_string(site.data.join(CHOSEN)).ok()?;
    Some(PathBuf::from(chosen.trim()))
}

/// Writes down the folder the user picked as the install folder. `None` forgets it, and
/// the folder is the one the Nexon installer recorded again.
pub(crate) fn choose_dir(site: &Site, folder: Option<&str>) -> std::io::Result<()> {
    match folder {
        Some(folder) => {
            std::fs::create_dir_all(&site.data)?;
            std::fs::write(site.data.join(CHOSEN), folder)
        }
        None => {
            let _ = std::fs::remove_file(site.data.join(CHOSEN));
            Ok(())
        }
    }
}

/// Where this app keeps what it knows about one install folder. Each folder has its own
/// records, so a second install or a restored backup is never judged by another's history.
pub(crate) fn records(site: &Site, install: &Path) -> Result<PathBuf, Error> {
    let real = install.canonicalize().unwrap_or_else(|_| install.to_path_buf());
    let dir = site.data.join("installs").join(install_key(&real));
    std::fs::create_dir_all(&dir).map_err(|e| Error::Io { what: format!("create {}", dir.display()), source: e })?;
    Ok(dir)
}

/// What the folder with the records of an install folder is called: a number worked out
/// from the install folder's real path, written in small letters or capitals alike.
fn install_key(real: &Path) -> String {
    format!("{:08x}", mfp_engine::crc::update(0, real.to_string_lossy().to_lowercase().as_bytes()))
}

/// Whether `install` is the folder the Nexon installer registered, as opposed to a copy or a
/// folder the user picked.
pub(crate) fn is_registered_install(site: &Site, install: &Path) -> bool {
    let real = |path: &Path| path.canonicalize().ok();
    site.registered_dir().is_some_and(|registered| real(&registered) == real(install))
}

/// The installed main version of `install`, as far as anything says.
///
/// What [a check](crate::offer::availability) has proven from the folder's own files goes
/// before anything the registry says, for as long as those files stay as they were.
///
/// Without such a proof, the registered install goes by the registry's value, or by the
/// version this app last patched to when that is newer (the registry value only catches up
/// once the game has been started). The registry says nothing about any other folder, so a
/// copy goes by what this app has recorded for it, and only falls back to the registry
/// until a check has worked its version out.
///
/// What the registry says is where the look for a folder's patches starts, and no more: it
/// is a guess. What may be offered on a guess alone is decided where a patch is offered;
/// see [`version_proven`].
///
/// Where nothing says anything, as for every test client that this app has not patched yet,
/// the version is not known until a check has proven it.
pub(crate) fn installed_version(site: &Site, install: &Path) -> Option<u32> {
    let recorded = recorded_version(site, install);
    if let Some(proven) = proven_version(site, install) {
        return Some(proven).max(recorded);
    }
    if is_registered_install(site, install) {
        site.registry_version().max(recorded)
    } else {
        recorded.or_else(|| site.version_guess())
    }
}

/// Whether `install` is known to be at main version `version`, as opposed to being said to
/// be by the registry: this app patched it to that version, or a check found that version's
/// files in it, and the folder's files are as they were then.
///
/// A patch file that says which files it starts from needs none of this: the folder is
/// compared with what it says before anything is changed. One that does not say fits any
/// folder, and is only for a folder of which this holds.
pub(crate) fn version_proven(site: &Site, install: &Path, version: u32) -> bool {
    recorded_version(site, install) == Some(version)
        || proven_version(site, install) == Some(version)
        || known_minor(site, install, version).is_some()
        || at_final(site, install, version)
}

/// The main version a check has proven `install` to be at from its own files, for as long
/// as those stay as they were then.
fn proven_version(site: &Site, install: &Path) -> Option<u32> {
    read_under_mark(site, install, "proven-version")?.parse().ok()
}

/// Remembers that the files of `install` prove it to be at `version`.
pub(crate) fn record_proven(site: &Site, install: &Path, version: u32) {
    write_under_mark(site, install, "proven-version", &version.to_string());
}

/// Whether the last check looked for a proof of the version `install` is at and found none,
/// and the folder's files are as they were then. What the registry says of such a folder is
/// a guess that has just been found wanting, and is not shown as its version.
pub(crate) fn found_unproven(site: &Site, install: &Path) -> bool {
    let went_by = site.unproven.lock().unwrap();
    went_by.as_ref().is_some_and(|went_by| went_by.install == install && went_by.mark == install_mark(site, install))
}

/// Whether this app has patched `install` before, and the folder is as that patch left it.
pub(crate) fn patched_before(site: &Site, install: &Path) -> bool {
    recorded_version(site, install).is_some()
}

/// The file with the newest main version the server was seen to have for the client.
const LATEST_SEEN: &str = "latest-seen";

/// The newest main version the server of the site's client was ever seen to have. It is
/// where the newest is looked for from when nothing is known of an install folder.
pub(crate) fn latest_seen(site: &Site) -> Option<u32> {
    std::fs::read_to_string(site.data.join(LATEST_SEEN)).ok()?.trim().parse().ok()
}

/// Notes that the server has main version `version`, when that is newer than any seen.
pub(crate) fn see_latest(site: &Site, version: u32) {
    if latest_seen(site) < Some(version) && std::fs::create_dir_all(&site.data).is_ok() {
        let _ = std::fs::write(site.data.join(LATEST_SEEN), version.to_string());
    }
}

/// Works out the version of a folder that is not the registered install, once.
///
/// Such a folder is usually a copy made earlier, so it can be one main version behind what
/// the registry says. It is behind exactly when it still holds the original files the
/// latest main patch starts from.
pub(crate) fn settle_version(site: &Site, install: &Path, shelf: &dyn Shelf) -> Result<(), Error> {
    let Some(hint) = unsettled(site, install) else { return Ok(()) };
    if hint > 1 {
        let before = site.patch_url(hint - 1, hint);
        match shelf.originals_of(&before) {
            Ok(Some(originals)) if !originals.is_empty() => {
                let mut checksums = site.checksums.lock().unwrap();
                // Only what the files prove is written down. A folder that does not fit goes
                // by the registry's hint, without that guess being recorded as a fact.
                if detect::fits(install, &originals, &mut checksums.1, &site.check_stop)? {
                    record_version(site, install, hint - 1);
                    // The main patch lists every file of the version it starts from. A folder
                    // that holds them all is at that version's last minor: the main patch is
                    // what it needs next, and no minor has to be looked for.
                    record_final(site, install, hint - 1);
                }
            }
            Ok(_) | Err(Error::NotPublished) => {}
            Err(err) => return Err(err),
        }
    }
    let _ = std::fs::write(records(site, install)?.join("version-asked"), asked_note(site, hint, install));
    Ok(())
}

/// The registry's version, when that guess is all there is to go by for `install` and the
/// folder's own files have not been looked at yet. `None` once the version is settled: for
/// the registered install always, for any other folder when a record says what it is at or
/// [`settle_version`] has looked.
pub(crate) fn unsettled(site: &Site, install: &Path) -> Option<u32> {
    let known = recorded_version(site, install).or_else(|| proven_version(site, install));
    if is_registered_install(site, install) || known.is_some() {
        return None;
    }
    let hint = site.version_guess()?;
    // A minor that was verified for this folder was verified against patches of this main
    // version, so the folder is at it.
    if known_minor(site, install, hint).is_some() {
        return None;
    }
    // Asked before for the folder as it is now, without a yes: asking again would read the
    // whole folder again for the same answer. Remembered across restarts.
    let asked = std::fs::read_to_string(records(site, install).ok()?.join("version-asked")).ok();
    (asked != Some(asked_note(site, hint, install))).then_some(hint)
}

/// What is written down once a folder has been compared with the patch that leads to `hint`.
fn asked_note(site: &Site, hint: u32, install: &Path) -> String {
    format!("{hint}\n{}", install_mark(site, install).unwrap_or_default())
}

/// Whether `install` was found to hold every file the main patch from `version` starts from,
/// and its files have not changed since.
pub(crate) fn at_final(site: &Site, install: &Path, version: u32) -> bool {
    read_under_mark(site, install, "final-in-place").and_then(|main| main.parse().ok()) == Some(version)
}

pub(crate) fn record_final(site: &Site, install: &Path, version: u32) {
    write_under_mark(site, install, "final-in-place", &version.to_string());
}

/// The main version this app worked out or patched `install` to, for as long as the folder's
/// files stay as they were then.
fn recorded_version(site: &Site, install: &Path) -> Option<u32> {
    read_under_mark(site, install, "applied-version")?.parse().ok()
}

pub(crate) fn record_version(site: &Site, install: &Path, version: u32) {
    write_under_mark(site, install, "applied-version", &version.to_string());
}

/// Forgets what was known of the version `install` is at: what this app patched it to, and
/// what checks proved of it. For a folder whose files have been left a mix of two versions,
/// with nothing but those files to say so: what was known of it was known of other files.
pub(crate) fn forget_version(site: &Site, install: &Path) {
    if let Ok(dir) = records(site, install) {
        for record in ["applied-version", "proven-version", "minor-in-place", "final-in-place"] {
            let _ = std::fs::remove_file(dir.join(record));
        }
    }
}

/// What an unfinished run wrote down about itself, for the run that takes the same patch up
/// again: also after the app was closed in between, or cut off.
///
/// Everything in it is of all the runs of that patch so far, not of the last one alone.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Left {
    /// The patch it was a run of: the name the file of a patch the app found is kept under,
    /// or the address of one the user gave.
    pub(crate) patch: String,
    /// How far the bar had come, in percent.
    pub(crate) percent: f64,
    /// How long the patch had been worked on.
    pub(crate) active: Duration,
    /// Game files that were changed for good. A run changes none before it is done, unless
    /// it replaces files as it builds them.
    pub(crate) files: usize,
    /// How many times the patch was paused.
    pub(crate) pauses: u32,
}

impl Left {
    /// The record as it is kept: one value to a line.
    pub(crate) fn written(&self) -> String {
        let Left { patch, percent, active, files, pauses } = self;
        // Cut to hundredths, not rounded: what a restart shows is never a figure the run had
        // not come to.
        let percent = (percent * 100.0).floor() / 100.0;
        format!("{patch}\n{percent:.2}\n{}\n{files}\n{pauses}", active.as_millis())
    }

    /// Reads what [`Left::written`] wrote.
    fn read(record: &str) -> Option<Left> {
        let mut lines = record.lines();
        let patch = lines.next()?.to_owned();
        let percent: f64 = lines.next()?.parse().ok()?;
        let millis: u64 = lines.next()?.parse().ok()?;
        // The two counts were added later. A record from before has none, and counts none.
        let files = lines.next().and_then(|line| line.parse().ok()).unwrap_or(0);
        let pauses = lines.next().and_then(|line| line.parse().ok()).unwrap_or(0);
        // A run can be left at any point before the apply, which is the last twentieth of
        // the bar.
        Some(Left { patch, percent: percent.clamp(0.0, 95.0), active: Duration::from_millis(millis), files, pauses })
    }
}

/// What an unfinished run in `install` wrote down.
pub(crate) fn left_record(site: &Site, install: &Path) -> Option<Left> {
    Left::read(&std::fs::read_to_string(records(site, install).ok()?.join("run-left")).ok()?)
}

/// What an unfinished run of the patch `patch` in `install` wrote down.
pub(crate) fn left_run(site: &Site, install: &Path, patch: &str) -> Option<Left> {
    left_record(site, install).filter(|left| left.patch == patch)
}

/// Writes a record so that it is the old one or the new one whenever the app is cut off,
/// never half of either: under another name first, then moved into its place. A run writes
/// how far it has come many times, and may be ended at any of them.
pub(crate) fn write_whole(path: &Path, record: &str) -> std::io::Result<()> {
    let fresh = path.with_extension(FRESH);
    std::fs::write(&fresh, record)?;
    std::fs::rename(&fresh, path)
}

/// What a record is called while it is being written; see [`write_whole`].
const FRESH: &str = "new";

/// Forgets the unfinished run in `install`, how far it had come and what it was applying:
/// its work is gone, or it has finished.
pub(crate) fn forget_left_run(site: &Site, install: &Path) {
    if let Ok(dir) = records(site, install) {
        for record in ["run-left", "run-target"] {
            let _ = std::fs::remove_file(dir.join(record));
        }
        // What a write of the first one was cut off at, if one was.
        let _ = std::fs::remove_file(dir.join("run-left").with_extension(FRESH));
    }
}

/// Writes down what the run that is about to begin in `install` applies, for as long as it
/// is unfinished: everything about `found`, the patch a check found, that is needed to
/// offer it again, or, of a patch the user gave the address of, that `address`.
///
/// A run that replaces game files as it builds them leaves the folder partly new when it is
/// cut off. Nothing this app otherwise knows about the folder holds then, and the patch to
/// finish is the one written here.
pub(crate) fn record_run_target(site: &Site, install: &Path, found: Option<&Target>, address: &str) {
    let record = match found {
        Some(target) => {
            let minor = target.minor.map_or("-".to_owned(), |minor| minor.to_string());
            let Target { label, url, file_name, size, version, .. } = target;
            format!("server\n{label}\n{url}\n{file_name}\n{size}\n{version}\n{minor}")
        }
        None => format!("address\n{address}"),
    };
    if let Ok(dir) = records(site, install) {
        let _ = std::fs::write(dir.join("run-target"), record);
    }
}

/// The patch from the server that the unfinished run in `install` was applying, as
/// [`record_run_target`] wrote it down. `None` when that run was of a patch the user gave
/// the address of: the status offers that address again by itself, once it has held it
/// against the rule for such addresses.
///
/// Of what was written down, only two things are taken: the name the patch file is kept
/// under, which says which patch it is, and its size. The address it is fetched from and
/// everything else about it are made anew from that name, the way a check makes them. A
/// record is a file like any other on the PC: whatever stands in it, a download comes from
/// where this client's patches are published, and is saved under a name of this app's own.
pub(crate) fn run_target(site: &Site, install: &Path) -> Option<Target> {
    let record = std::fs::read_to_string(records(site, install).ok()?.join("run-target")).ok()?;
    let mut lines = record.lines();
    if lines.next()? != "server" {
        return None;
    }
    let (_label, _address, kept_as) = (lines.next()?, lines.next()?, lines.next()?);
    patch_kept_as(site, kept_as, lines.next()?.parse().ok()?)
}

/// The patch of the site's client that is kept on this PC under the name `kept_as`, as a
/// check describes it: a main patch (`00420to00421.patch`) or a minor one
/// (`00421-minor-03to04.patch`). `None` for any other name. `size` is its size in bytes.
fn patch_kept_as(site: &Site, kept_as: &str, size: u64) -> Option<Target> {
    let client = site.client;
    let numbers = |pair: &str| {
        let (from, to) = pair.split_once("to")?;
        Some((from.parse::<u32>().ok()?, to.parse::<u32>().ok()?)).filter(|(from, to)| from < to)
    };
    let stem = kept_as.strip_suffix(".patch")?;
    let target = match stem.split_once("-minor-") {
        Some((version, minors)) => {
            let (version, (from, to)) = (version.parse().ok()?, numbers(minors)?);
            Target {
                label: full_label(client, version, to),
                size,
                url: site.minor_patch_url(version, from, to),
                file_name: client.minor_patch_file_name(version, from, to),
                version,
                minor: Some(to),
            }
        }
        None => {
            let (from, to) = numbers(stem)?;
            Target {
                label: version_name(client, to),
                size,
                url: site.patch_url(from, to),
                file_name: client.patch_file_name(from, to),
                version: to,
                minor: None,
            }
        }
    };
    // A number can be written in more ways than the one the name of a patch file has it in.
    (target.file_name == kept_as).then_some(target)
}

/// The patch of the site's client that is published at `url`, as a check describes it.
/// `None` for an address that is not where one of this client's main or minor patches is
/// published. `size` is its size in bytes.
pub(crate) fn patch_at(site: &Site, url: &str, size: u64) -> Option<Target> {
    let published = site.patch_files();
    let (version, file) = url.strip_prefix(&published)?.strip_prefix('/')?.split_once('/')?;
    let kept_as = match file.strip_prefix("Minor/") {
        Some(minors) => format!("{version}-minor-{minors}"),
        None => file.to_owned(),
    };
    patch_kept_as(site, &kept_as, size).filter(|target| target.url == url)
}

/// What the file of the patch at `url` is kept as in an install folder while a run is at
/// it: under the app's own name for one of the client's own patches, and under the name
/// its address ends in for any other.
pub(crate) fn kept_as(site: &Site, url: &str) -> String {
    patch_at(site, url, 0).map_or_else(|| file_at(url).to_owned(), |target| target.file_name)
}

/// The patch from the server that a run in `install` is to apply: the one the last check
/// found, or, for a folder that a run left partly new (`half_patched`), the one that run
/// was applying.
pub(crate) fn to_apply(site: &Site, install: &Path, half_patched: bool) -> Option<Target> {
    if half_patched {
        return run_target(site, install);
    }
    site.found_for(install).and_then(|found| found.target)
}
