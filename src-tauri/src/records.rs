//! What the app writes down about an install folder, and where: which folder it is, the
//! version and minor it was found to be at, and the run that was left unfinished in it.
//!
//! All of it is kept in the data folder of the [`Site`] the folder belongs to, and nothing
//! here asks for more than that site.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mfp_engine::{Error, detect};

use crate::address::file_at;
use crate::offer::{Shelf, full_label};
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
                label: client.version_label(to),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::StandIns;
    use crate::offer::Published;
    use crate::scratch::Scratch;

    fn left(percent: f64, files: usize, pauses: u32) -> Left {
        Left { patch: "00420to00421.patch".to_owned(), percent, active: Duration::from_millis(68_270), files, pauses }
    }

    #[test]
    fn what_a_run_left_is_read_back_as_it_was_written() {
        let record = left(69.25, 470, 1);
        assert_eq!(record.written(), "00420to00421.patch\n69.25\n68270\n470\n1");
        assert_eq!(Left::read(&record.written()), Some(record));
    }

    #[test]
    fn a_record_from_before_the_counts_counts_none() {
        assert_eq!(Left::read("00420to00421.patch\n69.25\n68270"), Some(left(69.25, 0, 0)));
    }

    #[test]
    fn a_patch_the_user_gave_the_address_of_is_on_record_by_that_address() {
        let address = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch";
        let record = Left { patch: address.to_owned(), ..left(12.5, 0, 2) };
        assert_eq!(record.written(), format!("{address}\n12.50\n68270\n0\n2"));
        assert_eq!(Left::read(&record.written()), Some(record));
    }

    #[test]
    fn a_run_is_never_taken_up_inside_the_apply() {
        assert_eq!(Left::read("a.patch\n99.40\n1000\n3\n0").map(|left| left.percent), Some(95.0));
    }

    #[test]
    fn a_record_that_is_cut_short_or_not_one_is_no_record() {
        for record in ["", "a.patch", "a.patch\n69.01", "a.patch\nsixty\n1000", "a.patch\n69.01\nlong"] {
            assert_eq!(Left::read(record), None, "{record:?}");
        }
    }

    /// The figure on record is what a restart shows. The screen shows whole percent, so the
    /// two are the same figure, and the record is never ahead of where the run was.
    #[test]
    fn the_figure_on_record_is_the_figure_that_was_shown() {
        for shown in [0.0, 6.99, 67.43, 67.996, 68.0, 69.004, 94.999] {
            let read = Left::read(&left(shown, 0, 0).written()).unwrap().percent;
            assert_eq!(read.floor(), f64::floor(shown), "{shown}");
            assert!(read <= shown && shown - read < 0.011, "{shown} was read back as {read}");
        }
    }

    #[test]
    fn a_record_is_replaced_whole_and_leaves_nothing_beside_it() {
        let scratch = Scratch::new("write-whole");
        let path = scratch.0.join("run-left");
        write_whole(&path, &left(10.0, 0, 0).written()).unwrap();
        write_whole(&path, &left(69.0, 470, 1).written()).unwrap();
        assert_eq!(Left::read(&std::fs::read_to_string(&path).unwrap()), Some(left(69.0, 470, 1)));
        let beside: Vec<_> = std::fs::read_dir(&scratch.0).unwrap().flatten().map(|file| file.file_name()).collect();
        assert_eq!(beside, ["run-left"]);
    }

    /// A write that was cut off has only got as far as the other name. What was there
    /// before is still there, whole.
    #[test]
    fn a_write_that_was_cut_off_leaves_the_record_as_it_was() {
        let scratch = Scratch::new("cut-off");
        let path = scratch.0.join("run-left");
        write_whole(&path, &left(67.0, 400, 0).written()).unwrap();
        std::fs::write(path.with_extension(FRESH), "00420to00421.patch\n69").unwrap();
        assert_eq!(Left::read(&std::fs::read_to_string(&path).unwrap()), Some(left(67.0, 400, 0)));
    }

    // From here on the records are files, written out below in exactly the forms the
    // release before the sites wrote them. A test that fails here has found a change that
    // would make the app forget what it knew of somebody's install.

    /// The mark of the made-up install: its game program is this long and was last written
    /// at this second.
    const MARK: &str = "4096 1764000000";

    /// A site of the live game with a data folder of its own, and beside it an install
    /// folder the registry knows nothing of, with the mark [`MARK`].
    struct Made {
        site: Site,
        install: PathBuf,
        _scratch: Scratch,
    }

    impl Made {
        fn new(name: &str) -> Made {
            Made::with(name, StandIns::default())
        }

        fn with(name: &str, stand_ins: StandIns) -> Made {
            Made::set_up(name, |_| stand_ins)
        }

        /// The same with the install folder being the one the Nexon installer registered,
        /// and `version` what the registry gives for it.
        fn registered(name: &str, version: u32) -> Made {
            Made::set_up(name, |install| StandIns::registered_at(install, version))
        }

        fn set_up(name: &str, stand_ins: impl FnOnce(&Path) -> StandIns) -> Made {
            let scratch = Scratch::new(name);
            let install = scratch.0.join("Maple");
            std::fs::create_dir_all(&install).unwrap();
            let program = install.join("MapleStory.exe");
            std::fs::write(&program, vec![0; 4096]).unwrap();
            let written = std::time::UNIX_EPOCH + Duration::from_secs(1_764_000_000);
            std::fs::File::options().write(true).open(&program).unwrap().set_modified(written).unwrap();
            let site = Site::new(&mfp_kms::LIVE, scratch.0.join("data"), stand_ins(&install));
            Made { site, install, _scratch: scratch }
        }

        /// Puts the record `name` of the install folder there as `text`.
        fn given(&self, name: &str, text: &str) {
            std::fs::write(self.record(name), text).unwrap();
        }

        fn record(&self, name: &str) -> PathBuf {
            records(&self.site, &self.install).unwrap().join(name)
        }

        /// What the record `name` holds now.
        fn kept(&self, name: &str) -> String {
            std::fs::read_to_string(self.record(name)).unwrap()
        }
    }

    #[test]
    fn everything_of_a_site_is_kept_in_its_data_folder() {
        let made = Made::new("layout");
        let data = &made.site.data;
        assert_eq!(patch_cache(&made.site), data.join("patches"));
        let dir = records(&made.site, &made.install).unwrap();
        assert_eq!(dir.parent(), Some(data.join("installs").as_path()));
        assert!(dir.is_dir());
        // Another folder has records of its own, and the same folder the same ones however
        // its path is written.
        let other = made.install.with_file_name("Copy");
        std::fs::create_dir_all(&other).unwrap();
        assert_ne!(records(&made.site, &other).unwrap(), dir);
        let shouted = PathBuf::from(made.install.to_string_lossy().to_uppercase());
        assert_eq!(records(&made.site, &shouted).unwrap(), dir);
    }

    /// The name of the folder with an install's records, for the live game where the Nexon
    /// installer puts it. The path is the one Windows gives for the folder itself, and the
    /// name is the one the release keeps that folder's records under.
    #[test]
    fn the_records_of_an_install_folder_are_found_under_the_same_name_as_before() {
        assert_eq!(install_key(Path::new(r"\\?\C:\Nexon\Maple")), "4f70a139");
        assert_eq!(install_key(Path::new(r"\\?\c:\NEXON\maple")), install_key(Path::new(r"\\?\C:\Nexon\Maple")));
    }

    #[test]
    fn the_mark_of_a_folder_is_the_size_and_the_time_of_its_game_program() {
        let made = Made::new("mark");
        assert_eq!(install_mark(&made.site, &made.install).as_deref(), Some(MARK));
        // A folder without the client's own program has none, whatever else is in it.
        std::fs::rename(made.install.join("MapleStory.exe"), made.install.join("MapleStoryT.exe")).unwrap();
        assert_eq!(install_mark(&made.site, &made.install), None);
    }

    #[test]
    fn the_version_an_install_is_at_is_read_and_written_as_before() {
        let made = Made::new("applied-version");
        made.given("applied-version", "421\n4096 1764000000");
        assert_eq!(recorded_version(&made.site, &made.install), Some(421));
        assert_eq!(installed_version(&made.site, &made.install), Some(421));
        record_version(&made.site, &made.install, 422);
        assert_eq!(made.kept("applied-version"), "422\n4096 1764000000");
        assert_eq!(recorded_version(&made.site, &made.install), Some(422));
    }

    /// The game was patched by something else since, or the record is from a release that
    /// did not write the mark yet, as some on people's PCs still are.
    #[test]
    fn a_record_made_under_another_mark_or_under_none_says_nothing_about_the_folder() {
        let made = Made::new("another-mark");
        for version in ["421\n4096 1763999999", "421"] {
            made.given("applied-version", version);
            assert_eq!(recorded_version(&made.site, &made.install), None, "{version:?}");
        }
        made.given("minor-in-place", "421 4\n8192 1764000000");
        made.given("final-in-place", "421");
        assert_eq!(known_minor(&made.site, &made.install, 421), None);
        assert!(!at_final(&made.site, &made.install, 421));
    }

    #[test]
    fn the_minor_an_install_is_at_is_read_and_written_as_before() {
        let made = Made::new("minor-in-place");
        made.given("minor-in-place", "421 4\n4096 1764000000");
        assert_eq!(known_minor(&made.site, &made.install, 421), Some(4));
        // It is the minor of that main version and of no other.
        assert_eq!(known_minor(&made.site, &made.install, 422), None);
        record_minor(&made.site, &made.install, 421, 5);
        assert_eq!(made.kept("minor-in-place"), "421 5\n4096 1764000000");
        assert_eq!(known_minor(&made.site, &made.install, 421), Some(5));
    }

    #[test]
    fn the_last_minor_of_a_version_is_read_and_written_as_before() {
        let made = Made::new("final-in-place");
        made.given("final-in-place", "420\n4096 1764000000");
        assert!(at_final(&made.site, &made.install, 420));
        assert!(!at_final(&made.site, &made.install, 421));
        record_final(&made.site, &made.install, 421);
        assert_eq!(made.kept("final-in-place"), "421\n4096 1764000000");
        assert!(at_final(&made.site, &made.install, 421));
    }

    #[test]
    fn what_a_run_left_is_read_from_its_file_with_the_counts_and_without() {
        let made = Made::new("run-left");
        made.given("run-left", "00420to00421.patch\n69.25\n68270\n470\n1");
        assert_eq!(left_record(&made.site, &made.install), Some(left(69.25, 470, 1)));
        assert_eq!(left_run(&made.site, &made.install, "00420to00421.patch"), Some(left(69.25, 470, 1)));
        // It is the record of that patch and of no other.
        assert_eq!(left_run(&made.site, &made.install, "00421to00422.patch"), None);
        // As the release before the counts wrote it.
        made.given("run-left", "00420to00421.patch\n69.25\n68270");
        assert_eq!(left_record(&made.site, &made.install), Some(left(69.25, 0, 0)));
    }

    #[test]
    fn a_patch_from_the_server_that_a_run_was_applying_is_read_and_written_as_before() {
        let made = Made::new("run-target-server");
        let main = "server\n1.2.421\nhttps://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch\n\
                    00420to00421.patch\n1234567890\n421\n-";
        made.given("run-target", main);
        let target = run_target(&made.site, &made.install).unwrap();
        let Target { label, size, url, file_name, version, minor } = &target;
        assert_eq!((label.as_str(), *size, *version, *minor), ("1.2.421", 1_234_567_890, 421, None));
        assert_eq!(url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch");
        assert_eq!(file_name, "00420to00421.patch");
        std::fs::remove_file(made.record("run-target")).unwrap();
        record_run_target(&made.site, &made.install, Some(&target), url);
        assert_eq!(made.kept("run-target"), main);

        let minor = "server\n1.2.421 (4)\nhttps://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch\n\
                     00421-minor-03to04.patch\n2345678\n421\n4";
        made.given("run-target", minor);
        let target = run_target(&made.site, &made.install).unwrap();
        assert_eq!((target.label.as_str(), target.minor), ("1.2.421 (4)", Some(4)));
        record_run_target(&made.site, &made.install, Some(&target), &target.url);
        assert_eq!(made.kept("run-target"), minor);
        // A folder that a run left partly new is offered that patch and no other.
        assert_eq!(to_apply(&made.site, &made.install, true).map(|target| target.file_name), Some(target.file_name));
        assert!(to_apply(&made.site, &made.install, false).is_none());
    }

    #[test]
    fn a_run_by_an_address_the_user_gave_is_written_down_with_the_address_and_is_no_patch_the_app_found() {
        let made = Made::new("run-target-address");
        let address = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch";
        // A run of a patch the app found was on record before: this one takes its place.
        made.given("run-target", &format!("server\n1.2.421\n{address}\n00420to00421.patch\n1234567890\n421\n-"));
        assert!(run_target(&made.site, &made.install).is_some());
        record_run_target(&made.site, &made.install, None, address);
        assert_eq!(made.kept("run-target"), format!("address\n{address}"));
        assert!(run_target(&made.site, &made.install).is_none());
        // A folder such a run left partly new is not offered any patch a check works out.
        assert!(to_apply(&made.site, &made.install, true).is_none());

        // What a release that took patch files from the user wrote of one is no patch
        // either, and no address.
        made.given("run-target", "file\nD:\\patches\\00420to00421.patch");
        assert!(run_target(&made.site, &made.install).is_none());
    }

    /// One of the client's own patches is the same patch whether a check found it or the
    /// user gave its address: where it leads and what its file is kept as are worked out
    /// from the address. Any other address is of a patch this app knows nothing more of.
    #[test]
    fn the_patch_at_an_address_among_the_clients_own_is_known_as_a_check_knows_it() {
        let made = Made::new("patch-at");
        let files = "https://maplestory.dn.nexoncdn.co.kr/Patch";
        let main = patch_at(&made.site, &format!("{files}/00421/00420to00421.patch"), 1000).expect("a main patch");
        assert_eq!((main.label.as_str(), main.version, main.minor, main.size), ("1.2.421", 421, None, 1000));
        assert_eq!(main.file_name, "00420to00421.patch");
        // A patch that skips versions is published under the one it leads to.
        let skips =
            patch_at(&made.site, &format!("{files}/00421/00417to00421.patch"), 1000).expect("a patch that skips");
        assert_eq!((skips.label.as_str(), skips.file_name.as_str()), ("1.2.421", "00417to00421.patch"));
        let minor = patch_at(&made.site, &format!("{files}/00421/Minor/03to04.patch"), 200).expect("a minor patch");
        assert_eq!((minor.label.as_str(), minor.version, minor.minor), ("1.2.421 (4)", 421, Some(4)));
        assert_eq!(minor.file_name, "00421-minor-03to04.patch");
        for target in [&main, &skips, &minor] {
            assert_eq!(kept_as(&made.site, &target.url), target.file_name);
            // The round trip: what a run of it writes down is read back as the same patch.
            record_run_target(&made.site, &made.install, Some(target), &target.url);
            assert_eq!(run_target(&made.site, &made.install).as_ref(), Some(target));
        }

        let unknown = [
            // Under another version's folder than the one it leads to, or under none.
            "/00422/00420to00421.patch",
            "/00420to00421.patch",
            "/421/00420to00421.patch",
            "/00421/420to421.patch",
            "/00421/00421to00420.patch",
            "/00421/minor/03to04.patch",
            "/00421/Minor/3to4.patch",
            "/00421/Minor/04to03.patch",
            "/00421/Minor/00421-minor-03to04.patch",
            "/00421/ExtraPatch.patch",
        ];
        for path in unknown {
            let url = format!("{files}{path}");
            assert_eq!(patch_at(&made.site, &url, 1000), None, "{path}");
            assert_eq!(kept_as(&made.site, &url), file_at(&url), "{path}");
        }
        // The other client's patches are none of this client's, under whatever folder.
        assert_eq!(patch_at(&made.site, &mfp_kms::TEST.patch_url(1207, 1208), 1000), None);
        assert_eq!(patch_at(&made.site, "https://maplestory.dn.nexoncdn.co.krT/00421/00420to00421.patch", 1), None);
    }

    #[test]
    fn a_run_that_is_forgotten_leaves_none_of_its_records() {
        let made = Made::new("forget");
        made.given("run-left", "00420to00421.patch\n69.25\n68270\n470\n1");
        made.given("run-left.new", "00420to00421.patch\n70");
        made.given("run-target", "file\nD:\\patches\\00420to00421.patch");
        made.given("applied-version", "420\n4096 1764000000");
        forget_left_run(&made.site, &made.install);
        let left: Vec<_> = std::fs::read_dir(records(&made.site, &made.install).unwrap())
            .unwrap()
            .flatten()
            .map(|file| file.file_name())
            .collect();
        assert_eq!(left, ["applied-version"]);
    }

    #[test]
    fn the_folder_the_user_picked_is_read_and_written_as_before() {
        let made = Made::new("install-dir");
        assert_eq!(chosen_dir(&made.site), None);
        // As the release wrote it, and as someone might write it by hand.
        std::fs::create_dir_all(&made.site.data).unwrap();
        for written in [r"D:\Games\Maple", "D:\\Games\\Maple\r\n"] {
            std::fs::write(made.site.data.join("install-dir"), written).unwrap();
            assert_eq!(chosen_dir(&made.site), Some(PathBuf::from(r"D:\Games\Maple")));
        }
        // A picked folder that is not there is not quietly replaced by another.
        assert_eq!(install_dir(&made.site), None);

        let picked = made.install.to_string_lossy().into_owned();
        choose_dir(&made.site, Some(&picked)).unwrap();
        assert_eq!(std::fs::read_to_string(made.site.data.join("install-dir")).unwrap(), picked);
        assert_eq!(install_dir(&made.site), Some(made.install.clone()));
        choose_dir(&made.site, None).unwrap();
        assert_eq!(chosen_dir(&made.site), None);
    }

    #[test]
    fn the_first_folder_to_be_picked_makes_the_data_folder() {
        let made = Made::new("first-pick");
        assert!(!made.site.data.exists());
        choose_dir(&made.site, Some(r"D:\Games\Maple")).unwrap();
        assert_eq!(chosen_dir(&made.site), Some(PathBuf::from(r"D:\Games\Maple")));
    }

    /// The guess at the version of a folder the registry does not name is looked into once
    /// for the folder as it is, and that it was is read and written as before.
    #[test]
    fn a_guess_that_was_looked_into_is_not_looked_into_again() {
        let made = Made::with("version-asked", StandIns::at_version(900));
        assert_eq!(unsettled(&made.site, &made.install), Some(900));
        assert_eq!(installed_version(&made.site, &made.install), Some(900));
        made.given("version-asked", "900\n4096 1764000000");
        assert_eq!(unsettled(&made.site, &made.install), None);
        // Asked about another guess, or about the folder as it was, is not asked about this.
        for other in ["899\n4096 1764000000", "900\n4096 1763999999"] {
            made.given("version-asked", other);
            assert_eq!(unsettled(&made.site, &made.install), Some(900), "{other:?}");
        }
        // A record of the version settles it for good.
        made.given("applied-version", "899\n4096 1764000000");
        assert_eq!(unsettled(&made.site, &made.install), None);
        assert_eq!(installed_version(&made.site, &made.install), Some(899));
    }

    /// No patch leads to the first version there can be, so nothing is asked of the server
    /// and only the note is written.
    #[test]
    fn looking_into_a_guess_writes_down_that_it_was_looked_into() {
        let made = Made::with("version-asked-written", StandIns::at_version(1));
        settle_version(&made.site, &made.install, &Published::of(&made.site)).unwrap();
        assert_eq!(made.kept("version-asked"), "1\n4096 1764000000");
        assert_eq!(unsettled(&made.site, &made.install), None);
    }

    #[test]
    fn a_build_for_trying_the_app_out_takes_a_made_up_folder_and_none_where_the_real_games_live() {
        let made = Made::with("turned-down", StandIns::of_a_session());
        choose_dir(&made.site, Some(&made.install.to_string_lossy())).unwrap();
        assert_eq!(install_dir(&made.site), Some(made.install.clone()));

        let real = made.install.with_file_name("Nexon").join("Maple");
        std::fs::create_dir_all(&real).unwrap();
        choose_dir(&made.site, Some(&real.to_string_lossy())).unwrap();
        assert_eq!(install_dir(&made.site), None);
        // The folder was picked all the same, and the screen says of it that it is not found.
        assert_eq!(chosen_dir(&made.site), Some(real.clone()));
        // A release build turns nothing down.
        let release = Site::new(&mfp_kms::LIVE, made.site.data.clone(), StandIns::default());
        assert_eq!(install_dir(&release), Some(real));
    }

    /// A record is a file on the PC, and whoever can write to the app's data folder can
    /// write one. Whatever stands in it, the patch a run takes up is one of this client's
    /// own, fetched from where this client's patches are published.
    #[test]
    fn of_what_a_run_wrote_down_only_the_name_of_its_patch_file_and_the_size_are_taken() {
        let made = Made::new("run-target-rebuilt");
        let record =
            |address: &str, kept_as: &str| format!("server\n9.9.999\n{address}\n{kept_as}\n1234567890\n999\n7");
        let elsewhere = "http://example.invalid/Patch/00421/00420to00421.patch";

        made.given("run-target", &record(elsewhere, "00420to00421.patch"));
        let main = Target {
            label: "1.2.421".to_owned(),
            size: 1_234_567_890,
            url: "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch".to_owned(),
            file_name: "00420to00421.patch".to_owned(),
            version: 421,
            minor: None,
        };
        assert_eq!(run_target(&made.site, &made.install), Some(main));

        made.given("run-target", &record("file:///C:/Windows/evil.patch", "00421-minor-03to04.patch"));
        let minor = run_target(&made.site, &made.install).expect("the minor patch");
        assert_eq!((minor.label.as_str(), minor.version, minor.minor), ("1.2.421 (4)", 421, Some(4)));
        assert_eq!(minor.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch");
        assert_eq!(minor.file_name, "00421-minor-03to04.patch");

        // A name that is not the one a patch file of this client is kept under names no
        // patch, wherever it would lead.
        let none_of_them = [
            r"..\..\evil.patch",
            r"..\00420to00421.patch",
            r"C:\Windows\System32\00420to00421.patch",
            r"00420to00421.patch\..\evil.exe",
            "00420to00421.patch.exe",
            "00420to00421.exe",
            "+0420to00421.patch",
            "0420to00421.patch",
            " 00420to00421.patch",
            "00421to00420.patch",
            "00421to00421.patch",
            "00421-minor-04to03.patch",
            "00421-minor-3to4.patch",
            "421-minor-03to04.patch",
            "evil.patch",
            "",
        ];
        for other in none_of_them {
            let nexons = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch";
            made.given("run-target", &record(nexons, other));
            assert_eq!(run_target(&made.site, &made.install), None, "{other:?}");
        }

        // The test world's patch is fetched from where the test world's are published.
        let test = Site::new(&mfp_kms::TEST, made.site.data.join("test"), StandIns::default());
        std::fs::write(
            records(&test, &made.install).unwrap().join("run-target"),
            record(elsewhere, "01207to01208.patch"),
        )
        .unwrap();
        let target = run_target(&test, &made.install).expect("the test world's patch");
        assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch");
        assert_eq!((target.label.as_str(), target.version), ("1.2.208", 1208));
    }

    /// What a check has proven from a folder's own files goes before what the registry
    /// says, for the registered install and for any other folder. What the registry says
    /// alone is a guess, and is not what a folder is known to be at.
    #[test]
    fn what_the_folders_files_prove_goes_before_what_the_registry_says() {
        // A copy: the registry's value is that of another folder.
        let made = Made::with("proof-before-guess", StandIns::at_version(900));
        assert_eq!(installed_version(&made.site, &made.install), Some(900));
        assert!(!version_proven(&made.site, &made.install, 900));
        made.given("proven-version", "899\n4096 1764000000");
        assert_eq!(installed_version(&made.site, &made.install), Some(899));
        assert!(version_proven(&made.site, &made.install, 899) && !version_proven(&made.site, &made.install, 900));
        // The folder's own files were looked at: the guess has not to be looked into.
        assert_eq!(unsettled(&made.site, &made.install), None);
        // A proof of the folder as it was says nothing of the folder as it is.
        made.given("proven-version", "899\n4096 1763999999");
        assert_eq!(installed_version(&made.site, &made.install), Some(900));
        assert!(!version_proven(&made.site, &made.install, 899));

        // The install the Nexon installer registered.
        let made = Made::registered("proof-before-registry", 900);
        assert!(is_registered_install(&made.site, &made.install));
        assert_eq!(install_dir(&made.site), Some(made.install.clone()));
        assert_eq!(installed_version(&made.site, &made.install), Some(900));
        assert!(!version_proven(&made.site, &made.install, 900));
        made.given("proven-version", "901\n4096 1764000000");
        assert_eq!(installed_version(&made.site, &made.install), Some(901));
        // What this app has patched the folder to since is newer than the proof.
        made.given("applied-version", "902\n4096 1764000000");
        assert_eq!(installed_version(&made.site, &made.install), Some(902));
        // Without a proof it goes as it always did: by the registry, or by this app's own
        // record when that is newer.
        std::fs::remove_file(made.record("proven-version")).unwrap();
        assert_eq!(installed_version(&made.site, &made.install), Some(902));
        made.given("applied-version", "899\n4096 1764000000");
        assert_eq!(installed_version(&made.site, &made.install), Some(900));
    }

    /// A folder that was left a mix of two versions has the mark it had, when the game's
    /// program was not among the files that changed. What was known of its version is
    /// forgotten outright.
    #[test]
    fn what_was_known_of_a_folders_version_is_forgotten_when_its_files_are_left_a_mix() {
        let made = Made::registered("forget-version", 900);
        for (record, said) in [
            ("applied-version", "901"),
            ("proven-version", "901"),
            ("minor-in-place", "901 3"),
            ("final-in-place", "901"),
        ] {
            made.given(record, &format!("{said}\n4096 1764000000"));
        }
        made.given("run-left", "00901to00902.patch\n12.50\n68270\n0\n1");
        assert!(version_proven(&made.site, &made.install, 901));
        assert_eq!(installed_version(&made.site, &made.install), Some(901));
        forget_version(&made.site, &made.install);
        assert!(!version_proven(&made.site, &made.install, 901));
        assert_eq!(known_minor(&made.site, &made.install, 901), None);
        assert!(!at_final(&made.site, &made.install, 901));
        // The registry's word is all that is left, and it is a guess.
        assert_eq!(installed_version(&made.site, &made.install), Some(900));
        // What a run left of itself is another matter, and is forgotten with its work.
        assert!(left_record(&made.site, &made.install).is_some());
    }

    #[test]
    fn a_folder_is_known_to_be_at_a_version_by_a_record_made_of_it_as_it_is() {
        let made = Made::registered("version-proven", 900);
        let proven = |version| version_proven(&made.site, &made.install, version);
        assert!(!proven(900));
        let records = [
            // This app patched it to the version, or a check proved the version, the
            // minor it is at, or that it is at the version's last minor.
            ("applied-version", "900"),
            ("proven-version", "900"),
            ("minor-in-place", "900 3"),
            ("final-in-place", "900"),
        ];
        for (record, said) in records {
            made.given(record, &format!("{said}\n4096 1764000000"));
            assert!(proven(900) && !proven(901), "{record}");
            made.given(record, &format!("{said}\n4096 1763999999"));
            assert!(!proven(900), "{record}, made of the folder as it was");
            std::fs::remove_file(made.record(record)).unwrap();
        }
    }
}
