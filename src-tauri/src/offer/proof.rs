//! The proof of the version an install folder is at, from the folder's own files: for a
//! folder that nothing on this PC speaks for, and for one that only the registry speaks for
//! when that is not enough to go by.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use mfp_engine::{Error, detect};

use super::{MAINS_AHEAD, REACH, Shelf, full_label, newest_main, newest_minor};
use crate::records::{install_mark, latest_seen, record_final, record_minor, record_proven, see_latest};
use crate::state::{Site, Unproven};

/// What looking for a proof of a folder's version came to.
pub(super) enum Proof {
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
pub(super) fn prove(site: &Site, install: &Path, shelf: &dyn Shelf, hint: Option<u32>) -> Result<Proof, Error> {
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
