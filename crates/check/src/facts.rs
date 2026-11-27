//! Everything the diagnostic found out about a PC, as plain values.
//!
//! The probes fill this in, the verdict is worked out from it, and the report is written
//! from it. Text in here is as Windows gave it: nothing in it has been made fit to show yet.
//! That happens in one place, where the report is written.

use std::path::PathBuf;

use mfp_kms::Server;

use crate::compare::Tally;
use crate::known::{Source, State};

/// Why a fact could not be established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unknown {
    /// Windows refused, with this error number.
    Windows(u32),
    /// The server refused, with this HTTP status.
    Http(u16),
    /// The server could not be reached.
    NoConnection,
    /// Nothing answered, or the answer made no sense.
    NoAnswer,
    /// It was not looked for: there was nothing to look at, or the step was left out.
    Skipped,
}

impl Unknown {
    /// That a fact could not be read, as the report says it: with the error number or the
    /// status the reading failed with, where there is one.
    pub fn label(self) -> String {
        match self {
            Unknown::Windows(code) => format!("확인 실패 (오류 {code})"),
            Unknown::Http(status) => format!("확인 실패 (HTTP {status})"),
            Unknown::NoConnection => "확인 실패 (연결 실패)".to_owned(),
            Unknown::NoAnswer | Unknown::Skipped => "확인 실패".to_owned(),
        }
    }
}

/// A fact, or why there is none.
pub type Fact<T> = Result<T, Unknown>;

/// The kind of drive, as Windows numbers them (`GetDriveTypeW`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Removable,
    Fixed,
    Remote,
    Disc,
    RamDisk,
    /// Windows did not say, or named a kind that is none of the others.
    Other,
}

impl DriveKind {
    pub fn from_windows(number: u32) -> DriveKind {
        match number {
            2 => DriveKind::Removable,
            3 => DriveKind::Fixed,
            4 => DriveKind::Remote,
            5 => DriveKind::Disc,
            6 => DriveKind::RamDisk,
            _ => DriveKind::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DriveKind::Removable => "이동식",
            DriveKind::Fixed => "고정",
            DriveKind::Remote => "네트워크",
            DriveKind::Disc => "광학",
            DriveKind::RamDisk => "램 디스크",
            DriveKind::Other => "종류 불명",
        }
    }
}

/// How a disk is attached to the PC: the number Windows gives (`STORAGE_BUS_TYPE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bus(pub u32);

impl Bus {
    pub const SCSI: Bus = Bus(1);
    pub const FIBRE_CHANNEL: Bus = Bus(6);
    pub const USB: Bus = Bus(7);
    pub const ISCSI: Bus = Bus(9);
    pub const SATA: Bus = Bus(11);
    pub const NVME: Bus = Bus(17);

    /// The name the bus goes by.
    pub fn label(self) -> String {
        let name = match self.0 {
            0 => "알 수 없음",
            1 => "SCSI",
            2 => "ATAPI",
            3 => "ATA",
            4 => "IEEE 1394",
            5 => "SSA",
            6 => "Fibre Channel",
            7 => "USB",
            8 => "RAID",
            9 => "iSCSI",
            10 => "SAS",
            11 => "SATA",
            12 => "SD",
            13 => "MMC",
            14 => "가상",
            15 => "파일 기반 가상",
            16 => "저장소 공간",
            17 => "NVMe",
            18 => "SCM",
            19 => "UFS",
            other => return format!("버스 {other}"),
        };
        name.to_owned()
    }
}

/// What Windows says about one volume and the disk it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// The drive letter with its colon, `C:`. Empty for a folder another computer shares
    /// that has no letter.
    pub letter: String,
    pub kind: DriveKind,
    pub file_system: Fact<String>,
    /// The name of the volume, as its owner gave it.
    pub label: String,
    pub read_only: bool,
    pub bus: Fact<Bus>,
    /// The maker and the model of the disk, as the disk names itself. Empty when it does not.
    pub model: String,
    /// Whether the disk has to move something to get from one place to another.
    pub seek_penalty: Fact<bool>,
    /// The size of the volume and what is free on it, in bytes.
    pub total: Fact<u64>,
    pub free: Fact<u64>,
    /// The number Windows gives the disk the volume is on. Two volumes with one number are
    /// on one disk.
    pub disk: Fact<u32>,
}

/// Who runs the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights {
    /// The user is one of the PC's administrators.
    pub administrator: bool,
    /// The program runs with an administrator's rights.
    pub elevated: bool,
    /// Windows asks before a program gets an administrator's rights. `None` when the
    /// setting could not be read.
    pub asks_first: Option<bool>,
}

/// The WebView2 runtime the app's window needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebView {
    pub version: String,
    /// Installed for every user of the PC, not just for this one.
    pub for_all: bool,
}

/// A network card that is connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    /// The model, as its driver names it.
    pub model: String,
    pub wireless: bool,
    /// Bits a second. `None` when the card does not say.
    pub speed: Option<u64>,
}

/// How the install folder was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// It was named when the program was started.
    Given,
    Registry,
    UsualPlace,
    DriveScan,
    /// The user typed it in.
    Typed,
    NotFound,
}

/// What the registry says about the game. Reported and not gone by.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Registry {
    /// The install folder Nexon's installer wrote down.
    pub install_dir: Option<String>,
    /// The number that has followed the client's version so far.
    pub version_hint: Option<u32>,
}

/// What the install folder holds, apart from the comparison of its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    /// The four numbers of the game program's file version.
    pub file_version: Fact<[u16; 4]>,
    /// The last version the game's own patcher wrote to `Patcher.log`, as digits and one
    /// underscore: `00421_04`. `None` when there is no such file or no such line.
    pub patcher_log: Option<String>,
    /// How many files the folder holds, and their sizes added up.
    pub files: Fact<(u64, u64)>,
    /// Whether this app's work folder or its lock file is in the folder.
    pub app_traces: bool,
}

/// How fast the game's drive is read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reads {
    /// How much was read in one go, in bytes: as much as came in the time a read is given.
    pub bytes: u64,
    /// Bytes a second the first time, and when the same bytes were read again.
    pub first: f64,
    pub again: f64,
    /// Bytes a second at which the bytes after those came, which were not read before.
    /// Only read when the second read was many times faster than the first, to tell a
    /// drive that keeps what it fetched from one that was held up the first time.
    pub fresh: Option<f64>,
    /// The median time of a read at a place far from the last one, in milliseconds.
    pub seek_ms: Option<f64>,
}

/// What Nexon's server has, and what was fetched from it.
#[derive(Debug, Clone, PartialEq)]
pub struct Shelf {
    /// The newest version and minor version on the server.
    pub newest: State,
    /// The patch that the folder would take next, or the newest minor patch when it needs
    /// none: its file name, its size, and how many files it names as what it starts from.
    pub patch: Option<PatchOnShelf>,
    /// Whether [`Shelf::patch`] is a main patch for this folder.
    pub patch_is_next_main: bool,
    /// Bytes fetched for the measurement of the line, and the seconds that took.
    pub sample: Fact<(u64, f64)>,
    /// The milliseconds a connection to the server takes to open: there and back once.
    /// The quickest of a few, with the looking up of the server's name left out.
    pub round_trip: Fact<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchOnShelf {
    pub file_name: String,
    pub size: u64,
    /// `None` when the patch file has no table of what it starts from.
    pub originals: Option<usize>,
}

/// What the comparison of the folder's files came to.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    /// No file was read: the files were held against the table by whether they are there
    /// and how long they are, and the state is told by that and by the version the game's
    /// own patcher logged last.
    pub by_length: bool,
    /// The state the folder is at: the newest of those it fits.
    pub at: Option<State>,
    /// The states that fit, when more than one does and a file that would tell them apart
    /// could not be read.
    pub either: Vec<State>,
    /// The state the tally below is against: the one the folder is at, or the nearest.
    pub against: Option<(State, Source)>,
    pub tally: Tally,
    /// Files in the folder that the sheet does not name.
    pub not_in_table: u64,
    /// Bytes read, and the seconds that took.
    pub bytes_read: u64,
    pub seconds: f64,
    /// It was stopped before it was done.
    pub cut_short: bool,
}

/// How a step that is only taken when asked for ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Asked<T> {
    Done(T),
    /// The user said no, or was not there to say yes.
    Declined,
    /// There was nothing to do it on.
    NotPossible,
}

/// The room the patch that is waiting needs on the game's drive, in bytes, for each of the
/// two ways the engine applies one: as `mfp_engine::run::room_needed` works it out from
/// the patch file. The diagnostic has no patch file and cannot know these; whoever has one
/// hands them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    /// The usual way, which builds every new file before it puts any in place.
    pub safe: u64,
    /// The immediate way, which puts new files in place as it goes and cannot be undone.
    pub immediate: u64,
}

/// How the write limit test ended: the step that writes as much as a main patch does.
#[derive(Debug, Clone, PartialEq)]
pub enum WriteLimit {
    Done(Landing),
    /// The user said no, or was not there to say yes.
    Declined,
    /// The volume it was to be run on has less free than the test writes and leaves:
    /// `free` is what it has (if that could be read), `writes` what the test would write,
    /// `floor` what it leaves.
    NoRoom {
        volume: String,
        free: Option<u64>,
        writes: u64,
        floor: u64,
    },
    /// The volume it was to be run on took no write, with this Windows error number, or
    /// (without one) is none of the fixed volumes of the PC that can be written to.
    NoPlace {
        volume: String,
        error: Option<i32>,
    },
    /// Less memory is available than the test wants there to be before it begins:
    /// `available` is what there is (if that could be read), `wanted` what it wants.
    NoMemory {
        available: Option<u64>,
        wanted: u64,
    },
    /// There was nothing to do it on.
    NotPossible,
}

/// The folder the diagnostic makes for its files on a volume that has no install folder.
pub const OWN_FOLDER: &str = "mfp-check-tmp";
/// What the write limit test leaves free on the drive it writes to, and on any other that
/// goes down with what it writes: it stops before a step that would leave less. Enough for
/// Windows and whatever else runs to go on writing while the test holds its files.
pub const LIMIT_FLOOR: u64 = 4 << 30;
/// What the write limit test leaves of memory, where memory goes down with what it writes:
/// it stops before a step that would leave less. A seat that keeps its writes in memory is
/// then found out without being brought down.
pub const MEMORY_FLOOR: u64 = 4 << 30;
/// What the write limit test leaves of memory where the write probe has shown that memory
/// is not where the PC keeps what it writes: what the PC needs to go on, and no more. The
/// test itself takes a few megabytes.
pub const MEMORY_OWN: u64 = 1 << 30;

/// Where on a volume the diagnostic wrote its files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// In the install folder.
    Install,
    /// In a folder of its own under the root of the volume.
    Root,
    /// In a folder of its own under the folder Windows keeps temporary files in, where
    /// the root of the volume took no folder.
    Temp,
}

/// The write probe on one volume other than the game's.
#[derive(Debug, Clone, PartialEq)]
pub struct Probed {
    /// The volume, `D:`.
    pub volume: String,
    /// What the probe found, or the Windows error number with which the volume took no
    /// write.
    pub landing: Result<Landing, Option<i32>>,
}

/// What became of the empty file that was made and removed in the install folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteTest {
    Passed,
    /// With the Windows error number.
    Failed(Option<i32>),
    /// The file was made and could not be removed again.
    LeftBehind(Option<i32>),
    Skipped,
}

/// What was free on one volume around the writing of the probe file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub letter: String,
    /// The volume the probe file was written to.
    pub written_to: bool,
    pub before: Option<u64>,
    pub after_write: Option<u64>,
    pub after_delete: Option<u64>,
    /// Once more some seconds later, as memory is. `None` when nothing was written.
    pub later: Option<u64>,
}

/// What was read off the PC after one step of a write: a part of what was to be written,
/// made safe on the drive before the next part began.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// Bytes this step wrote, and the seconds that took, flush included.
    pub bytes: u64,
    pub seconds: f64,
    /// What each watched volume had free after the step, in the order of
    /// [`Landing::volumes`].
    pub free: Vec<Option<u64>>,
    pub memory: Option<Memory>,
}

/// Why the diagnostic ended a write before all of it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Halt {
    /// The next step would have left the drive written to with less free than it was told
    /// to leave.
    Drive,
    /// Another volume than the one written to went down with what was written, and the
    /// next step would have left that one with less free than was to be left: the PC keeps
    /// what it writes there.
    Elsewhere,
    /// Memory went down with what was written, and the next step would have left less of
    /// it than was to be left: the PC keeps what it writes in memory.
    Memory,
    /// Two steps in a row were written at a small part of the speed of the steps before:
    /// whatever took the writes until then has stopped taking them.
    Slow,
    /// A step took longer than a step may.
    TimedOut,
}

/// What a write onto a volume found: where what is written shows up, and how far the
/// writing got. The write probe makes one of these out of one step of 512 MB, and the write
/// limit test one out of as many steps of 1 GB as a main patch writes.
#[derive(Debug, Clone, PartialEq)]
pub struct Landing {
    /// Where on the volume the files were written.
    pub place: Place,
    /// Bytes that were written before the writing ended.
    pub written: u64,
    /// Bytes that were to be written.
    pub wanted: u64,
    /// The seconds the writing took, flush included.
    pub seconds: f64,
    /// Why the writing ended early, as a Windows error number.
    pub error: Option<i32>,
    /// The writing was ended early by the diagnostic itself, and why.
    pub halted: Option<Halt>,
    /// Every step that was written, in order. The last may be shorter than the others.
    pub steps: Vec<Step>,
    pub volumes: Vec<Moved>,
    /// What Windows said of its memory before the write, after it, after the file was
    /// deleted, and once more some seconds later.
    pub memory: [Option<Memory>; 4],
    /// The seconds between the reading after the delete and the last one.
    pub waited: f64,
    /// The files are gone again, and with them the folder that was made for them.
    pub deleted: bool,
}

impl Landing {
    /// The volume that was written to, `D:`.
    pub fn written_to(&self) -> Option<&str> {
        self.volumes.iter().find(|volume| volume.written_to).map(|volume| volume.letter.as_str())
    }
}

/// What Windows says of its memory at one moment, in bytes.
///
/// What is available counts memory that holds parts of files Windows has read or written
/// and can hand out again at once. So a file that is written through Windows and then made
/// safe on its drive takes nothing from it, and memory that is gone from it is memory
/// something has really taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    /// What programs can still have.
    pub available: u64,
    /// What holds parts of files and of Windows itself: Windows calls it the system cache.
    pub cache: u64,
    /// What drivers and Windows hold that is never moved out of memory.
    pub nonpaged: u64,
    /// What has been promised to programs and drivers in all.
    pub commit: u64,
}

/// The names of running programs and drivers that matter, and of nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Programs {
    /// Programs that serve a seat without a disk.
    pub diskless: Vec<String>,
    /// Programs that put a disk back as it was at every start.
    pub restore: Vec<String>,
    /// Programs a PC room manages its seats with.
    pub seat: Vec<String>,
    /// The port driver of Windows' own iSCSI initiator, when it is loaded: it is how a PC
    /// reaches a disk that another computer hands it over the network.
    pub iscsi: Vec<String>,
    /// The driver of a product that hands a seat its disks over the network, when it is
    /// loaded.
    pub netdisk: Vec<String>,
    /// Storage drivers that come with Windows for adapters of servers, when they are
    /// loaded. An ordinary PC has none of these adapters and loads none of the drivers.
    pub spare: Vec<String>,
    /// Names that hold a word few other programs hold: these may be such programs.
    pub hints: Vec<String>,
    /// Names that hold a word such programs tend to hold and many others do too, and names
    /// known to be something else. They are listed and decide nothing.
    pub noted: Vec<String>,
    /// The game or its launcher, when it runs.
    pub game: Vec<String>,
}

/// Something the diagnostic wrote on this PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wrote {
    /// The empty file in the install folder, made and removed.
    EmptyFile { removed: bool },
    /// The probe file in the install folder, of this many bytes.
    ProbeFile { bytes: u64, removed: bool },
    /// The files of the write limit test in the install folder, of this many bytes in all.
    LimitFiles { bytes: u64, removed: bool },
    /// A folder of the diagnostic's own on another volume than the game's, with files of
    /// this many bytes in all: the write probe's, or (`limit`) the write limit test's.
    OwnFolder { volume: String, place: Place, bytes: u64, limit: bool, removed: bool },
    /// Files and folders an earlier run of the diagnostic left, removed now.
    Leftovers { removed: usize, left: usize },
    /// The report, saved beside the program.
    Report { file_name: String },
}

/// What was asked of Nexon's server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Traffic {
    pub requests: u32,
    pub received: u64,
}

impl Facts {
    /// Nothing found yet, at the moment `when` (seconds since 1970): every fact is one that
    /// was not looked for.
    pub fn none_yet(when: u64) -> Facts {
        Facts {
            tool_version: env!("CARGO_PKG_VERSION"),
            when,
            took: (0.0, 0.0),
            tables: [Server::Live, Server::Test].map(|server| crate::known::Table::built_in(server).newest()),
            table_given: false,
            started_from: Err(Unknown::Skipped),
            windows: String::new(),
            cpu: String::new(),
            cores: 0,
            threads: 0,
            ram: 0,
            rights: Err(Unknown::Skipped),
            webview: Err(Unknown::Skipped),
            adapters: Err(Unknown::Skipped),
            system_drive: Err(Unknown::Skipped),
            client: Server::Live,
            how: How::NotFound,
            install: None,
            registry: Registry::default(),
            game_drive: Err(Unknown::Skipped),
            room: None,
            other_volumes: Vec::new(),
            game: None,
            reads: Err(Unknown::Skipped),
            shelf: Err(Unknown::Skipped),
            sizes: Err(Unknown::Skipped),
            quick: Err(Unknown::Skipped),
            full: Asked::NotPossible,
            write_test: WriteTest::Skipped,
            landing: Asked::NotPossible,
            other_landings: Vec::new(),
            limit: WriteLimit::NotPossible,
            programs: Err(Unknown::Skipped),
            drivers_listed: Err(Unknown::Skipped),
            wrote: Vec::new(),
            traffic: Traffic::default(),
        }
    }
}

/// Everything that was found.
#[derive(Debug, Clone)]
pub struct Facts {
    /// The version of the diagnostic, and when it ran, in seconds since 1970.
    pub tool_version: &'static str,
    pub when: u64,
    /// The seconds the run took that asks nothing, and the seconds all of it took, the
    /// steps that were asked for included. Neither counts the waiting for an answer.
    pub took: (f64, f64),
    /// The newest state each table of known file states knows, for the live client and the
    /// test world.
    pub tables: [State; 2],
    /// The table for the client that was found was read from a file, and is not the one
    /// built into the program.
    pub table_given: bool,
    /// The kind of drive the program was started from.
    pub started_from: Fact<DriveKind>,
    /// The Windows edition and build, and whether it is a 64-bit Windows.
    pub windows: String,
    pub cpu: String,
    pub cores: usize,
    pub threads: usize,
    pub ram: u64,
    pub rights: Fact<Rights>,
    pub webview: Fact<Option<WebView>>,
    pub adapters: Fact<Vec<Adapter>>,
    pub system_drive: Fact<Volume>,
    pub client: Server,
    pub how: How,
    pub install: Option<PathBuf>,
    pub registry: Registry,
    pub game_drive: Fact<Volume>,
    /// What the patch that is waiting needs of the game's drive, where the caller knows.
    pub room: Option<Room>,
    /// Every other fixed volume of the PC. One of them may be where a seat keeps what it
    /// writes.
    pub other_volumes: Vec<Volume>,
    pub game: Option<Game>,
    pub reads: Fact<Reads>,
    pub shelf: Fact<Shelf>,
    /// The files held against the table by whether they are there and how long they are,
    /// with none of them read: what a run that asks nothing finds out.
    pub sizes: Fact<Comparison>,
    /// The files up to 64 MB read and held against the table by their checksums, when that
    /// was asked for.
    pub quick: Fact<Comparison>,
    pub full: Asked<Comparison>,
    pub write_test: WriteTest,
    /// The write probe in the install folder, and on every other fixed volume when it was
    /// run there too.
    pub landing: Asked<Landing>,
    pub other_landings: Vec<Probed>,
    pub limit: WriteLimit,
    pub programs: Fact<Programs>,
    pub drivers_listed: Fact<()>,
    pub wrote: Vec<Wrote>,
    pub traffic: Traffic,
}
