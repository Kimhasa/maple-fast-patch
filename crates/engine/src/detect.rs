//! Works out whether a patch file on the server is made for this install, without
//! downloading all of it.
//!
//! A patch file starts with the checksums of the original files it was built from. Fetching
//! just that table and comparing it with the files on disk tells which patch fits. That is how
//! the installed minor version is found: the game records it nowhere readable.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use crate::container::PatchFile;
use crate::download;
use crate::error::{Error, Result};
use crate::plan;
use crate::stage;

/// The table sits at the very start; these sizes are tried in turn until it is complete.
const PREFIX_SIZES: [u64; 3] = [256 << 10, 2 << 20, 16 << 20];

/// Fetches the original-file checksums of a patch file on the server. `None` when the patch
/// has no such table. `scratch` is a folder for a small temporary file.
pub fn remote_originals(url: &str, scratch: &Path) -> Result<Option<BTreeMap<String, u32>>> {
    fs::create_dir_all(scratch).map_err(|e| Error::file("create", scratch, e))?;
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = scratch.join(format!("peek-{}-{serial}.tmp", std::process::id()));
    let result = peek(url, &temp);
    let _ = fs::remove_file(&temp);
    result
}

fn peek(url: &str, temp: &Path) -> Result<Option<BTreeMap<String, u32>>> {
    let mut found = None;
    for size in PREFIX_SIZES {
        let bytes = download::fetch_prefix(url, size)?;
        let whole_file = (bytes.len() as u64) < size;
        fs::write(temp, &bytes).map_err(|e| Error::file("write", temp, e))?;
        found = PatchFile::open(temp).and_then(|patch| plan::originals_of(&patch))?;
        if found.is_some() || whole_file {
            break;
        }
    }
    Ok(found)
}

/// Checksums of install files, remembered so a file is only read again after it changed.
#[derive(Default)]
pub struct Checksums {
    known: HashMap<PathBuf, (u64, SystemTime, u32)>,
}

impl Checksums {
    fn of(&mut self, path: &Path, stop: &AtomicBool) -> Result<Option<u32>> {
        let Ok(meta) = fs::metadata(path) else { return Ok(None) };
        if !meta.is_file() {
            return Ok(None);
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if let Some(&(len, when, crc)) = self.known.get(path)
            && len == meta.len()
            && when == modified
        {
            return Ok(Some(crc));
        }
        let print = stage::fingerprint(path, stop, &|_| {})?;
        self.known.insert(path.to_path_buf(), (print.len, modified, print.crc));
        Ok(Some(print.crc))
    }
}

/// Whether every original file a patch was built from is present and unchanged.
pub fn fits(
    install: &Path,
    originals: &BTreeMap<String, u32>,
    checksums: &mut Checksums,
    stop: &AtomicBool,
) -> Result<bool> {
    // Small files first: a patch that does not fit is usually given away by one of them,
    // long before the multi-gigabyte files would have to be read.
    let mut files = Vec::with_capacity(originals.len());
    for (name, expected) in originals {
        let path = install.join(plan::relative_path(name)?);
        let Ok(meta) = fs::metadata(&path) else { return Ok(false) };
        files.push((meta.len(), path, *expected));
    }
    files.sort();
    for (_, path, expected) in files {
        if checksums.of(&path, stop)? != Some(expected) {
            return Ok(false);
        }
    }
    Ok(true)
}
