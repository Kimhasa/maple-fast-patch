//! What the files of a client are known to be, version by version.
//!
//! A patch file says of every file it writes what the new file's checksum and length are,
//! and of every file it starts from what its checksum was. The `known_files` program of
//! `mfp-tools` reads a row of patch files and writes that down as a table; the tables for
//! the versions the two clients are at are built into this crate. A folder is held against
//! them to tell which version and minor version it is at, without the registry and without
//! a next patch on the server.
//!
//! To write the tables anew after a patch (the live client's, from the last main patch and
//! every minor patch since, in order):
//!
//! ```text
//! cargo run --release -p mfp-tools --bin known_files -- crates\check\data\live.tsv --server live
//!     00421to00422.patch 01to02.patch 02to03.patch
//! ```
//!
//! A table is plain text. `client live` names the client. Each `state <version> <minor>`
//! line is a point the patches lead through, oldest first, where minor `0` stands for the
//! last minor of that version, whichever it was. Every other line is a file: its name and,
//! after tabs, what it is from which state on: `2:1a2b3c4d:1048576` (from the third state
//! on, this checksum and this length), `0:1a2b3c4d` (the length is not known) or `4:-`
//! (gone). Before the first state a line names, nothing is known of the file.
//!
//! A file no patch names is not in the table. Nothing is known of it, which is not the same
//! as it being wrong.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use mfp_engine::plan;
use mfp_kms::{Client, Server};

/// A point in the life of a client: a main version as the patch files count it, and a minor
/// version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct State {
    pub version: u32,
    /// Zero stands for the last minor of the version, whichever number that was: a main
    /// patch starts from there and does not say.
    pub minor: u32,
}

impl State {
    /// The state as it is shown, the way Nexon's notices write a version:
    /// `Ver 1.2.421 마이너버전(4)`, `Ver 1.2.421` for the first minor, and
    /// `Ver 1.2.420 (마지막 마이너)`.
    pub fn label(&self, client: &Client) -> String {
        let version = format!("Ver {}", client.version_label(self.version));
        match self.minor {
            0 => format!("{version} (마지막 마이너)"),
            1 => version,
            minor => format!("{version} 마이너버전({minor})"),
        }
    }

    /// The state a line of the game's own patcher names: `00421_04` is minor 4 of version
    /// 421.
    pub fn logged(log: &str) -> Option<State> {
        let (version, minor) = log.split_once('_')?;
        Some(State { version: version.parse().ok()?, minor: minor.parse().ok()? })
    }

    /// Where the state stands in time among the states of one client. The last minor of a
    /// version comes after every minor that has a number.
    pub fn order(&self) -> (u32, u32) {
        (self.version, if self.minor == 0 { u32::MAX } else { self.minor })
    }
}

/// What a file is expected to be at a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// The file is there, with this checksum, and with this length where a patch said it.
    File { crc: u32, len: Option<u64> },
    /// The file was deleted.
    Gone,
}

/// Where what a [`Sheet`] says comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The table built into this program.
    Table,
    /// The start of a patch file on Nexon's server.
    Server,
}

/// Everything that is known of the files of a client at one state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sheet {
    pub state: State,
    pub source: Source,
    /// The files by the form of their names that names are compared in (see [`key`]), each
    /// with its name as a patch writes it.
    pub files: BTreeMap<String, (String, Expect)>,
}

impl Sheet {
    /// A sheet of what a patch file starts from: the checksums in the table at its start.
    pub fn from_originals(state: State, originals: &BTreeMap<String, u32>) -> Sheet {
        let files = originals
            .iter()
            .map(|(name, crc)| (key(name), (name.replace('/', "\\"), Expect::File { crc: *crc, len: None })))
            .collect();
        Sheet { state, source: Source::Server, files }
    }

    /// How many files the sheet expects to be there.
    pub fn present(&self) -> usize {
        self.files.values().filter(|(_, expect)| matches!(expect, Expect::File { .. })).count()
    }
}

/// A name in the form names are compared in: Windows takes `Data\A.wz` and `data/a.wz` for
/// one file.
pub fn key(name: &str) -> String {
    plan::name_key(name)
}

/// One line of a table: a file and what it is from which state on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    name: String,
    /// The number of a state, and what the file is from there on. In order.
    changes: Vec<(usize, Expect)>,
}

/// A table of known file states of one client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub server: Server,
    /// The states the table knows, oldest first.
    pub states: Vec<State>,
    rows: Vec<Row>,
}

const LIVE: &str = include_str!("../data/live.tsv");
const TEST: &str = include_str!("../data/test.tsv");

impl Table {
    /// The table built into this program for the client of `server`.
    pub fn built_in(server: Server) -> &'static Table {
        static TABLES: OnceLock<[Table; 2]> = OnceLock::new();
        let tables = TABLES.get_or_init(|| {
            // Both are checked by a test, so a table that cannot be read never ships.
            [LIVE, TEST].map(|text| Table::parse(text).expect("a built-in table cannot be read"))
        });
        match server {
            Server::Live => &tables[0],
            Server::Test => &tables[1],
        }
    }

    /// Reads a table as the `known_files` program writes it.
    pub fn parse(text: &str) -> Result<Table, String> {
        let mut server = None;
        let mut states = Vec::new();
        let mut rows = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let wrong = |what: &str| format!("line {}: {what}", number + 1);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix("client ") {
                server = Some(Server::from_key(name).ok_or_else(|| wrong("a client that is not known"))?);
                continue;
            }
            if let Some(state) = line.strip_prefix("state ") {
                let numbers = state.split_once(' ').and_then(|(version, minor)| {
                    Some(State { version: version.parse().ok()?, minor: minor.parse().ok()? })
                });
                states.push(numbers.ok_or_else(|| wrong("a state without its two numbers"))?);
                continue;
            }
            let mut fields = line.split('\t');
            let name = fields.next().unwrap_or_default();
            // The names decide which files are read, so they are held to what a patch file
            // may name.
            plan::relative_path(name).map_err(|_| wrong("a name that leaves the folder"))?;
            let mut changes = Vec::new();
            for field in fields {
                let mut parts = field.split(':');
                let state: usize =
                    parts.next().and_then(|state| state.parse().ok()).ok_or_else(|| wrong("no state number"))?;
                let expect = match (parts.next(), parts.next()) {
                    (Some("-"), None) => Expect::Gone,
                    (Some(crc), len) => Expect::File {
                        crc: u32::from_str_radix(crc, 16).map_err(|_| wrong("a checksum that is no number"))?,
                        len: match len {
                            Some(len) => Some(len.parse().map_err(|_| wrong("a length that is no number"))?),
                            None => None,
                        },
                    },
                    (None, _) => return Err(wrong("a state without what the file is there")),
                };
                if state >= states.len() || changes.last().is_some_and(|(last, _)| *last >= state) {
                    return Err(wrong("states out of order"));
                }
                changes.push((state, expect));
            }
            if changes.is_empty() {
                return Err(wrong("a file of which nothing is said"));
            }
            rows.push(Row { name: name.to_owned(), changes });
        }
        let server = server.ok_or("no client is named")?;
        if states.is_empty() {
            return Err("no state is named".to_owned());
        }
        Ok(Table { server, states, rows })
    }

    /// The newest state the table knows.
    pub fn newest(&self) -> State {
        *self.states.last().expect("a table has a state")
    }

    /// What the file `name` is at the state numbered `state`, or `None` when the table does
    /// not say.
    pub fn at(&self, name: &str, state: usize) -> Option<Expect> {
        let wanted = key(name);
        let row = self.rows.iter().find(|row| key(&row.name) == wanted)?;
        expect_at(row, state)
    }

    /// One sheet for each state of the table, oldest first.
    pub fn sheets(&self) -> Vec<Sheet> {
        let sheet = |number: usize| {
            let files =
                self.rows.iter().filter_map(|row| Some((key(&row.name), (row.name.clone(), expect_at(row, number)?))));
            Sheet { state: self.states[number], source: Source::Table, files: files.collect() }
        };
        (0..self.states.len()).map(sheet).collect()
    }
}

/// What a row says of its file at the state numbered `state`.
fn expect_at(row: &Row, state: usize) -> Option<Expect> {
    row.changes.iter().rev().find(|(from, _)| *from <= state).map(|(_, expect)| *expect)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: &str = "# a comment\nclient live\nstate 900 0\nstate 901 1\nstate 901 2\n\
        Data\\Base.wz\t0:0000000a\n\
        Data\\Map.wz\t0:0000000b\t1:0000000c:300\t2:0000000d:400\n\
        Data\\New.wz\t1:0000000e:50\n\
        Data\\Old.wz\t0:0000000f\t2:-\n";

    #[test]
    fn table_says_what_a_file_is_at_each_state() {
        let table = Table::parse(SMALL).unwrap();
        assert_eq!(table.server, Server::Live);
        assert_eq!(table.states.len(), 3);
        assert_eq!(table.newest(), State { version: 901, minor: 2 });
        let file = |crc: u32, len: Option<u64>| Some(Expect::File { crc, len });
        // Never written by a patch: the same at every state, and its length is not known.
        assert_eq!([0, 1, 2].map(|state| table.at(r"Data\Base.wz", state)), [file(0xa, None); 3]);
        // Written twice.
        assert_eq!(table.at(r"Data\Map.wz", 0), file(0xb, None));
        assert_eq!(table.at(r"Data\Map.wz", 1), file(0xc, Some(300)));
        assert_eq!(table.at(r"Data\Map.wz", 2), file(0xd, Some(400)));
        // Nothing is known before the patch that made it, and a deleted file is gone.
        assert_eq!(table.at(r"Data\New.wz", 0), None);
        assert_eq!(table.at(r"Data\New.wz", 2), file(0xe, Some(50)));
        assert_eq!(table.at(r"Data\Old.wz", 1), file(0xf, None));
        assert_eq!(table.at(r"Data\Old.wz", 2), Some(Expect::Gone));
        // Names are compared the way Windows compares them, and a file no patch names is
        // not known.
        assert_eq!(table.at("data/MAP.WZ", 1), file(0xc, Some(300)));
        assert_eq!(table.at(r"Data\Other.wz", 2), None);
    }

    #[test]
    fn each_state_has_a_sheet_of_what_is_known_there() {
        let sheets = Table::parse(SMALL).unwrap().sheets();
        assert_eq!(sheets.iter().map(|sheet| sheet.files.len()).collect::<Vec<_>>(), [3, 4, 4]);
        assert_eq!(sheets.iter().map(Sheet::present).collect::<Vec<_>>(), [3, 4, 3]);
        assert!(sheets.iter().all(|sheet| sheet.source == Source::Table));
        assert_eq!(
            sheets[1].files[r"data\new.wz"],
            (r"Data\New.wz".to_owned(), Expect::File { crc: 0xe, len: Some(50) })
        );
    }

    #[test]
    fn table_that_is_not_one_is_refused() {
        for (text, why) in [
            ("state 900 0\nData\\A.wz\t0:0000000a\n", "no client"),
            ("client live\nData\\A.wz\t0:0000000a\n", "no state"),
            ("client other\nstate 900 0\n", "an unknown client"),
            ("client live\nstate 900\n", "half a state"),
            ("client live\nstate 900 0\nData\\A.wz\n", "a file of which nothing is said"),
            ("client live\nstate 900 0\nData\\A.wz\t1:0000000a\n", "a state the table does not have"),
            ("client live\nstate 900 0\nstate 901 1\nData\\A.wz\t1:0000000a\t0:0000000b\n", "states out of order"),
            ("client live\nstate 900 0\nData\\A.wz\t0:xyz\n", "a checksum that is no number"),
            ("client live\nstate 900 0\nData\\A.wz\t0:0000000a:big\n", "a length that is no number"),
            ("client live\nstate 900 0\n..\\Windows\\a.dll\t0:0000000a\n", "a name that leaves the folder"),
        ] {
            assert!(Table::parse(text).is_err(), "{why} was accepted");
        }
    }

    #[test]
    fn built_in_tables_can_be_read_and_know_the_versions_they_were_made_for() {
        let live = Table::built_in(Server::Live);
        assert_eq!(live.server, Server::Live);
        assert!(live.newest().order() >= (421, 4), "{:?}", live.newest());
        let test = Table::built_in(Server::Test);
        assert_eq!(test.server, Server::Test);
        assert!(test.newest().order() >= (1208, 2), "{:?}", test.newest());
        for table in [live, test] {
            // The first state is where a main patch starts from, and the states follow in order.
            assert_eq!(table.states[0].minor, 0);
            assert!(table.states.windows(2).all(|pair| pair[0].order() < pair[1].order()));
            // The newest sheet names the game's own program and more than a thousand files.
            let newest = table.sheets().pop().unwrap();
            assert!(newest.present() > 1000, "{}", newest.present());
            assert!(newest.files.contains_key(&key(table.server.client().game_program)));
        }
    }

    #[test]
    fn states_are_shown_as_nexon_writes_versions() {
        let (live, test) = (&mfp_kms::LIVE, &mfp_kms::TEST);
        assert_eq!(State { version: 421, minor: 4 }.label(live), "Ver 1.2.421 마이너버전(4)");
        assert_eq!(State { version: 421, minor: 1 }.label(live), "Ver 1.2.421");
        assert_eq!(State { version: 420, minor: 0 }.label(live), "Ver 1.2.420 (마지막 마이너)");
        assert_eq!(State { version: 1208, minor: 2 }.label(test), "Ver 1.2.208 마이너버전(2)");
        // The last minor of a version comes after its numbered ones and before the next version.
        let order = |version, minor| State { version, minor }.order();
        assert!(order(420, 3) < order(420, 0) && order(420, 0) < order(421, 1));
    }

    #[test]
    fn sheet_of_what_a_patch_starts_from_has_checksums_and_no_lengths() {
        let originals: BTreeMap<String, u32> = [("Data/Map.wz".to_owned(), 7), ("MapleStory.exe".to_owned(), 9)].into();
        let sheet = Sheet::from_originals(State { version: 419, minor: 0 }, &originals);
        assert_eq!(sheet.source, Source::Server);
        assert_eq!(sheet.files[r"data\map.wz"], (r"Data\Map.wz".to_owned(), Expect::File { crc: 7, len: None }));
        assert_eq!(sheet.present(), 2);
    }
}
