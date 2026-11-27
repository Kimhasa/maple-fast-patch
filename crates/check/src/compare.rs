//! Holds the files of a folder against what they are known to be, to tell which version the
//! folder is at and which of its files are not what Nexon published.
//!
//! What is known comes as [`Sheet`]s, one for each state of the client: from the table built
//! into this program, and from the patch files on Nexon's server. A folder is at a state
//! when every file the sheet of that state names is in the folder as the sheet has it.
//!
//! Reading every file takes long on a slow drive, so there are three passes. The first
//! reads nothing, and goes by which files are there and how long they are. The quick one
//! reads the small files and goes by length for the large ones, where a sheet knows the
//! length. The full one reads everything. Nothing is ever written.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use mfp_engine::{Error, crc, plan};

use crate::known::{Expect, Sheet, State};

/// The quick pass reads files up to this length.
pub const QUICK_LIMIT: u64 = 64 << 20;

/// What was found of one file in the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// No file of that name is there.
    Missing,
    /// The file is there. `crc` is its checksum once it was read.
    Present { len: u64, crc: Option<u32> },
    /// The file is there and could not be read. `code` is the Windows error number.
    Unreadable { len: u64, code: Option<i32> },
}

/// How a folder stands against one sheet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    /// Files that were read and have the checksum the sheet names.
    pub same: usize,
    /// Files that were not read and have the length the sheet names.
    pub same_length: usize,
    /// Files that were not read, of which the sheet names no length to go by.
    pub unread: usize,
    /// Files that are not what the sheet names: another checksum, another length, or there
    /// when the sheet says the file is gone. By name.
    pub differs: Vec<String>,
    /// Files the sheet names that are not there. By name.
    pub missing: Vec<String>,
    /// Files that could not be read, with the Windows error number. By name.
    pub unreadable: Vec<(String, Option<i32>)>,
}

impl Tally {
    /// Whether nothing speaks against the folder being at the sheet's state, and something
    /// speaks for it.
    pub fn fits(&self) -> bool {
        self.differs.is_empty() && self.missing.is_empty() && self.same > 0
    }

    /// How many files speak against it.
    pub fn against(&self) -> usize {
        self.differs.len() + self.missing.len()
    }

    /// As [`Tally::fits`], where no file was read: nothing speaks against the state, and
    /// the length of at least one file speaks for it.
    pub fn fits_by_length(&self) -> bool {
        self.differs.is_empty() && self.missing.is_empty() && self.same + self.same_length > 0
    }
}

/// Holds what was seen of a folder against one sheet. A file the sheet names that was never
/// looked at counts as not read.
pub fn tally(sheet: &Sheet, seen: &BTreeMap<String, Seen>) -> Tally {
    let mut tally = Tally::default();
    for (key, (name, expect)) in &sheet.files {
        match (expect, seen.get(key)) {
            (Expect::Gone, Some(Seen::Present { .. } | Seen::Unreadable { .. })) => tally.differs.push(name.clone()),
            (Expect::Gone, _) => {}
            (Expect::File { .. }, Some(Seen::Missing)) => tally.missing.push(name.clone()),
            (
                Expect::File { len: Some(wanted), .. },
                Some(Seen::Present { len, .. } | Seen::Unreadable { len, .. }),
            ) if wanted != len => {
                tally.differs.push(name.clone());
            }
            (Expect::File { .. }, Some(Seen::Unreadable { code, .. })) => tally.unreadable.push((name.clone(), *code)),
            (Expect::File { crc: wanted, .. }, Some(Seen::Present { crc: Some(crc), .. })) => {
                if wanted == crc {
                    tally.same += 1;
                } else {
                    tally.differs.push(name.clone());
                }
            }
            (Expect::File { len: Some(_), .. }, Some(Seen::Present { crc: None, .. })) => tally.same_length += 1,
            (Expect::File { len: None, .. }, Some(Seen::Present { crc: None, .. }) | None) => tally.unread += 1,
            (Expect::File { len: Some(_), .. }, None) => tally.unread += 1,
        }
    }
    tally
}

/// How a folder stands against every sheet there is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    /// One tally for each sheet, in the order of the sheets.
    pub tallies: Vec<Tally>,
    /// The sheets the folder fits, by their place among the sheets, with each state once.
    pub fits: Vec<usize>,
    /// The sheet the folder comes nearest to: the one it fits, or else the one with the most
    /// files that are right, then with the fewest that are wrong, and of several the newest.
    /// `None` without sheets.
    pub nearest: Option<usize>,
}

impl Standing {
    /// The state the folder is at: the newest of those it fits.
    ///
    /// More than one fits where a later state is only known to hold more than an earlier
    /// one, or the same. A folder that holds everything a state is known to hold is at
    /// that state, which is how the app tells too. This is only the answer once the files
    /// that could tell the fitting states apart were read: see [`tells_apart`].
    pub fn settled(&self, sheets: &[Sheet]) -> Option<State> {
        self.fits.iter().map(|at| sheets[*at].state).max_by_key(State::order)
    }
}

/// Holds what was seen of a folder against every sheet. Sheets that name no file say
/// nothing and fit nothing.
pub fn standing(sheets: &[Sheet], seen: &BTreeMap<String, Seen>) -> Standing {
    standing_as(sheets, seen, false)
}

/// As [`standing`], for a folder none of whose files was read: a file that is as long as
/// a sheet says counts as right. Sheets that name no length fit nothing this way.
///
/// This tells less than checksums do. A file that was changed and kept its length passes,
/// and two states whose files are equally long cannot be told apart.
pub fn standing_by_length(sheets: &[Sheet], seen: &BTreeMap<String, Seen>) -> Standing {
    standing_as(sheets, seen, true)
}

fn standing_as(sheets: &[Sheet], seen: &BTreeMap<String, Seen>, by_length: bool) -> Standing {
    let tallies: Vec<Tally> = sheets.iter().map(|sheet| tally(sheet, seen)).collect();
    // The files that are right, as this pass counts them.
    let right = |tally: &Tally| if by_length { tally.same + tally.same_length } else { tally.same };
    let mut fits: Vec<usize> = Vec::new();
    for (at, tally) in tallies.iter().enumerate() {
        let fitting = if by_length { tally.fits_by_length() } else { tally.fits() };
        // Two sheets of one state say the same thing; the first one stands for both.
        if fitting && !fits.iter().any(|other| sheets[*other].state == sheets[at].state) {
            fits.push(at);
        }
    }
    let said_something = |at: &usize| right(&tallies[*at]) + tallies[*at].against() > 0;
    let nearest = match fits[..] {
        [_, ..] => fits.iter().copied().max_by_key(|at| sheets[*at].state.order()),
        // The most files that are right, then the fewest that are wrong. Counting only what
        // is wrong would favour a sheet that names few files.
        [] => (0..sheets.len()).filter(said_something).min_by_key(|at| {
            let tally = &tallies[*at];
            (std::cmp::Reverse(right(tally)), tally.against(), std::cmp::Reverse(sheets[*at].state.order()))
        }),
    };
    Standing { tallies, fits, nearest }
}

/// A file that would tell the sheets at `among` apart: one that is in the folder, was not
/// read yet, and that the sheets expect to be different things, or that one of them knows
/// and another does not. The shortest such file.
pub fn tells_apart<'a>(sheets: &'a [Sheet], among: &[usize], seen: &BTreeMap<String, Seen>) -> Option<&'a str> {
    let [first, rest @ ..] = among else { return None };
    // The checksum a sheet expects of a file, or nothing where it expects none.
    let expected = |sheet: usize, key: &str| match sheets[sheet].files.get(key) {
        Some((_, Expect::File { crc, .. })) => Some(*crc),
        _ => None,
    };
    let mut best: Option<(u64, &str)> = None;
    for sheet in among {
        for key in sheets[*sheet].files.keys() {
            let Some(Seen::Present { len, crc: None }) = seen.get(key) else { continue };
            let differ = rest.iter().any(|other| expected(*other, key) != expected(*first, key));
            if differ && best.is_none_or(|(shortest, _)| *len < shortest) {
                best = Some((*len, key));
            }
        }
    }
    best.map(|(_, key)| key)
}

/// Reads a file from end to end and takes its checksum, as the engine does. `on_bytes`
/// hears how many more bytes were read, after each piece. `None` when `stop` was set
/// meanwhile: it is looked at before every piece, so a long file is left within a moment.
fn checksum(path: &Path, stop: &AtomicBool, on_bytes: &mut dyn FnMut(u64)) -> std::io::Result<Option<(u64, u32)>> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 1 << 20];
    let (mut len, mut sum) = (0u64, 0u32);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(Some((len, sum)));
        }
        sum = crc::update(sum, &buffer[..count]);
        len += count as u64;
        on_bytes(count as u64);
    }
}

/// What was seen of a folder so far, and how much reading that took.
#[derive(Debug, Clone, Default)]
pub struct Folder {
    pub seen: BTreeMap<String, Seen>,
    /// The names of the files that were looked at, as a patch writes them, by their keys.
    names: BTreeMap<String, String>,
    /// Bytes read for checksums, and the seconds that took.
    pub bytes_read: u64,
    pub seconds: f64,
}

impl Folder {
    /// Looks at every file the sheets name, without reading any: whether it is there, and
    /// how long it is.
    pub fn look(install: &Path, sheets: &[Sheet]) -> Folder {
        let mut folder = Folder::default();
        folder.look_at_more(install, sheets);
        folder
    }

    /// Looks at the files `sheets` name that were not looked at before. What is known of
    /// the others stays.
    pub fn look_at_more(&mut self, install: &Path, sheets: &[Sheet]) {
        for sheet in sheets {
            for (key, (name, _)) in &sheet.files {
                if self.seen.contains_key(key) {
                    continue;
                }
                // A name that could lead out of the folder is never followed.
                let Ok(relative) = plan::relative_path(name) else { continue };
                let found = match std::fs::metadata(install.join(relative)) {
                    Ok(meta) if meta.is_file() => Seen::Present { len: meta.len(), crc: None },
                    _ => Seen::Missing,
                };
                self.seen.insert(key.clone(), found);
                self.names.insert(key.clone(), name.clone());
            }
        }
    }

    /// The files that are there and not read yet, no longer than `limit`: their keys and
    /// lengths, shortest first.
    pub fn unread(&self, limit: u64) -> Vec<(String, u64)> {
        let mut files: Vec<(String, u64)> = self
            .seen
            .iter()
            .filter_map(|(key, seen)| match seen {
                Seen::Present { len, crc: None } if *len <= limit => Some((key.clone(), *len)),
                _ => None,
            })
            .collect();
        files.sort_by_key(|(_, len)| *len);
        files
    }

    /// Reads one file for its checksum. `on_bytes` hears how many more bytes of it were
    /// read, piece after piece. `Err` only when `stop` was set meanwhile; a file that
    /// cannot be read is noted as that.
    pub fn read(
        &mut self,
        install: &Path,
        key: &str,
        stop: &AtomicBool,
        on_bytes: &mut dyn FnMut(u64),
    ) -> Result<(), Error> {
        let Some(Seen::Present { len, crc: None }) = self.seen.get(key).copied() else { return Ok(()) };
        let Some(Ok(relative)) = self.names.get(key).map(|name| plan::relative_path(name)) else { return Ok(()) };
        let started = Instant::now();
        let found = match checksum(&install.join(relative), stop, on_bytes) {
            Ok(Some((read, sum))) => {
                self.bytes_read += read;
                Seen::Present { len: read, crc: Some(sum) }
            }
            Ok(None) => return Err(Error::Stopped),
            Err(err) => Seen::Unreadable { len, code: err.raw_os_error() },
        };
        self.seconds += started.elapsed().as_secs_f64();
        self.seen.insert(key.to_owned(), found);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::known::{Source, Table};

    /// A table of three states. `Small.ini` tells every state from the next, `Big.wz` only
    /// the first from the others, and `Base.wz` is never written.
    const TABLE: &str = "client live\nstate 900 0\nstate 901 1\nstate 901 2\n\
        Data\\Base.wz\t0:0000000a\n\
        Data\\Big.wz\t0:000000b0\t1:000000b1:5000\n\
        Small.ini\t0:000000c0\t1:000000c1:21\t2:000000c2:21\n\
        Data\\Old.wz\t0:0000000f\t2:-\n";

    fn sheets() -> Vec<Sheet> {
        Table::parse(TABLE).unwrap().sheets()
    }

    fn seen(files: &[(&str, Seen)]) -> BTreeMap<String, Seen> {
        files.iter().map(|(name, seen)| (crate::known::key(name), *seen)).collect()
    }

    fn read(len: u64, crc: u32) -> Seen {
        Seen::Present { len, crc: Some(crc) }
    }

    fn unread(len: u64) -> Seen {
        Seen::Present { len, crc: None }
    }

    #[test]
    fn folder_that_holds_what_one_state_names_is_at_that_state() {
        let sheets = sheets();
        let at_newest = seen(&[
            (r"Data\Base.wz", read(100, 0xa)),
            (r"Data\Big.wz", read(5000, 0xb1)),
            ("Small.ini", read(21, 0xc2)),
            (r"Data\Old.wz", Seen::Missing),
        ]);
        let standing = standing(&sheets, &at_newest);
        assert_eq!(standing.fits, [2]);
        assert_eq!(standing.settled(&sheets), Some(State { version: 901, minor: 2 }));
        assert_eq!(standing.nearest, Some(2));
        assert_eq!((standing.tallies[2].same, standing.tallies[2].against()), (3, 0));
        // Against the state before, the small file differs and the deleted one is missing.
        assert_eq!(standing.tallies[1].differs, ["Small.ini"]);
        assert_eq!(standing.tallies[1].missing, [r"Data\Old.wz"]);
    }

    #[test]
    fn folder_that_was_not_read_is_told_by_what_is_there_and_how_long_it_is() {
        let sheets = sheets();
        // The lengths the two newer states name, and nothing read.
        let looked = seen(&[
            (r"Data\Base.wz", unread(100)),
            (r"Data\Big.wz", unread(5000)),
            ("Small.ini", unread(21)),
            (r"Data\Old.wz", Seen::Missing),
        ]);
        // By checksum nothing fits, for nothing was read.
        assert!(standing(&sheets, &looked).fits.is_empty());
        // By length the newest state fits: two files are as long as it says, and the file
        // it says is gone is gone. The first state names no length, and so fits nothing.
        let by_length = standing_by_length(&sheets, &looked);
        assert_eq!(by_length.fits, [2]);
        assert_eq!(by_length.nearest, Some(2));
        assert_eq!(
            (by_length.tallies[2].same, by_length.tallies[2].same_length, by_length.tallies[2].unread),
            (0, 2, 1)
        );
        // The state before it is spoken against by the file that should still be there.
        assert_eq!(by_length.tallies[1].missing, [r"Data\Old.wz"]);
        // With the old file there it is the state before that fits. Which of two states
        // with the same lengths a folder is at, lengths cannot say.
        let kept = seen(&[
            (r"Data\Base.wz", unread(100)),
            (r"Data\Big.wz", unread(5000)),
            ("Small.ini", unread(21)),
            (r"Data\Old.wz", unread(7)),
        ]);
        let by_length = standing_by_length(&sheets, &kept);
        assert_eq!(by_length.fits, [1]);
        // A file of another length speaks against every state that names its length.
        let wrong = seen(&[
            (r"Data\Base.wz", unread(100)),
            (r"Data\Big.wz", unread(4999)),
            ("Small.ini", unread(21)),
            (r"Data\Old.wz", Seen::Missing),
        ]);
        let by_length = standing_by_length(&sheets, &wrong);
        assert!(by_length.fits.is_empty());
        assert_eq!(by_length.nearest, Some(2));
        assert_eq!(by_length.tallies[2].differs, [r"Data\Big.wz"]);
    }

    #[test]
    fn file_that_should_be_gone_and_is_there_speaks_against_a_state() {
        let sheets = sheets();
        let kept_the_old_file = seen(&[
            (r"Data\Base.wz", read(100, 0xa)),
            (r"Data\Big.wz", read(5000, 0xb1)),
            ("Small.ini", read(21, 0xc2)),
            (r"Data\Old.wz", read(7, 0xf)),
        ]);
        let standing = standing(&sheets, &kept_the_old_file);
        assert!(standing.fits.is_empty());
        assert_eq!(standing.settled(&sheets), None);
        assert_eq!(standing.tallies[2].differs, [r"Data\Old.wz"]);
        // One file speaks against the newest state and one against the one before; the
        // newest of the two is the nearest.
        assert_eq!(standing.nearest, Some(2));
    }

    #[test]
    fn length_alone_speaks_against_a_state_and_never_for_it() {
        let sheets = sheets();
        // Nothing was read. The large file has the length the two newer states name.
        let looked = seen(&[
            (r"Data\Base.wz", unread(100)),
            (r"Data\Big.wz", unread(5000)),
            ("Small.ini", unread(21)),
            (r"Data\Old.wz", unread(7)),
        ]);
        let standing_now = standing(&sheets, &looked);
        assert!(standing_now.fits.is_empty(), "a folder of which nothing was read fits no state");
        assert_eq!(standing_now.tallies[1].same_length, 2);
        assert_eq!(standing_now.tallies[0].unread, 4);
        // Another length is enough to say the file is not the one a state names.
        let longer = seen(&[(r"Data\Big.wz", unread(5001)), ("Small.ini", read(21, 0xc1))]);
        let standing_then = standing(&sheets, &longer);
        assert_eq!(standing_then.tallies[1].differs, [r"Data\Big.wz"]);
        assert!(standing_then.fits.is_empty());
    }

    #[test]
    fn states_that_cannot_be_told_apart_yet_name_the_file_that_would() {
        let sheets = sheets();
        // The small file was not read: the two newer states both fit what was.
        let looked = seen(&[
            (r"Data\Base.wz", read(100, 0xa)),
            (r"Data\Big.wz", unread(5000)),
            ("Small.ini", unread(21)),
            (r"Data\Old.wz", Seen::Missing),
        ]);
        let standing_now = standing(&sheets, &looked);
        // Only the newest fits as it is: the one before expects the deleted file.
        assert_eq!(standing_now.fits, [2]);
        // Between the first two states, the small file tells them apart and is the shortest.
        assert_eq!(tells_apart(&sheets, &[0, 1], &looked), Some("small.ini"));
        assert_eq!(tells_apart(&sheets, &[1, 2], &looked), Some("small.ini"));
        // Once it is read, nothing is left to read between the two newer states.
        let mut then = looked.clone();
        then.insert("small.ini".to_owned(), read(21, 0xc2));
        assert_eq!(tells_apart(&sheets, &[1, 2], &then), None);
        assert_eq!(tells_apart(&sheets, &[2], &looked), None);
        assert_eq!(tells_apart(&sheets, &[], &looked), None);
    }

    #[test]
    fn folder_that_fits_two_states_is_at_the_newer_once_nothing_is_left_to_tell_them_apart() {
        // The second state is only known to hold one file more than the first.
        const TWO: &str =
            "client live\nstate 901 1\nstate 901 2\nData\\A.wz\t0:0000000a:10\nData\\New.wz\t1:0000000b:20\n";
        let sheets = Table::parse(TWO).unwrap().sheets();
        let looked = seen(&[(r"Data\A.wz", read(10, 0xa)), (r"Data\New.wz", unread(20))]);
        let standing_now = standing(&sheets, &looked);
        assert_eq!(standing_now.fits, [0, 1]);
        // The file only the newer state knows has not been read: that is what is read next.
        assert_eq!(tells_apart(&sheets, &standing_now.fits, &looked), Some(r"data\new.wz"));
        // Read, and what the newer state says: the folder is at the newer state.
        let both = seen(&[(r"Data\A.wz", read(10, 0xa)), (r"Data\New.wz", read(20, 0xb))]);
        let standing_then = standing(&sheets, &both);
        assert_eq!(standing_then.fits, [0, 1]);
        assert_eq!(tells_apart(&sheets, &standing_then.fits, &both), None);
        assert_eq!(standing_then.settled(&sheets), Some(State { version: 901, minor: 2 }));
        assert_eq!(standing_then.nearest, Some(1));
        // Read, and something else: only the older state fits.
        let other = seen(&[(r"Data\A.wz", read(10, 0xa)), (r"Data\New.wz", read(20, 0xc))]);
        assert_eq!(standing(&sheets, &other).settled(&sheets), Some(State { version: 901, minor: 1 }));
    }

    #[test]
    fn two_sheets_of_one_state_count_once_and_an_empty_sheet_fits_nothing() {
        let mut sheets = sheets();
        let again = Sheet { source: Source::Server, ..sheets[2].clone() };
        sheets.push(again);
        sheets.push(Sheet { state: State { version: 902, minor: 1 }, source: Source::Server, files: BTreeMap::new() });
        let at_newest = seen(&[
            (r"Data\Base.wz", read(100, 0xa)),
            (r"Data\Big.wz", read(5000, 0xb1)),
            ("Small.ini", read(21, 0xc2)),
            (r"Data\Old.wz", Seen::Missing),
        ]);
        let standing = standing(&sheets, &at_newest);
        assert_eq!(standing.fits, [2]);
        assert!(!standing.tallies[4].fits());
    }

    #[test]
    fn folder_that_fits_nothing_is_nearest_to_the_state_the_fewest_files_speak_against() {
        let sheets = sheets();
        let one_wrong = seen(&[
            (r"Data\Base.wz", read(100, 0xdead)),
            (r"Data\Big.wz", read(5000, 0xb1)),
            ("Small.ini", read(21, 0xc1)),
            (r"Data\Old.wz", read(7, 0xf)),
        ]);
        let standing = standing(&sheets, &one_wrong);
        assert!(standing.fits.is_empty());
        assert_eq!(standing.nearest, Some(1));
        assert_eq!(standing.tallies[1].differs, [r"Data\Base.wz"]);
        // Without sheets, and with nothing seen, there is nothing to be near to.
        assert_eq!(super::standing(&[], &one_wrong).nearest, None);
        assert_eq!(super::standing(&sheets, &BTreeMap::new()).nearest, None);
    }

    #[test]
    fn file_that_cannot_be_read_is_neither_right_nor_wrong() {
        let sheets = sheets();
        let locked = seen(&[
            (r"Data\Base.wz", Seen::Unreadable { len: 100, code: Some(32) }),
            (r"Data\Big.wz", read(5000, 0xb1)),
            ("Small.ini", read(21, 0xc2)),
        ]);
        let tally = tally(&sheets[2], &locked);
        assert_eq!(tally.unreadable, [(r"Data\Base.wz".to_owned(), Some(32))]);
        assert!(tally.fits());
    }

    #[test]
    fn files_of_a_folder_are_looked_at_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path();
        std::fs::create_dir(install.join("Data")).unwrap();
        std::fs::write(install.join("Data").join("Base.wz"), b"base").unwrap();
        std::fs::write(install.join("Small.ini"), b"small file").unwrap();
        let sheets = sheets();
        let stop = AtomicBool::new(false);

        let mut folder = Folder::look(install, &sheets);
        assert_eq!(folder.seen[r"data\base.wz"], unread(4));
        assert_eq!(folder.seen[r"data\big.wz"], Seen::Missing);
        assert_eq!(folder.unread(u64::MAX), [(r"data\base.wz".to_owned(), 4), ("small.ini".to_owned(), 10)]);
        assert_eq!(folder.unread(5), [(r"data\base.wz".to_owned(), 4)]);

        let mut heard = 0u64;
        folder.read(install, "small.ini", &stop, &mut |bytes| heard += bytes).unwrap();
        let crc = mfp_engine::crc::update(0, b"small file");
        assert_eq!(folder.seen["small.ini"], read(10, crc));
        assert_eq!((folder.bytes_read, heard), (10, 10));
        // A file that is not there, or was read already, is left as it is.
        folder.read(install, r"data\big.wz", &stop, &mut |bytes| heard += bytes).unwrap();
        folder.read(install, "small.ini", &stop, &mut |bytes| heard += bytes).unwrap();
        assert_eq!((folder.bytes_read, heard), (10, 10));
        // A stop is passed on, and the file stays unread.
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(matches!(folder.read(install, r"data\base.wz", &stop, &mut |_| {}), Err(Error::Stopped)));
        assert_eq!(folder.seen[r"data\base.wz"], unread(4));
    }
}
