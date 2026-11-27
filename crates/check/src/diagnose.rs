//! Runs the diagnostic, as calls that an app or a console program makes one after the
//! other, and hands back everything that was found.
//!
//! A diagnosis has two parts. The first, [`Diagnosis::start`], asks nothing and reads
//! little: what the PC and its volumes are, which programs run, which version the game says
//! it is, whether its files are there and as long as they should be, a short sample of how
//! fast the drive reads, and what Nexon's server has. It is done in about ten seconds on
//! the slowest PC it is meant for, a seat that reads its game over the network, and each
//! part of it that is timed has a ceiling. The second part is the steps that read much or
//! write, [`Extra`]. Each is run by itself with [`Diagnosis::run`], and before it is, a
//! [`Diagnosis::offer`] says what it costs on this PC and the words to ask for it with.
//!
//! Nothing here prints, reads a console, or keeps anything outside the values it hands
//! back: a [`Diagnosis`] can be kept, cloned and passed between threads. What a call is
//! doing is told as numbers to a callback, and every call ends early when its stop flag is
//! set, leaving nothing of what it wrote.
//!
//! The console program is [`run`]: it finds the install folder, asks the user through a
//! [`Desk`] with the words of the offers, and makes the same calls.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use mfp_kms::{Client, Server};

use crate::compare::{self, Folder, QUICK_LIMIT, Standing};
use crate::discover::{self, Places};
use crate::facts::{
    Asked, Comparison, DriveKind, Fact, Facts, How, PatchOnShelf, Place, Probed, Registry, Room, Shelf, Traffic,
    Unknown, Volume, WriteLimit, WriteTest, Wrote,
};
use crate::known::{Sheet, State, Table};
use crate::privacy::Scrub;
use crate::probe::game::Walked;
use crate::probe::write::Fill;
use crate::probe::{game, programs, read, system, volume, write};
use crate::report::Head;
use crate::server::{self, Nexon};
use crate::units::{count, gb, size, speed, took};
use crate::verdict::{Evidence, ROOM_NEEDED, Verdict, verdict};

/// The longest the questions to Nexon's server are given in the part of a diagnosis that
/// asks nothing. What is not answered by then is not waited for.
pub const SERVER_TIME: Duration = Duration::from_secs(6);

/// How the steps that write are sized. The figures they are meant to be run with are the
/// default ones; others are for trying the steps out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sizes {
    /// How much the write probe writes on each volume.
    pub probe_bytes: u64,
    /// How long after the files of a write are removed memory is looked at once more.
    pub settle: Duration,
    /// Running the write probe on every other fixed volume of the PC too, and not only in
    /// the install folder.
    pub probe_other_volumes: bool,
    /// How much the write limit test writes, and how much to a file.
    pub limit_bytes: u64,
    pub limit_step: u64,
    /// What the write limit test leaves of memory; see [`write::MEMORY_FLOOR`].
    pub limit_memory_floor: u64,
}

impl Default for Sizes {
    fn default() -> Sizes {
        Sizes {
            probe_bytes: write::PROBE_BYTES,
            settle: write::SETTLE,
            probe_other_volumes: true,
            limit_bytes: ROOM_NEEDED,
            limit_step: write::LIMIT_STEP,
            limit_memory_floor: write::MEMORY_FLOOR,
        }
    }
}

/// What a diagnosis is run on, as whoever starts it knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The install folder. Nothing is looked for: not where the registry says, not in the
    /// usual place, not on the drives. Without one, only the PC is looked at.
    pub install: Option<PathBuf>,
    /// Which client the folder holds.
    pub client: Server,
    /// How the folder was found, which the report says.
    pub how: How,
    /// The caller knows the game to be running. Nothing is read from the game's files
    /// then: no read sample, and neither of the comparisons that read.
    pub game_running: bool,
    /// Read from the game's drive also when the diagnostic itself finds the game running.
    /// Without this, that is treated as [`Request::game_running`] is.
    pub reads_while_running: bool,
    /// Making and removing an empty file in the install folder, to see that it takes
    /// writes.
    pub write_test: bool,
    /// What the caller's own test of that came to. With this, no empty file is made.
    pub writable: Option<WriteTest>,
    /// Asking Nexon's patch server what it has: which version is the newest, which patch
    /// comes next, and what its patches start from. Without this, a diagnosis asks the
    /// server nothing but what [`Request::time_the_line`] asks for.
    pub online: bool,
    /// What the caller knows the server to have. With this, the part of a diagnosis that
    /// asks nothing does not ask the server what it has either; a comparison that reads
    /// may still ask it what its patches start from, where `online` allows.
    pub shelf: Option<OnShelf>,
    /// Timing the line to Nexon's server also where the caller says what the server has
    /// ([`Request::shelf`]): a piece of the newest main patch is fetched and connections
    /// to its server are opened, and nothing else is asked of it. Whether `online` is set
    /// makes no difference to this. Without a shelf handed in it changes nothing: a
    /// diagnosis that asks the server what it has times the line as it does so.
    pub time_the_line: bool,
    /// A table of known file states to go by in place of the one built into the program,
    /// for the client it is of: one made after a patch that came out since.
    pub table: Option<Table>,
    /// What the patch that is waiting needs of the game's drive, as the engine works it
    /// out from the patch file; see [`Diagnosis::with_room`].
    pub room: Option<Room>,
    /// Which program the diagnosis is made for: the first line of its report names that
    /// program and its version.
    pub head: Head,
    pub sizes: Sizes,
}

/// What the caller knows Nexon's server to have, in place of asking it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnShelf {
    /// The newest version and minor version on the server. The verdict goes by this: it
    /// tells a folder that is one minor patch behind, and files that fit no known version
    /// because they are of a newer one from files that are not Nexon's.
    pub newest: State,
    /// The patch the folder would take next, and whether that is a main patch. The report
    /// names it, and nothing goes by it.
    pub patch: Option<PatchOnShelf>,
    pub patch_is_next_main: bool,
}

impl Request {
    /// A diagnosis of the install folder of `client` at `install`, as an app asks for it:
    /// everything at the figures it is meant to be run with.
    pub fn of(install: &Path, client: Server) -> Request {
        Request {
            install: Some(install.to_path_buf()),
            client,
            how: How::Given,
            game_running: false,
            reads_while_running: false,
            write_test: true,
            writable: None,
            online: true,
            shelf: None,
            time_the_line: false,
            table: None,
            room: None,
            head: Head::Check,
            sizes: Sizes::default(),
        }
    }
}

/// A step that reads much or writes, and is only run when asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Extra {
    /// Reading the game's files up to 64 MB and holding their checksums against the table.
    Checksums,
    /// Reading every file of the game that is not read yet.
    Full,
    /// The write probe: a gigabyte onto every fixed volume, in four files, and off again.
    WriteProbe,
    /// The write limit test: as much as a main patch writes, onto one volume, and off
    /// again.
    WriteLimit,
}

impl Extra {
    /// Every step, in the order they are offered in.
    pub const ALL: [Extra; 4] = [Extra::Checksums, Extra::Full, Extra::WriteProbe, Extra::WriteLimit];

    /// What the step is called.
    pub fn label(self) -> &'static str {
        match self {
            Extra::Checksums => "파일 내용 비교 (일부)",
            Extra::Full => "파일 내용 비교 (전체)",
            Extra::WriteProbe => "쓰기 검사",
            Extra::WriteLimit => "한도 검사",
        }
    }
}

/// What a call is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// The parts of the diagnosis that asks nothing, in their order.
    Pc,
    Install,
    Sizes,
    Server,
    Reads,
    /// Reading files for their checksums: the small ones, or all that are left.
    Checksums,
    Full,
    /// Asking Nexon's server what its patches start from, in the middle of a comparison.
    Tables,
    /// Writing the files of the write probe, and of the write limit test.
    WriteProbe,
    WriteLimit,
}

impl Stage {
    /// What is being done, in the words the console shows.
    pub fn label(self) -> &'static str {
        match self {
            Stage::Pc => "PC 확인 중",
            Stage::Install => "설치 폴더 확인 중",
            Stage::Server => "넥슨 패치 서버 확인 중",
            Stage::Sizes => "파일 크기 비교 중",
            Stage::Reads => "읽기 속도 측정 중",
            Stage::Checksums => "파일 내용 비교 (일부) 중",
            Stage::Full => "파일 내용 비교 (전체) 중",
            Stage::Tables => "넥슨 패치 서버의 원본 표 확인 중",
            Stage::WriteProbe => "쓰기 검사 중",
            Stage::WriteLimit => "한도 검사 중",
        }
    }
}

/// How far a call is: `done` of `total`, which never goes back within one call.
///
/// For a step that reads or writes these are bytes. For the diagnosis that asks nothing
/// they are its parts, five in all. The write probe goes from volume to volume: `part`
/// counts them from nought, the install folder first, in the order of [`Offer::volumes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub stage: Stage,
    pub done: u64,
    pub total: u64,
    pub part: u32,
}

/// Counts what a call has done and tells whoever watches.
struct Meter<'a> {
    now: Progress,
    tell: &'a mut dyn FnMut(Progress),
}

impl<'a> Meter<'a> {
    fn new(stage: Stage, total: u64, tell: &'a mut dyn FnMut(Progress)) -> Meter<'a> {
        Meter { now: Progress { stage, done: 0, total, part: 0 }, tell }
    }

    fn say(&mut self) {
        (self.tell)(self.now);
    }

    /// Goes on to another stage, with what is done as it is.
    fn stage(&mut self, stage: Stage) {
        self.now.stage = stage;
        self.say();
    }

    /// There is this much more to do than was known.
    fn expect(&mut self, more: u64) {
        self.now.total += more;
    }

    /// This much more is done.
    fn add(&mut self, more: u64) {
        self.reach(self.now.done + more);
    }

    /// This much is done in all. Never less than before, and never more than there is.
    fn reach(&mut self, done: u64) {
        self.now.done = self.now.done.max(done);
        self.now.total = self.now.total.max(self.now.done);
        self.say();
    }

    /// All of it is done.
    fn finish(&mut self) {
        self.reach(self.now.total);
    }
}

/// Nexon's server as a thread of its own asks it: every request is counted where whoever
/// waits can see it, and none is begun once nobody waits any more.
struct Counted<'a> {
    shelf: &'a Nexon,
    asked: &'a Mutex<Traffic>,
    given_up: &'a AtomicBool,
}

impl Counted<'_> {
    fn ask<T>(&self, ask: impl FnOnce(&Nexon) -> Result<T, Unknown>) -> Result<T, Unknown> {
        if self.given_up.load(Ordering::Relaxed) {
            return Err(Unknown::NoAnswer);
        }
        // Counted before it is made: a request that never comes back was made all the same.
        self.asked.lock().map(|mut asked| asked.requests += 1).ok();
        let answer = ask(self.shelf);
        self.asked.lock().map(|mut asked| asked.received = self.shelf.traffic().received).ok();
        answer
    }
}

impl server::Shelf for Counted<'_> {
    fn size_of(&self, url: &str) -> Result<Option<u64>, Unknown> {
        self.ask(|shelf| shelf.size_of(url))
    }

    fn start_of(&self, url: &str, len: u64) -> Result<Option<Vec<u8>>, Unknown> {
        self.ask(|shelf| shelf.start_of(url, len))
    }

    /// The connections that are opened to time the way to the server ask it for nothing.
    /// They go to it all the same, and are counted as one more thing that was asked.
    fn round_trip(&self, url: &str) -> Result<f64, Unknown> {
        self.ask(|shelf| shelf.round_trip(url))
    }
}

/// Asks Nexon's server on a thread of its own, so that whoever waits for the answer can
/// stop waiting: when `stop` is set, and when `within` is over. `None` then. What was
/// asked is added to `traffic` either way.
///
/// A request cannot be taken back once it is made. The thread that made it ends by itself
/// when the request does, at the latest when the connection gives up, and asks nothing
/// more after that.
fn ask_server<T: Send + 'static>(
    within: Option<Duration>,
    stop: &AtomicBool,
    traffic: &mut Traffic,
    job: impl FnOnce(&dyn server::Shelf) -> T + Send + 'static,
) -> Option<T> {
    let asked = Arc::new(Mutex::new(Traffic::default()));
    let given_up = Arc::new(AtomicBool::new(false));
    let (answer, waiting) = mpsc::channel();
    let (theirs, their_flag) = (Arc::clone(&asked), Arc::clone(&given_up));
    let asking = std::thread::Builder::new().name("mfp-check-server".to_owned()).spawn(move || {
        let nexon = Nexon::new();
        let counted = Counted { shelf: &nexon, asked: &theirs, given_up: &their_flag };
        // Nobody may be waiting any more: then the answer goes nowhere.
        let _ = answer.send(job(&counted));
    });
    let until = within.map(|within| Instant::now() + within);
    let mut found = None;
    while asking.is_ok() {
        match waiting.recv_timeout(Duration::from_millis(50)) {
            Ok(answer) => {
                found = Some(answer);
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) || until.is_some_and(|until| Instant::now() >= until) {
                    break;
                }
            }
        }
    }
    given_up.store(true, Ordering::Relaxed);
    if let Ok(asked) = asked.lock() {
        traffic.requests += asked.requests;
        traffic.received += asked.received;
    }
    found
}

/// A volume a step writes to, or can be run on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `D:`. Nothing for an install folder that another computer shares.
    pub letter: String,
    /// The game's drive, where the step writes into the install folder.
    pub game: bool,
    /// What the volume has free.
    pub free: Option<u64>,
    /// Whether that is enough for the write limit test: what it writes and what it leaves.
    pub room: bool,
}

/// Why a step is not run.
#[derive(Debug, Clone, PartialEq)]
pub enum Refused {
    /// There is no install folder.
    NoInstall,
    /// The game runs, and nothing is read from under it.
    GameRunning,
    /// Every file there is to read was read already.
    NothingToRead,
    /// The write limit test has no room, no memory, or no such volume to be run on: one
    /// of [`WriteLimit::NoRoom`], [`WriteLimit::NoMemory`] and [`WriteLimit::NoPlace`].
    Limit(Box<WriteLimit>),
}

/// What a step would cost on this PC, and the words to ask for it with. The console asks
/// with [`Offer::question`]; a window shows `what`, `cost` and `warning`, which are the
/// parts that question is made of.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub step: Extra,
    /// Why the step cannot be run as things are, and that in words fit to show. `None`
    /// and empty for a step that can be run.
    pub refused: Option<Refused>,
    pub reason: String,
    /// How many files the step reads, and how many bytes it reads or writes in all.
    pub files: u64,
    pub bytes: u64,
    /// About how long that takes at the speed the drive was read at. For a step that
    /// writes this is a guess, and `cost` says it as one. `None` where the drive was not
    /// timed.
    pub seconds: Option<f64>,
    /// The step writes onto the PC.
    pub writes: bool,
    /// The volumes the write probe writes to, the install folder first; for the write
    /// limit test, the volumes it can be run on, the game's drive first.
    pub volumes: Vec<Target>,
    /// What the step does, in one line.
    pub what: String,
    /// What it costs, in one line. Empty where the drive was not timed.
    pub cost: String,
    /// What can come of it, or what is done to the PC besides: for the steps that write,
    /// and for the steps that read while the game runs. One line each.
    pub warning: Vec<String>,
    /// The whole question, as the console asks it: the three above and 진행하시겠습니까?
    pub question: String,
}

/// What a diagnosis leaves for the steps that are asked for to go on from.
#[derive(Debug, Clone)]
struct Work {
    install: PathBuf,
    client: &'static Client,
    walked: Walked,
    /// The newest state the table that is gone by knows.
    known_up_to: State,
    sheets: Vec<Sheet>,
    folder: Folder,
    newest: Fact<State>,
    /// The game or its launcher runs, by these names, fit to show.
    running: Vec<String>,
    /// Nothing says that the game's files are not to be read.
    reads_allowed: bool,
}

/// What a diagnosis has found so far: the facts, the verdict they come to, and the report
/// of them. Cheap to clone, and safe to hand to another thread.
#[derive(Debug, Clone)]
pub struct Diagnosis {
    facts: Arc<Facts>,
    scrub: Arc<Scrub>,
    request: Arc<Request>,
    work: Option<Arc<Work>>,
    stopped: bool,
}

impl Diagnosis {
    /// Runs the part of a diagnosis that asks nothing: no file of the game is read but
    /// for a short sample of one, and nothing is written but one empty file that is
    /// removed at once. `on_progress` hears which of its five parts it is at.
    ///
    /// Setting `stop` ends it at the next part, with what was found until then; a request
    /// to the server that is under way is waited for.
    pub fn start(request: &Request, stop: &AtomicBool, on_progress: &mut dyn FnMut(Progress)) -> Diagnosis {
        let started = Instant::now();
        let scrub = system::hidden();
        let mut facts = Facts::none_yet(system::now());
        let mut meter = Meter::new(Stage::Pc, 5, on_progress);
        let work = unasked(request, stop, &mut facts, &scrub, &mut meter);
        let stopped = stop.load(Ordering::Relaxed);
        if !stopped {
            meter.finish();
        }
        let seconds = started.elapsed().as_secs_f64();
        facts.took = (seconds, seconds);
        Diagnosis {
            facts: Arc::new(facts),
            scrub: Arc::new(scrub),
            request: Arc::new(request.clone()),
            work: work.map(Arc::new),
            stopped,
        }
    }

    /// Everything that was found, as plain values.
    pub fn facts(&self) -> &Facts {
        &self.facts
    }

    /// What takes the names of the PC and of its user out of a text.
    pub fn scrub(&self) -> &Scrub {
        &self.scrub
    }

    /// The verdict the facts come to: what kind of PC it is, whether a main patch is
    /// expected to go through, and whether it stays, each with the facts it rests on.
    pub fn verdict(&self) -> Verdict {
        verdict(&Evidence::of(&self.facts, &self.scrub))
    }

    /// The report as text: what the console prints, with nothing in it that names the PC
    /// or its user. Its first line names the program the request said it is made for.
    pub fn report(&self) -> String {
        crate::report::render(&self.facts, &self.scrub, &self.request.head)
    }

    /// The same diagnosis with what the patch that is waiting needs of the game's drive,
    /// as `mfp_engine::run::room_needed` says once the patch file is on this PC. Nothing
    /// is looked at again. The answer about the patch in everyday words goes by these
    /// figures from then on: without them it cannot say whether the immediate way fits.
    pub fn with_room(&self, room: Option<Room>) -> Diagnosis {
        let mut next = self.clone();
        Arc::make_mut(&mut next.facts).room = room;
        next
    }

    /// Whether the call that made this value was ended by its stop flag before it was
    /// done. What it holds is then what was found until then.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Whether anything the diagnostic wrote could not be removed again. The report names
    /// it; the next [`Diagnosis::start`] on the same folder removes it.
    pub fn left_behind(&self) -> bool {
        self.facts.wrote.iter().any(|wrote| match wrote {
            Wrote::EmptyFile { removed }
            | Wrote::ProbeFile { removed, .. }
            | Wrote::LimitFiles { removed, .. }
            | Wrote::OwnFolder { removed, .. } => !removed,
            Wrote::Leftovers { left, .. } => *left > 0,
            Wrote::Report { .. } => false,
        })
    }

    /// Whether a step was run to where it has something to show.
    pub fn ran(&self, step: Extra) -> bool {
        match step {
            Extra::Checksums => self.facts.quick.is_ok(),
            Extra::Full => matches!(self.facts.full, Asked::Done(_)),
            Extra::WriteProbe => matches!(self.facts.landing, Asked::Done(_)),
            Extra::WriteLimit => matches!(self.facts.limit, WriteLimit::Done(_)),
        }
    }

    /// The facts and the scrub by themselves, for whoever wants to add to the facts.
    pub fn into_parts(self) -> (Facts, Scrub) {
        (Arc::unwrap_or_clone(self.facts), Arc::unwrap_or_clone(self.scrub))
    }

    /// How fast the game's drive was read, in bytes a second, when it was timed.
    fn read_speed(&self) -> Option<f64> {
        self.facts.reads.as_ref().ok().map(|reads| reads.first)
    }

    /// What a step would cost on this PC as things are now, and the words to ask for it
    /// with. Reads what the volumes have free and what memory is available, and nothing
    /// else. `on` is the volume the write limit test is to be run on, `D:`; `None` is the
    /// game's drive. The other steps take no notice of it.
    pub fn offer(&self, step: Extra, on: Option<&str>) -> Offer {
        let read_speed = self.read_speed();
        let mut offer = Offer {
            step,
            refused: None,
            reason: String::new(),
            files: 0,
            bytes: 0,
            seconds: None,
            writes: matches!(step, Extra::WriteProbe | Extra::WriteLimit),
            volumes: Vec::new(),
            what: String::new(),
            cost: String::new(),
            warning: Vec::new(),
            question: String::new(),
        };
        let Some(work) = &self.work else {
            (offer.refused, offer.reason) = (Some(Refused::NoInstall), "설치 폴더를 찾지 못함".to_owned());
            return offer;
        };
        let sizes = &self.request.sizes;
        match step {
            Extra::Checksums | Extra::Full => {
                let limit = if step == Extra::Checksums { QUICK_LIMIT } else { u64::MAX };
                let files = work.folder.unread(limit);
                (offer.files, offer.bytes) = (files.len() as u64, files.iter().map(|(_, len)| len).sum());
                offer.what = if step == Extra::Checksums {
                    format!(
                        "파일 내용 비교 (일부): {} 이하 게임 파일 {}개({})를 읽어 넥슨 패치의 체크섬과 비교합니다.",
                        size(QUICK_LIMIT),
                        count(offer.files),
                        size(offer.bytes),
                    )
                } else {
                    format!(
                        "파일 내용 비교 (전체): 아직 읽지 않은 게임 파일 {}개({})를 모두 읽습니다.",
                        count(offer.files),
                        size(offer.bytes)
                    )
                };
                // Said in the questions that read, so that nobody reads gigabytes from
                // under a game that is being played without knowing it.
                if !work.running.is_empty() {
                    offer.warning.push(format!("{} 실행 중입니다.", work.running.join(", ")));
                }
                if !work.reads_allowed {
                    offer.refused = Some(Refused::GameRunning);
                    offer.reason = "게임 실행 중에는 게임 파일을 읽지 않음".to_owned();
                } else if files.is_empty() {
                    (offer.refused, offer.reason) = (Some(Refused::NothingToRead), "읽지 않은 파일 없음".to_owned());
                }
            }
            Extra::WriteProbe => {
                let plan = Fill::probe(sizes.probe_bytes, sizes.settle);
                offer.volumes = probe_targets(&self.facts, &work.install, sizes.probe_other_volumes);
                let others: Vec<&str> = offer.volumes[1..].iter().map(|volume| volume.letter.as_str()).collect();
                offer.bytes = plan.total.saturating_mul(offer.volumes.len() as u64);
                let each =
                    format!("{} 임시 파일 {}개({})", size(plan.step), plan.total.div_ceil(plan.step), size(plan.total));
                let noted = "썼다가 지우고, 볼륨별 여유 공간과 메모리의 변화를 기록합니다.";
                if others.is_empty() {
                    offer.what = format!("쓰기 검사: 설치 폴더에 {each}를 {noted}");
                } else {
                    offer.what = format!(
                        "쓰기 검사: 고정 볼륨 {}곳(설치 폴더, {})에 각각 {each}, 모두 {}를 {noted}",
                        offer.volumes.len(),
                        others.join(", "),
                        size(offer.bytes),
                    );
                    offer
                        .warning
                        .push(format!("설치 폴더 밖에서는 볼륨에 {} 폴더를 만들어 쓰고 지웁니다.", write::OWN_FOLDER));
                }
            }
            Extra::WriteLimit => {
                let volumes = limit_targets(&self.facts, &work.install, sizes);
                // What the write probe wrote at on the volume in question, where it was
                // run there: that is how fast this test will write, not how fast is read.
                let line = line_of(&self.facts);
                let probed = match target_named(&volumes, on) {
                    Ok(at) if volumes[at].game => match &self.facts.landing {
                        Asked::Done(landing) => Some(landing),
                        _ => None,
                    },
                    Ok(at) => self
                        .facts
                        .other_landings
                        .iter()
                        .find(|probed| probed.volume == volumes[at].letter)
                        .and_then(|probed| probed.landing.as_ref().ok()),
                    Err(_) => None,
                };
                let speeds =
                    Speeds { read: read_speed, write: probed.and_then(|landing| settled_speed(landing, line)) };
                // And where the probe showed the writes going is what the test has to
                // leave of memory.
                let plan = limit_plan(sizes, &self.facts, probed);
                return limit_offer(volumes, on, sizes, speeds, no_memory_for(&plan), &self.scrub);
            }
        }
        (offer.seconds, offer.cost) = about(offer.bytes, read_speed, offer.writes);
        offer.question = question(&offer);
        offer
    }

    /// Runs a step and hands back the diagnosis with what it found added. A step that
    /// [`Diagnosis::offer`] refuses is not run: the diagnosis comes back as it was, but
    /// for the write limit test, of which the report then says why it was refused. `on`
    /// is as for the offer.
    ///
    /// `on_progress` hears how many bytes of how many are read or written. Setting `stop`
    /// ends the step within about a second: a file that is being read is left, and files
    /// that were written are removed. What was found until then is kept, and the report
    /// says of it that it was cut short.
    ///
    /// Two steps must not be run at once on one install folder, and none while a patch
    /// is being applied to it.
    pub fn run(
        &self,
        step: Extra,
        on: Option<&str>,
        stop: &AtomicBool,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Diagnosis {
        let offer = self.offer(step, on);
        let mut next = self.clone();
        next.stopped = stop.load(Ordering::Relaxed);
        let Some(work) = &self.work else { return next };
        if next.stopped {
            return next;
        }
        let started = Instant::now();
        let mut facts = (*self.facts).clone();
        let mut work = (**work).clone();
        let sizes = &self.request.sizes;
        match (step, offer.refused) {
            (Extra::WriteLimit, Some(Refused::Limit(not_run))) => facts.limit = *not_run,
            (_, Some(_)) => return next,
            (Extra::Checksums | Extra::Full, None) => {
                let stage = if step == Extra::Checksums { Stage::Checksums } else { Stage::Full };
                let mut meter = Meter::new(stage, 0, on_progress);
                let limit = if step == Extra::Checksums { QUICK_LIMIT } else { u64::MAX };
                let found = work.read(limit, self.request.online, &mut facts.traffic, stop, &mut meter);
                if step == Extra::Full {
                    facts.full = Asked::Done(found);
                } else {
                    // The files said more than their lengths did: the patch that comes
                    // next is looked up again.
                    let moved_on = facts.sizes.as_ref().is_ok_and(|sizes| sizes.at != found.at);
                    let online = self.request.online && self.request.shelf.is_none();
                    if let (true, true, Ok(newest)) = (moved_on, online, work.newest) {
                        let (client, at) = (work.client, found.at);
                        let looked =
                            ask_server(None, stop, &mut facts.traffic, move |server| shelf(client, newest, at, server));
                        if let Some(looked) = looked {
                            facts.shelf = Ok(looked);
                        }
                    }
                    // Nothing was too long to read: this was the full comparison too.
                    if work.folder.unread(u64::MAX).is_empty() && !found.cut_short {
                        facts.full = Asked::Done(found.clone());
                    }
                    facts.quick = Ok(found);
                }
            }
            (Extra::WriteProbe, None) => {
                let mut meter = Meter::new(Stage::WriteProbe, offer.bytes, on_progress);
                write_probe(&offer.volumes, &work.install, sizes, &mut facts, stop, &mut meter);
            }
            (Extra::WriteLimit, None) => {
                let mut meter = Meter::new(Stage::WriteLimit, offer.bytes, on_progress);
                let target = target_named(&offer.volumes, on).map(|at| &offer.volumes[at]);
                facts.limit = match target {
                    Ok(target) => {
                        let probed = match &facts.landing {
                            Asked::Done(landing) if target.game => Some(landing),
                            _ => facts
                                .other_landings
                                .iter()
                                .find(|probed| !target.game && probed.volume == target.letter)
                                .and_then(|probed| probed.landing.as_ref().ok()),
                        };
                        let plan = limit_plan(sizes, &facts, probed);
                        write_limit(target, &work.install, plan, stop, &mut meter)
                    }
                    Err(not_there) => *not_there,
                };
                if let WriteLimit::Done(landing) = &facts.limit {
                    match landing.place {
                        Place::Install if landing.written > 0 => {
                            facts.wrote.push(Wrote::LimitFiles { bytes: landing.written, removed: landing.deleted });
                        }
                        Place::Install => {}
                        place => facts.wrote.push(Wrote::OwnFolder {
                            volume: landing.written_to().unwrap_or_default().to_owned(),
                            place,
                            bytes: landing.written,
                            limit: true,
                            removed: landing.deleted,
                        }),
                    }
                }
            }
        }
        facts.took.1 += started.elapsed().as_secs_f64();
        next.stopped = stop.load(Ordering::Relaxed);
        next.facts = Arc::new(facts);
        next.work = Some(Arc::new(work));
        next
    }
}

/// The offer of the write limit test on the one of `volumes` that `on` names; see
/// [`Diagnosis::offer`]. `no_memory` is why memory does not allow it, where it does not.
fn limit_offer(
    volumes: Vec<Target>,
    on: Option<&str>,
    sizes: &Sizes,
    speeds: Speeds,
    no_memory: Option<WriteLimit>,
    scrub: &Scrub,
) -> Offer {
    let target = target_named(&volumes, on);
    let named = match target {
        Ok(at) if volumes[at].game => "설치 폴더".to_owned(),
        Ok(at) => format!("{} 볼륨", volumes[at].letter),
        Err(_) => on.unwrap_or_default().to_owned(),
    };
    let (what, warning) = limit_words(&named, sizes);
    let (seconds, cost) = speeds.cost_of_writing(sizes.limit_bytes);
    // Too little memory to tell, in time, whether the writing takes it; a volume that is
    // none of them; a volume without the room.
    let not_run = no_memory.or_else(|| match target {
        Ok(at) if volumes[at].room => None,
        Ok(at) => Some(no_room_on(&volumes[at], sizes)),
        Err(not_there) => Some(*not_there),
    });
    let reason = not_run.as_ref().and_then(|not_run| crate::report::limit_not_run(not_run, scrub)).unwrap_or_default();
    let mut offer = Offer {
        step: Extra::WriteLimit,
        refused: not_run.map(|not_run| Refused::Limit(Box::new(not_run))),
        reason,
        files: 0,
        bytes: sizes.limit_bytes,
        seconds,
        writes: true,
        volumes,
        what,
        cost,
        warning,
        question: String::new(),
    };
    offer.question = question(&offer);
    offer
}

/// The question a step is asked for with, made of the parts of its offer.
fn question(offer: &Offer) -> String {
    let after = |text: &str| if text.is_empty() { String::new() } else { format!(" {text}") };
    let (what, cost, warning) = (&offer.what, after(&offer.cost), &offer.warning);
    let body = match offer.step {
        // What is done to the PC besides comes before what it costs.
        Extra::WriteProbe => format!("{what}{}{cost}", after(&warning.join(" "))),
        // What can come of it has a line of its own.
        Extra::WriteLimit => format!("{what}{cost}\n{}", warning.join("\n")),
        Extra::Checksums | Extra::Full => format!("{what}{cost}{}", after(&warning.join(" "))),
    };
    format!("{body} 진행하시겠습니까?")
}

/// What a step costs on this PC: about how many seconds `bytes` take at the speed the
/// drive was just read at, and that as it is said in the question that asks for the step.
/// For a step that writes (`written`) it is a guess, and is said as one. Nothing where
/// that speed was not measured.
fn about(bytes: u64, read_speed: Option<f64>, written: bool) -> (Option<f64>, String) {
    let Some(read) = read_speed.filter(|read| *read > 0.0) else { return (None, String::new()) };
    let seconds = bytes as f64 / read;
    let how = if written { "로 쓴다면" } else { "로" };
    (Some(seconds), format!("이 PC의 읽기 속도({}){how} 약 {}.", speed(read), took(seconds)))
}

/// How fast this PC reads and writes, in bytes a second, as far as that was measured:
/// what the game's drive was read at, and what the write probe wrote at once its steps
/// had settled.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Speeds {
    read: Option<f64>,
    write: Option<f64>,
}

impl Speeds {
    /// About how long writing `bytes` takes, and that as the question says it: by the
    /// speed that was written at where that is known, and else, as a guess, by the speed
    /// that was read at.
    fn cost_of_writing(self, bytes: u64) -> (Option<f64>, String) {
        match self.write.filter(|write| *write > 0.0) {
            Some(write) => {
                let seconds = bytes as f64 / write;
                (Some(seconds), format!("이 PC의 쓰기 속도({})로 약 {}.", speed(write), took(seconds)))
            }
            None => about(bytes, self.read, true),
        }
    }
}

/// What the wired line of the PC carries, in bits a second.
fn line_of(facts: &Facts) -> Option<u64> {
    // The cards are listed with the cabled ones first, the fastest of them first.
    let wired = facts.adapters.as_ref().ok().and_then(|cards| cards.iter().find(|card| !card.wireless));
    wired.and_then(|card| card.speed)
}

/// The speed a write settled at: what its steps were written at once they were at the
/// speed of the line, where they got there, and else what all of it was written at.
fn settled_speed(landing: &crate::facts::Landing, line: Option<u64>) -> Option<f64> {
    match crate::verdict::went(landing, line) {
        crate::verdict::Went::Line { speed, .. } => Some(speed),
        _ if landing.written > 0 && landing.seconds > 0.0 => Some(landing.written as f64 / landing.seconds),
        _ => None,
    }
}

/// What the write limit test does, in one line, and what can come of it, where it is run
/// on `named`: `설치 폴더`, or `D: 볼륨`.
fn limit_words(named: &str, sizes: &Sizes) -> (String, Vec<String>) {
    let step = if sizes.limit_step == 1 << 30 { "1 GB".to_owned() } else { size(sizes.limit_step) };
    let what = format!(
        "한도 검사: {named}에 본 패치만큼({}) 임시 파일을 {step}씩 실제로 썼다가 모두 지웁니다.",
        size(sizes.limit_bytes)
    );
    let risk = "PC방 좌석에서는 쓴 양이 재부팅 전까지 쓰기 공간에 남을 수 있고, 그 공간을 다른 좌석과 함께 쓰는 매장에서는 좌석이 재부팅되거나 다른 좌석에 영향을 줄 수 있습니다.";
    (what, vec![risk.to_owned()])
}

/// The verdict for an install folder from what can be told at once: how the drives are
/// attached, what is free, and which programs run. Nothing is printed, read from the game's
/// files, written or asked of a server, so that the app can call this before a patch
/// starts. `write_test` is what the caller's own test of the folder came to.
pub fn quick_verdict(install: &Path, write_test: WriteTest) -> Verdict {
    let scrub = system::hidden();
    let mut facts = Facts::none_yet(system::now());
    facts.system_drive = volume::look(&system::windows_dir());
    facts.install = Some(install.to_path_buf());
    facts.how = How::Given;
    facts.game_drive = volume::look(install);
    facts.write_test = write_test;
    look_at_programs(&mut facts);
    verdict(&Evidence::of(&facts, &scrub))
}

/// The part of a diagnosis that asks nothing and reads little. `None` when there is no
/// install folder to go on with.
fn unasked(request: &Request, stop: &AtomicBool, facts: &mut Facts, scrub: &Scrub, meter: &mut Meter) -> Option<Work> {
    let stopped = || stop.load(Ordering::Relaxed);

    meter.say();
    let machine = mfp_engine::preflight::machine();
    facts.started_from = system::started_from();
    facts.windows = system::windows(&machine.os);
    (facts.cpu, facts.cores, facts.threads, facts.ram) =
        (machine.cpu, machine.cores, machine.threads, machine.ram_bytes);
    facts.rights = system::rights();
    facts.webview = Ok(system::webview());
    facts.adapters = system::adapters();
    facts.system_drive = volume::look(&system::windows_dir());
    look_at_programs(facts);

    meter.reach(1);
    meter.stage(Stage::Install);
    facts.client = request.client;
    facts.room = request.room;
    facts.install = request.install.clone();
    facts.how = if facts.install.is_some() { request.how } else { How::NotFound };
    let client = facts.client.client();
    facts.registry = Registry { install_dir: discover::recorded(facts.client), version_hint: client.version_hint() };
    if let Some(install) = &facts.install {
        facts.game_drive = volume::look(install);
    }
    facts.other_volumes = other_volumes(facts);
    let install = facts.install.clone()?;

    let walked = game::walk(&install, stop);
    facts.game = Some(game::look(&install, client.game_program, &walked));

    // What an earlier run could not take away again goes first.
    let (in_install, elsewhere) = (write::remove_leftovers(&install), write::remove_leftovers_elsewhere());
    let (removed, left) = (in_install.0 + elsewhere.0, in_install.1 + elsewhere.1);
    if removed + left > 0 {
        facts.wrote.push(Wrote::Leftovers { removed, left });
    }
    if let Some(found) = request.writable {
        // The caller has tried the folder itself: no file is made for it here.
        facts.write_test = found;
    } else if request.write_test && !stopped() {
        facts.write_test = write::empty_file(&install);
        match facts.write_test {
            WriteTest::Passed => facts.wrote.push(Wrote::EmptyFile { removed: true }),
            WriteTest::LeftBehind(_) => facts.wrote.push(Wrote::EmptyFile { removed: false }),
            WriteTest::Failed(_) | WriteTest::Skipped => {}
        }
    }

    let given = request.table.as_ref().filter(|table| table.server == facts.client);
    let table = given.unwrap_or_else(|| Table::built_in(facts.client));
    facts.table_given = given.is_some();
    facts.tables[usize::from(facts.client == Server::Test)] = table.newest();

    // Which files are there and how long they are: no file is read for this.
    meter.reach(2);
    meter.stage(Stage::Sizes);
    let sheets = table.sheets();
    let folder = Folder::look(&install, &sheets);
    let logged = facts.game.as_ref().and_then(|game| game.patcher_log.as_deref()).and_then(State::logged);
    let sizes = by_length(&sheets, &folder, &walked, logged);
    let settled = sizes.at;
    facts.sizes = Ok(sizes);

    // What the server has: as the caller knows it, or by asking, which has one ceiling
    // for all of it.
    meter.reach(3);
    let newest = if let Some(known) = &request.shelf {
        // The caller said what the server has. Where it wants the line timed all the
        // same, that is the one thing the server is asked, under the same ceiling.
        facts.shelf = Ok(if request.time_the_line && !stopped() {
            meter.stage(Stage::Server);
            let told = known.clone();
            let timed = ask_server(Some(SERVER_TIME), stop, &mut facts.traffic, move |server| {
                shelf_as_told(client, &told, Some(server))
            });
            // What did not come in the time there is was tried all the same: it is no
            // figure that nobody asked for.
            timed.unwrap_or_else(|| Shelf {
                sample: Err(Unknown::NoAnswer),
                round_trip: Err(Unknown::NoAnswer),
                ..shelf_as_told(client, known, None)
            })
        } else {
            shelf_as_told(client, known, None)
        });
        Ok(known.newest)
    } else if request.online && !stopped() {
        meter.stage(Stage::Server);
        let from = table.newest().version;
        let looked = ask_server(Some(SERVER_TIME), stop, &mut facts.traffic, move |server| {
            let newest = server::newest(client, from, server)?;
            Ok((newest, shelf(client, newest, settled, server)))
        });
        match looked.unwrap_or(Err(Unknown::NoAnswer)) {
            Ok((newest, looked)) => {
                facts.shelf = Ok(looked);
                Ok(newest)
            }
            Err(why) => {
                facts.shelf = Err(why);
                Err(why)
            }
        }
    } else {
        Err(Unknown::Skipped)
    };

    // A sample of how fast the drive reads, which each of its two reads ends by itself.
    let running: Vec<String> = facts
        .programs
        .as_ref()
        .map(|programs| programs.game.iter().map(|name| scrub.text(name)).collect())
        .unwrap_or_default();
    let reads_allowed = !request.game_running && (running.is_empty() || request.reads_while_running);
    meter.reach(4);
    if reads_allowed && !stopped() {
        meter.stage(Stage::Reads);
        facts.reads = match &walked.largest {
            Some((largest, _)) => read::time(largest, stop),
            None => Err(Unknown::Skipped),
        };
    }
    // The steps that are asked for were not: nobody has said yes to them yet.
    if !stopped() {
        let nothing_to_read = !reads_allowed || folder.unread(u64::MAX).is_empty();
        facts.full = if nothing_to_read { Asked::NotPossible } else { Asked::Declined };
        (facts.landing, facts.limit) = (Asked::Declined, WriteLimit::Declined);
    }
    Some(Work { install, client, walked, known_up_to: table.newest(), sheets, folder, newest, running, reads_allowed })
}

/// The fixed volumes of the PC other than the ones Windows and the game are on.
fn other_volumes(facts: &Facts) -> Vec<Volume> {
    let letter_of = |volume: &Fact<Volume>| volume.as_ref().ok().map(|volume| volume.letter.clone());
    let taken = [letter_of(&facts.system_drive), letter_of(&facts.game_drive)];
    volume::letters()
        .into_iter()
        .filter(|(letter, kind)| *kind == DriveKind::Fixed && !taken.contains(&Some(letter.clone())))
        .filter_map(|(letter, _)| volume::look(Path::new(&format!("{letter}\\"))).ok())
        .collect()
}

/// The fixed volumes that can be written to other than the game's: the one Windows is on,
/// where that is another, and the rest. By their letters.
fn other_targets(facts: &Facts) -> Vec<String> {
    let game = facts.game_drive.as_ref().ok().map(|volume| volume.letter.as_str());
    let system = facts.system_drive.as_ref().ok();
    system
        .into_iter()
        .chain(&facts.other_volumes)
        .filter(|volume| volume.kind == DriveKind::Fixed && !volume.read_only && !volume.letter.is_empty())
        .filter(|volume| Some(volume.letter.as_str()) != game)
        .map(|volume| volume.letter.clone())
        .collect()
}

/// The game's drive, as a volume that a step writes to in the install folder.
fn games_target(facts: &Facts, install: &Path) -> Target {
    Target {
        letter: facts.game_drive.as_ref().map(|volume| volume.letter.clone()).unwrap_or_default(),
        game: true,
        free: volume::root_of(install).ok().and_then(|root| volume::free(&root).ok()),
        room: true,
    }
}

/// The volumes the write probe writes to: the game's drive, and where `others` says so,
/// every other fixed volume that can be written to.
fn probe_targets(facts: &Facts, install: &Path, others: bool) -> Vec<Target> {
    let others = if others { other_targets(facts) } else { Vec::new() };
    let others = others.into_iter().map(|letter| {
        let free = volume::free(&format!("{letter}\\")).ok();
        Target { letter, game: false, free, room: true }
    });
    std::iter::once(games_target(facts, install)).chain(others).collect()
}

/// The volumes the write limit test can be run on: the game's drive first, and then every
/// other fixed volume that can be written to, each with whether it has the room.
fn limit_targets(facts: &Facts, install: &Path, sizes: &Sizes) -> Vec<Target> {
    let needed = sizes.limit_bytes.saturating_add(write::LIMIT_FLOOR);
    let mut targets = probe_targets(facts, install, true);
    for target in &mut targets {
        target.room = target.free.is_some_and(|free| free >= needed);
    }
    targets
}

/// Which of `targets` is meant by `on`: the first of them without a name, and else the
/// one with that letter. `Err` with what the report says where there is none.
fn target_named(targets: &[Target], on: Option<&str>) -> Result<usize, Box<WriteLimit>> {
    match on {
        None if targets.is_empty() => Err(Box::new(WriteLimit::NotPossible)),
        None => Ok(0),
        Some(named) => targets
            .iter()
            .position(|volume| volume.letter.eq_ignore_ascii_case(named))
            .ok_or_else(|| Box::new(WriteLimit::NoPlace { volume: named.to_owned(), error: None })),
    }
}

/// That a volume has less free than the write limit test writes and leaves, with the
/// figures.
fn no_room_on(target: &Target, sizes: &Sizes) -> WriteLimit {
    WriteLimit::NoRoom {
        volume: target.letter.clone(),
        free: target.free,
        writes: sizes.limit_bytes,
        floor: write::LIMIT_FLOOR,
    }
}

/// That less memory is available than the write limit test wants there to be before it
/// begins, where that is so: too little to tell, in time, whether the writing takes it.
fn no_memory_for(plan: &Fill) -> Option<WriteLimit> {
    let wanted = plan.memory_to_begin();
    let available = system::memory().map(|memory| memory.available);
    (wanted > 0 && !available.is_some_and(|available| available >= wanted))
        .then_some(WriteLimit::NoMemory { available, wanted })
}

/// How the write limit test is run on this PC: sized as asked, and with what counts as
/// too slow worked out from what the PC's wired line carries.
///
/// What it leaves of memory goes by what the write probe showed on the volume in
/// question (`probed`). Where the probe's steps settled at what the line carries and
/// memory did not go with them, memory is not where this PC keeps what it writes: the
/// test then asks for no more memory than the PC needs for itself. Where memory was seen
/// to go down, or no probe was run, it asks for all it is sized to.
fn limit_plan(sizes: &Sizes, facts: &Facts, probed: Option<&crate::facts::Landing>) -> Fill {
    use crate::verdict::{Keeps, Went, keeps, went};
    let line = line_of(facts);
    let not_in_memory = probed.is_some_and(|landing| {
        landing.written >= landing.wanted
            && keeps(landing) == Keeps::Unseen
            && matches!(went(landing, line), Went::Line { memory_held: true, .. })
    });
    let floor = if not_in_memory { sizes.limit_memory_floor.min(write::MEMORY_OWN) } else { sizes.limit_memory_floor };
    Fill::limit(sizes.limit_bytes, sizes.limit_step, floor, sizes.settle).on_line(line)
}

/// The write probe: writes its files into the install folder and then onto every other
/// of `targets`, each in its turn.
fn write_probe(
    targets: &[Target],
    install: &Path,
    sizes: &Sizes,
    facts: &mut Facts,
    stop: &AtomicBool,
    meter: &mut Meter,
) {
    let plan = Fill::probe(sizes.probe_bytes, sizes.settle);
    meter.say();
    let landing = write::fill(install, plan, stop, &mut |written| meter.reach(written));
    // Files were made unless the folder refused them outright, or the drive had too
    // little free for the probe to begin.
    if landing.written > 0 || (landing.error.is_none() && landing.halted.is_none()) {
        facts.wrote.push(Wrote::ProbeFile { bytes: landing.written, removed: landing.deleted });
    }
    facts.landing = Asked::Done(landing);
    facts.other_landings.clear();
    for (at, target) in targets.iter().enumerate().skip(1) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // What the volumes before this one did not write is not waited for.
        let before = plan.total.saturating_mul(at as u64);
        meter.now.part = at as u32;
        meter.reach(before);
        let parents = write::parents_on(&target.letter);
        let probed =
            probe_elsewhere(&target.letter, &parents, plan, stop, &mut |written| meter.reach(before + written));
        if let Ok(landing) = &probed.landing {
            facts.wrote.push(Wrote::OwnFolder {
                volume: target.letter.clone(),
                place: landing.place,
                bytes: landing.written,
                limit: false,
                removed: landing.deleted,
            });
        }
        facts.other_landings.push(probed);
    }
    if !stop.load(Ordering::Relaxed) {
        meter.finish();
    }
}

/// The write probe on a volume other than the game's, in a folder of its own under the
/// first of `parents` that takes one.
fn probe_elsewhere(
    letter: &str,
    parents: &[(PathBuf, Place)],
    plan: Fill,
    stop: &AtomicBool,
    on_progress: &mut dyn FnMut(u64),
) -> Probed {
    Probed { volume: letter.to_owned(), landing: write::fill_elsewhere(parents, plan, stop, on_progress) }
}

/// The write limit test: writes as much as a main patch does onto `target`, file after
/// file, and removes it all again.
fn write_limit(target: &Target, install: &Path, plan: Fill, stop: &AtomicBool, meter: &mut Meter) -> WriteLimit {
    meter.say();
    let mut progress = |written: u64| meter.reach(written);
    let landing = if target.game {
        write::fill(install, plan, stop, &mut progress)
    } else {
        match write::fill_elsewhere(&write::parents_on(&target.letter), plan, stop, &mut progress) {
            Ok(landing) => landing,
            Err(error) => return WriteLimit::NoPlace { volume: target.letter.clone(), error },
        }
    };
    // It wrote all it set out to write. A test that ended early leaves the count where
    // it got to.
    if landing.written >= landing.wanted && !stop.load(Ordering::Relaxed) {
        meter.finish();
    }
    WriteLimit::Done(landing)
}

/// Notes which of the running programs and drivers matter.
fn look_at_programs(facts: &mut Facts) {
    let games: Vec<&str> =
        Server::ALL.iter().flat_map(|server| [server.client().game_program, server.client().launcher]).collect();
    let drivers = programs::drivers();
    facts.drivers_listed = drivers.as_ref().map(|_| ()).map_err(|why| *why);
    facts.programs = programs::processes()
        .map(|processes| programs::sort(processes.into_iter().chain(drivers.unwrap_or_default()), &games));
}

impl Work {
    /// Reads the files of the folder that are not read yet and no longer than `limit`, and
    /// says how the folder stands by their checksums.
    ///
    /// The server's patches are asked what they start from when the table built into this
    /// program does not settle it, or when the server has moved on since.
    fn read(
        &mut self,
        limit: u64,
        online: bool,
        traffic: &mut Traffic,
        stop: &AtomicBool,
        meter: &mut Meter,
    ) -> Comparison {
        let stage = meter.now.stage;
        meter.expect(self.folder.unread(limit).iter().map(|(_, len)| len).sum());
        meter.say();
        let mut cut_short = read_all(&mut self.folder, &self.install, limit, stop, meter);
        let mut standing = tell_apart(&self.sheets, &mut self.folder, &self.install, stop, meter);
        let table_is_behind = self.newest.is_ok_and(|newest| newest.order() > self.known_up_to.order());
        if let Ok(newest) = self.newest
            && online
            && !cut_short
            && (standing.fits.is_empty() || table_is_behind)
        {
            meter.stage(Stage::Tables);
            let have: Vec<State> = self.sheets.iter().map(|sheet| sheet.state).collect();
            let client = self.client;
            let asked = ask_server(None, stop, traffic, move |server| server::sheets(client, newest, &have, server).0);
            let more = asked.unwrap_or_default();
            meter.stage(stage);
            if !more.is_empty() {
                self.sheets.extend(more);
                self.folder.look_at_more(&self.install, &self.sheets);
                meter.expect(self.folder.unread(limit).iter().map(|(_, len)| len).sum());
                cut_short = read_all(&mut self.folder, &self.install, limit, stop, meter);
                standing = tell_apart(&self.sheets, &mut self.folder, &self.install, stop, meter);
            }
        }
        let cut_short = cut_short || stop.load(Ordering::Relaxed);
        if !cut_short {
            meter.finish();
        }
        comparison(&self.sheets, &standing, &self.folder, &self.walked, cut_short)
    }
}

/// Reads every file that is there, not read yet and no longer than `limit`, shortest first.
/// Says whether it was stopped before the end.
fn read_all(folder: &mut Folder, install: &Path, limit: u64, stop: &AtomicBool, meter: &mut Meter) -> bool {
    for (key, len) in folder.unread(limit) {
        let before = meter.now.done;
        if folder.read(install, &key, stop, &mut |bytes| meter.add(bytes)).is_err() {
            return true;
        }
        // A file that could not be read, or was shorter than it said, counts as done.
        meter.reach(before + len);
    }
    false
}

/// Works out how the folder stands, and while more than one state fits, reads the file that
/// tells them apart, however long it is.
fn tell_apart(sheets: &[Sheet], folder: &mut Folder, install: &Path, stop: &AtomicBool, meter: &mut Meter) -> Standing {
    let mut standing = compare::standing(sheets, &folder.seen);
    for _ in 0..32 {
        let Some(key) = compare::tells_apart(sheets, &standing.fits, &folder.seen).map(str::to_owned) else { break };
        // A file that the pass was not going to read: it is that much more to do.
        if let Some(compare::Seen::Present { len, .. }) = folder.seen.get(&key) {
            meter.expect(*len);
        }
        if folder.read(install, &key, stop, &mut |bytes| meter.add(bytes)).is_err() {
            break;
        }
        standing = compare::standing(sheets, &folder.seen);
    }
    standing
}

/// Files in the folder that the sheet at `against` does not name.
fn not_in_table(sheets: &[Sheet], against: Option<usize>, walked: &Walked) -> u64 {
    match against {
        Some(at) => walked.keys.iter().filter(|key| !sheets[at].files.contains_key(*key)).count() as u64,
        None => walked.files,
    }
}

/// What a comparison by checksums came to, for the report.
fn comparison(sheets: &[Sheet], standing: &Standing, folder: &Folder, walked: &Walked, cut_short: bool) -> Comparison {
    let against = standing.nearest.map(|at| &sheets[at]);
    // While a file that would tell the fitting states apart is still unread, as after a
    // stop, the folder could be at any of them.
    let open = compare::tells_apart(sheets, &standing.fits, &folder.seen).is_some();
    Comparison {
        by_length: false,
        at: standing.settled(sheets).filter(|_| !open),
        either: if open { standing.fits.iter().map(|at| sheets[*at].state).collect() } else { Vec::new() },
        against: against.map(|sheet| (sheet.state, sheet.source)),
        tally: standing.nearest.map(|at| standing.tallies[at].clone()).unwrap_or_default(),
        not_in_table: not_in_table(sheets, standing.nearest, walked),
        bytes_read: folder.bytes_read,
        seconds: folder.seconds,
        cut_short,
    }
}

/// What the comparison that reads no file came to: how the folder stands by which files
/// are there and how long they are.
///
/// Lengths tell less than checksums. Where one state fits, the folder is taken to be at
/// it. Where several do, it is at the one the game's own patcher logged last (`logged`),
/// if that is among them, and otherwise it could be at any of them.
fn by_length(sheets: &[Sheet], folder: &Folder, walked: &Walked, logged: Option<State>) -> Comparison {
    let standing = compare::standing_by_length(sheets, &folder.seen);
    let fits: Vec<State> = standing.fits.iter().map(|at| sheets[*at].state).collect();
    let at = match (&fits[..], logged) {
        ([only], _) => Some(*only),
        (_, Some(logged)) if fits.contains(&logged) => Some(logged),
        _ => None,
    };
    let against = match at {
        Some(at) => standing.fits.iter().copied().find(|fit| sheets[*fit].state == at),
        None => standing.nearest,
    };
    Comparison {
        by_length: true,
        at,
        either: if at.is_none() { fits } else { Vec::new() },
        against: against.map(|at| (sheets[at].state, sheets[at].source)),
        tally: against.map(|at| standing.tallies[at].clone()).unwrap_or_default(),
        not_in_table: not_in_table(sheets, against, walked),
        bytes_read: 0,
        seconds: 0.0,
        cut_short: false,
    }
}

/// What the server has for a folder at `at`: the main patch it would take next, or else the
/// newest minor patch, and how fast the line to the server is.
fn shelf(client: &Client, newest: State, at: Option<State>, nexon: &dyn server::Shelf) -> Shelf {
    let on_shelf = |url: String, file_name: String| -> Option<PatchOnShelf> {
        let size = nexon.size_of(&url).ok().flatten()?;
        let originals = server::originals_of(&url, nexon).ok().flatten().map(|table| table.len());
        Some(PatchOnShelf { file_name, size, originals })
    };
    let behind = at.filter(|at| at.version < newest.version);
    let next_main = behind.and_then(|at| {
        // Straight to the newest version where the server has that patch, or else one on.
        [newest.version, at.version + 1]
            .into_iter()
            .find_map(|to| on_shelf(client.patch_url(at.version, to), client.patch_file_name(at.version, to)))
    });
    let patch = next_main.clone().or_else(|| {
        let (from, to) = (newest.minor.checked_sub(1).filter(|from| *from >= 1)?, newest.minor);
        on_shelf(client.minor_patch_url(newest.version, from, to), format!("{from:02}to{to:02}.patch"))
    });
    let (sample, round_trip) = line_to(client, newest, nexon);
    Shelf { newest, patch, patch_is_next_main: next_main.is_some(), sample, round_trip }
}

/// How fast the line to Nexon's server is, and how long the way there: a piece of the main
/// patch to `newest` is fetched, and connections to its server are opened. The bytes that
/// came with the seconds they took, and the milliseconds of the way there and back. Nothing
/// else is asked of the server.
fn line_to(client: &Client, newest: State, nexon: &dyn server::Shelf) -> (Fact<(u64, f64)>, Fact<f64>) {
    // The newest main patch is the one file there that is sure to be long enough.
    let Some(before) = newest.version.checked_sub(1) else {
        return (Err(Unknown::Skipped), Err(Unknown::Skipped));
    };
    let url = client.patch_url(before, newest.version);
    let sample = server::sample(&url, nexon);
    // The way there is timed last: by now the server has answered, so its name is looked
    // up already and a connection to it opens.
    (sample, nexon.round_trip(&url))
}

/// What a diagnosis says of the server where its caller said what the server has
/// (`known`): that, and how fast the line is where there is a server to time it on
/// (`nexon`). Nothing else is asked of the server: not which version is the newest, not how
/// large a patch is, not what one starts from.
fn shelf_as_told(client: &Client, known: &OnShelf, nexon: Option<&dyn server::Shelf>) -> Shelf {
    let untimed = (Err(Unknown::Skipped), Err(Unknown::Skipped));
    let (sample, round_trip) = nexon.map_or(untimed, |nexon| line_to(client, known.newest, nexon));
    Shelf {
        newest: known.newest,
        patch: known.patch.clone(),
        patch_is_next_main: known.patch_is_next_main,
        sample,
        round_trip,
    }
}

// The console program: finds the folder, asks with the words of the offers, and makes the
// calls above.

/// Whether a step that is only taken when asked for is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Choice {
    /// The user is asked, and no answer means no.
    #[default]
    Ask,
    Yes,
    No,
}

/// How the console program runs a diagnosis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The install folder, when whoever starts the program names it. Nothing is looked for
    /// then: not where the registry says, not in the usual place, not on the drives.
    pub folder: Option<PathBuf>,
    /// Which client is meant. `None` goes by what is found.
    pub server: Option<Server>,
    /// Reading the game's files up to 64 MB to hold their checksums against the table.
    pub checksums: Choice,
    /// Reading every file of the game for the full comparison.
    pub full: Choice,
    /// Writing the probe files.
    pub write_probe: Choice,
    /// Reading from the game's drive while the game runs. `No` leaves out the read sample
    /// and both comparisons that read; otherwise their questions say that the game runs.
    pub heavy_reads: Choice,
    /// Making and removing the empty file.
    pub write_test: bool,
    /// Asking Nexon's patch server.
    pub online: bool,
    /// How much the write probe writes on each volume.
    pub probe_bytes: u64,
    /// How long after the probe files are removed memory is looked at once more.
    pub probe_settle: Duration,
    /// Running the write probe on every other fixed volume of the PC too, and not only in
    /// the install folder.
    pub probe_other_volumes: bool,
    /// Writing as much as a main patch does, to see how far the PC takes it.
    pub write_limit: Choice,
    /// How much the write limit test writes, and how much to a file. Other figures than
    /// the ones it is meant to be run with are for trying it out.
    pub limit_bytes: u64,
    pub limit_step: u64,
    /// The volume the write limit test is run on, `D:`. `None` is the game's drive, or
    /// the one the user picks.
    pub limit_on: Option<String>,
    /// What the write limit test leaves of memory; see [`write::MEMORY_FLOOR`].
    pub limit_memory_floor: u64,
    /// A table of known file states to go by in place of the one built into the program,
    /// for the client it is of: one made after a patch that came out since.
    pub table: Option<Table>,
}

impl Default for Options {
    fn default() -> Options {
        let sizes = Sizes::default();
        Options {
            folder: None,
            server: None,
            checksums: Choice::Ask,
            full: Choice::Ask,
            write_probe: Choice::Ask,
            heavy_reads: Choice::Ask,
            write_test: true,
            online: true,
            probe_bytes: sizes.probe_bytes,
            probe_settle: sizes.settle,
            probe_other_volumes: sizes.probe_other_volumes,
            write_limit: Choice::Ask,
            limit_bytes: sizes.limit_bytes,
            limit_step: sizes.limit_step,
            limit_on: None,
            limit_memory_floor: sizes.limit_memory_floor,
            table: None,
        }
    }
}

impl Options {
    /// How the steps that write are sized.
    fn sizes(&self) -> Sizes {
        Sizes {
            probe_bytes: self.probe_bytes,
            settle: self.probe_settle,
            probe_other_volumes: self.probe_other_volumes,
            limit_bytes: self.limit_bytes,
            limit_step: self.limit_step,
            limit_memory_floor: self.limit_memory_floor,
        }
    }
}

/// Whoever runs the console program: told what is going on, and asked what only they can
/// decide.
pub trait Desk {
    /// What the diagnostic is doing now.
    fn status(&mut self, text: &str);
    /// A question with yes or no for an answer. No answer is a no.
    fn confirm(&mut self, question: &str) -> bool;
    /// A question with a line of text for an answer. `None` when there is none.
    fn answer(&mut self, question: &str) -> Option<String>;
    /// A question with one of `choices` for an answer: its place among them. No answer is
    /// `default`, and so is the answer of a desk that cannot ask this.
    fn choose(&mut self, question: &str, choices: &[String], default: usize) -> usize {
        let _ = (question, choices);
        default
    }
}

impl Choice {
    fn decide(self, desk: &mut dyn Desk, question: &str) -> bool {
        match self {
            Choice::Yes => true,
            Choice::No => false,
            Choice::Ask => desk.confirm(question),
        }
    }
}

/// Turns the numbers a call reports into the lines the console shows: each stage once,
/// and how far the long ones are, now and then.
#[derive(Default)]
struct Told {
    seen: Vec<(Stage, u32)>,
    at: u64,
}

impl Told {
    /// `names` are what the volumes of the write probe are called, and `each` how much is
    /// written to one of them; `step` is the length of a file of the write limit test.
    fn tell(&mut self, desk: &mut dyn Desk, progress: Progress, names: &[String], each: u64, step: u64) {
        let Progress { stage, done, total, part } = progress;
        let name = names.get(part as usize).map_or("", String::as_str);
        if !self.seen.contains(&(stage, part)) {
            self.seen.push((stage, part));
            self.at = match stage {
                Stage::WriteProbe => each.saturating_mul(u64::from(part)),
                _ => self.at,
            };
            match stage {
                Stage::WriteProbe => desk.status(&format!("{}: {name}", stage.label())),
                _ => desk.status(stage.label()),
            }
        }
        // Often enough to show that it is alive, and not so often that it floods a console.
        let every = match stage {
            Stage::Checksums | Stage::Full => 512 << 20,
            Stage::WriteProbe => 256 << 20,
            Stage::WriteLimit => step.max(64 << 20),
            _ => return,
        };
        if done < self.at + every {
            return;
        }
        self.at = done;
        desk.status(&match stage {
            Stage::WriteProbe => {
                let written = done - each.saturating_mul(u64::from(part));
                format!("{}: {name} {}", stage.label(), size(written))
            }
            Stage::WriteLimit => format!("{}: {} / {}", stage.label(), gb(done), gb(total)),
            _ => format!("파일 내용 비교 중: {} / {}", size(done), size(total)),
        });
    }
}

/// Runs the whole diagnostic as the console program does: the part that asks nothing, and
/// then each step that is asked for, where the answer is yes. Setting `stop` ends it at the
/// next step, with what was found until then.
pub fn run(options: &Options, desk: &mut dyn Desk, stop: &AtomicBool) -> (Facts, Scrub) {
    let stopped = || stop.load(Ordering::Relaxed);
    let (client, how, install) = locate(options, desk);
    let request = Request {
        install,
        client,
        how,
        game_running: false,
        reads_while_running: options.heavy_reads != Choice::No,
        write_test: options.write_test,
        writable: None,
        online: options.online,
        shelf: None,
        time_the_line: false,
        table: options.table.clone(),
        room: None,
        head: Head::Check,
        sizes: options.sizes(),
    };
    let plan = Fill::probe(options.probe_bytes, options.probe_settle);
    let mut told = Told::default();
    let mut diagnosis =
        Diagnosis::start(&request, stop, &mut |progress| told.tell(desk, progress, &[], 0, options.limit_step));

    for step in Extra::ALL {
        if stopped() || diagnosis.work.is_none() {
            break;
        }
        let choice = match step {
            Extra::Checksums => options.checksums,
            Extra::Full => options.full,
            Extra::WriteProbe => options.write_probe,
            Extra::WriteLimit => options.write_limit,
        };
        let on = match step {
            // Told beforehand not to run it, it is left out without a word about room.
            Extra::WriteLimit if choice == Choice::No => continue,
            Extra::WriteLimit => match pick(options, &diagnosis, desk) {
                Ok(on) => on,
                Err(not_run) => {
                    Arc::make_mut(&mut diagnosis.facts).limit = *not_run;
                    continue;
                }
            },
            _ => {
                let offer = diagnosis.offer(step, None);
                if offer.refused.is_some() || !choice.decide(desk, &offer.question) {
                    continue;
                }
                None
            }
        };
        let names: Vec<String> = match step {
            Extra::WriteProbe => {
                let volumes = diagnosis.offer(step, None).volumes;
                volumes
                    .iter()
                    .map(|volume| if volume.game { "설치 폴더".to_owned() } else { volume.letter.clone() })
                    .collect()
            }
            _ => Vec::new(),
        };
        let mut told = Told::default();
        diagnosis = diagnosis.run(step, on.as_deref(), stop, &mut |progress| {
            told.tell(desk, progress, &names, plan.total, options.limit_step);
        });
    }
    diagnosis.into_parts()
}

/// Which volume the write limit test is run on, once the user has agreed to it: `None`
/// for the game's drive, or the letter of another. `Err` when it is not run, with why:
/// nobody agreed, the volume has no room for it, too little memory is available, or the
/// volume that was named is none of them.
///
/// It is refused, with the figures, where the volume has less free than the test writes
/// and leaves: it is to find the limit of the PC, not to run a drive full. Where no volume
/// has the room, nothing is even asked.
fn pick(options: &Options, diagnosis: &Diagnosis, desk: &mut dyn Desk) -> Result<Option<String>, Box<WriteLimit>> {
    let offer = diagnosis.offer(Extra::WriteLimit, options.limit_on.as_deref());
    // Too little memory, or a volume that was named and is none of them or has no room.
    if let Some(Refused::Limit(not_run)) = &offer.refused
        && (options.limit_on.is_some() || matches!(**not_run, WriteLimit::NoMemory { .. }))
    {
        return Err(not_run.clone());
    }
    let at = pick_among(options, &offer, desk)?;
    Ok((!offer.volumes[at].game).then(|| offer.volumes[at].letter.clone()))
}

/// Which of the volumes of an offer of the write limit test it is run on: its place
/// among them. See [`pick`].
fn pick_among(options: &Options, offer: &Offer, desk: &mut dyn Desk) -> Result<usize, Box<WriteLimit>> {
    let (volumes, sizes) = (&offer.volumes, options.sizes());
    let no_room = |volume: &Target| Box::new(no_room_on(volume, &sizes));
    let Some(game) = volumes.first() else { return Err(Box::new(WriteLimit::NotPossible)) };

    // The volume was named when the program was started.
    if let Some(named) = &options.limit_on {
        let at = target_named(volumes, Some(named))?;
        if !volumes[at].room {
            return Err(no_room(&volumes[at]));
        }
        return if options.write_limit.decide(desk, &offer.question) {
            Ok(at)
        } else {
            Err(Box::new(WriteLimit::Declined))
        };
    }
    if !volumes.iter().any(|volume| volume.room) {
        return Err(no_room(game));
    }
    // Told to run it without being asked, or with one volume only: the game's drive.
    let asks_which = options.write_limit == Choice::Ask && volumes.len() > 1;
    let asked = if asks_which {
        // The same words, about a volume that is yet to be picked.
        let (what, warning) = limit_words("고정 볼륨 한 곳(다음에 선택, 기본은 게임 드라이브)", &sizes);
        question(&Offer { what, warning, ..offer.clone() })
    } else {
        offer.question.clone()
    };
    if !options.write_limit.decide(desk, &asked) {
        return Err(Box::new(WriteLimit::Declined));
    }
    let at = if asks_which {
        let choices: Vec<String> = volumes
            .iter()
            .map(|volume| {
                let role = if volume.game { " 게임 드라이브(설치 폴더)" } else { "" };
                let free = volume.free.map_or("확인 불가".to_owned(), gb);
                let short = if volume.room { "" } else { " · 쓸 양과 남길 양보다 적음" };
                format!("{}{role} · 여유 {free}{short}", volume.letter)
            })
            .collect();
        desk.choose("한도 검사를 할 볼륨을 선택해 주세요.", &choices, 0).min(volumes.len() - 1)
    } else {
        0
    };
    if volumes[at].room { Ok(at) } else { Err(no_room(&volumes[at])) }
}

/// Which client a folder that was named holds: the test world's only when its program is
/// there and the live game's is not.
fn client_in(folder: &Path) -> Server {
    let has = |client: &Client| folder.join(client.game_program).is_file();
    if has(&mfp_kms::TEST) && !has(&mfp_kms::LIVE) { Server::Test } else { Server::Live }
}

/// Finds the install folder: the one that was named, or else by looking, or else by asking.
/// Which client it holds, how it was found, and where it is.
fn locate(options: &Options, desk: &mut dyn Desk) -> (Server, How, Option<PathBuf>) {
    if let Some(folder) = &options.folder {
        return (options.server.unwrap_or_else(|| client_in(folder)), How::Given, Some(folder.clone()));
    }
    let servers: Vec<Server> = options.server.map_or(Server::ALL.to_vec(), |server| vec![server]);
    for server in &servers {
        if let Some((how, dir)) = discover::find(server.client(), &Places::of_this_pc(*server)) {
            return (*server, how, Some(dir));
        }
    }
    let question = "설치 폴더를 찾지 못했습니다. 메이플스토리 설치 폴더 경로를 입력해 주세요. (건너뛰려면 Enter)";
    while let Some(typed) = desk.answer(question).filter(|typed| !typed.trim().is_empty()) {
        let found = servers.iter().find_map(|server| Some((*server, discover::from_typed(&typed, server.client())?)));
        if let Some((server, dir)) = found {
            return (server, How::Typed, Some(dir));
        }
        desk.status("메이플스토리 설치 폴더가 아닙니다.");
    }
    (servers[0], How::NotFound, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::known::Expect;
    use crate::verdict::Outlook;

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    /// A desk that answers from a script and notes what it was asked.
    #[derive(Default)]
    struct Scripted {
        yes: bool,
        /// Which of the choices it is offered it takes.
        picks: usize,
        asked: Vec<String>,
        offered: Vec<String>,
        said: Vec<String>,
    }

    impl Desk for Scripted {
        fn status(&mut self, text: &str) {
            self.said.push(text.to_owned());
        }

        fn confirm(&mut self, question: &str) -> bool {
            self.asked.push(question.to_owned());
            self.yes
        }

        fn answer(&mut self, question: &str) -> Option<String> {
            self.asked.push(question.to_owned());
            None
        }

        fn choose(&mut self, question: &str, choices: &[String], _default: usize) -> usize {
            self.asked.push(question.to_owned());
            self.offered = choices.to_vec();
            self.picks
        }
    }

    /// Makes a folder that holds, for every file the newest sheet of the live client's
    /// table names, a file of a few made-up bytes: an install in shape, and in no file the
    /// real one. The game's program is made longer than the checksum comparison reads, by
    /// setting its length, which takes no time and no room worth speaking of.
    fn made_up_install(dir: &Path) -> usize {
        let newest = Table::built_in(Server::Live).sheets().pop().unwrap();
        let mut made = 0;
        for (name, expect) in newest.files.values() {
            if matches!(expect, Expect::Gone) {
                continue;
            }
            let path = dir.join(mfp_engine::plan::relative_path(name).unwrap());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, name.as_bytes()).unwrap();
            if name == mfp_kms::LIVE.game_program {
                std::fs::File::options().write(true).open(&path).unwrap().set_len(QUICK_LIMIT + 1).unwrap();
            }
            made += 1;
        }
        made
    }

    /// What the row of a report that is called `name` says, in its first line.
    fn said(report: &str, name: &str) -> Option<String> {
        let row = report.lines().find(|line| line.starts_with(&format!("{name}  ")))?;
        Some(row[name.len()..].trim().to_owned())
    }

    /// A made-up server that has every file it is asked for, unless it is down, and notes
    /// what it was asked.
    #[derive(Default)]
    struct MadeUpServer {
        down: bool,
        asked: std::cell::RefCell<Vec<String>>,
    }

    impl MadeUpServer {
        fn answer<T>(&self, what: String, with: T) -> Result<T, Unknown> {
            self.asked.borrow_mut().push(what);
            if self.down { Err(Unknown::NoConnection) } else { Ok(with) }
        }
    }

    impl server::Shelf for MadeUpServer {
        fn size_of(&self, url: &str) -> Result<Option<u64>, Unknown> {
            self.answer(format!("size {url}"), Some(1 << 30))
        }

        fn start_of(&self, url: &str, len: u64) -> Result<Option<Vec<u8>>, Unknown> {
            self.answer(format!("start {url}"), Some(vec![7; len.min(4096) as usize]))
        }

        fn round_trip(&self, url: &str) -> Result<f64, Unknown> {
            self.answer(format!("round trip {url}"), 8.3)
        }
    }

    /// The app says what the server has and wants the line timed all the same: a piece of
    /// the newest main patch is fetched, the way to its server is timed, and nothing else
    /// is asked. Here the server is a made-up one: no network.
    #[test]
    fn line_is_timed_for_a_caller_that_says_what_the_server_has_and_nothing_else_is_asked() {
        let live = &mfp_kms::LIVE;
        let newest = State { version: 421, minor: 4 };
        let patch = PatchOnShelf { file_name: "03to04.patch".to_owned(), size: 163_520_879, originals: None };
        let known = OnShelf { newest, patch: Some(patch), patch_is_next_main: false };
        let main_patch = live.patch_url(420, 421);
        let timing = [format!("start {main_patch}"), format!("round trip {main_patch}")];

        let server = MadeUpServer::default();
        let timed = shelf_as_told(live, &known, Some(&server));
        // What the caller said is passed on as it is.
        assert_eq!((timed.newest, &timed.patch, timed.patch_is_next_main), (newest, &known.patch, false));
        assert!(matches!(timed.sample, Ok((4096, seconds)) if seconds >= 0.0), "{:?}", timed.sample);
        assert_eq!(timed.round_trip, Ok(8.3));
        assert_eq!(*server.asked.borrow(), timing);
        // Without a server to time it on, nothing is asked and nothing is said to be measured.
        let untimed = shelf_as_told(live, &known, None);
        assert_eq!((untimed.sample, untimed.round_trip), (Err(Unknown::Skipped), Err(Unknown::Skipped)));
        assert_eq!((untimed.newest, &untimed.patch), (newest, &known.patch));
        // A server that cannot be reached: each of the two says so, and is not left out.
        let down = MadeUpServer { down: true, ..MadeUpServer::default() };
        let unreached = shelf_as_told(live, &known, Some(&down));
        assert_eq!((unreached.sample, unreached.round_trip), (Err(Unknown::NoConnection), Err(Unknown::NoConnection)));
        assert_eq!(*down.asked.borrow(), timing);

        // The diagnosis that asks the server what it has times the line in the same way,
        // after its other questions.
        let asking = MadeUpServer::default();
        let looked = shelf(live, newest, Some(newest), &asking);
        assert_eq!((looked.sample.map(|(bytes, _)| bytes), looked.round_trip), (Ok(4096), Ok(8.3)));
        let asked = asking.asked.borrow();
        assert!(asked.len() > 2 && asked[0].starts_with("size "), "{asked:?}");
        assert_eq!(asked[asked.len() - 2..], timing);
        // A request times the line only where it is told to.
        assert!(!Request::of(Path::new(r"D:\Games\Maple"), Server::Live).time_the_line);
    }

    fn offline(folder: &Path) -> Options {
        Options {
            folder: Some(folder.to_path_buf()),
            online: false,
            heavy_reads: Choice::Yes,
            probe_bytes: 4 * MIB,
            probe_settle: Duration::from_millis(200),
            // The tests write into their own folders and nowhere else on the PC they run
            // on, whatever volumes that PC has.
            probe_other_volumes: false,
            // A write limit test of the size it is meant to have is never run by a test:
            // here it writes six megabytes in three files, if it is run at all, and asks
            // nothing of the memory of the PC the tests run on.
            limit_bytes: 6 * MIB,
            limit_step: 2 * MIB,
            limit_memory_floor: 0,
            ..Options::default()
        }
    }

    #[test]
    fn run_that_asks_nothing_reads_no_file_of_the_game_and_writes_one_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let made = made_up_install(dir.path());
        let mut desk = Scripted::default();
        let (facts, scrub) = run(&offline(dir.path()), &mut desk, &AtomicBool::new(false));

        assert_eq!((facts.how, facts.client), (How::Given, Server::Live));
        assert_eq!(facts.install.as_deref(), Some(dir.path()));
        assert!(facts.system_drive.is_ok() && facts.game_drive.is_ok());
        assert_eq!(facts.game.as_ref().unwrap().files.unwrap().0, made as u64);
        // The empty file was made and removed, and that is all that was written: every
        // question was answered with no.
        assert_eq!(facts.write_test, WriteTest::Passed);
        assert_eq!(facts.wrote, [Wrote::EmptyFile { removed: true }]);
        assert_eq!((&facts.full, &facts.landing), (&Asked::Declined, &Asked::Declined));
        assert_eq!((&facts.limit, &facts.quick), (&WriteLimit::Declined, &Err(Unknown::Skipped)));
        assert!(facts.other_landings.is_empty());
        // Each step that reads much or writes was asked for by itself, in this order.
        let steps = [
            "파일 내용 비교 (일부): ",
            "파일 내용 비교 (전체): ",
            "쓰기 검사: 설치 폴더에 ",
            "한도 검사: 설치 폴더에 ",
        ];
        let asked: Vec<&String> =
            desk.asked.iter().filter(|question| steps.iter().any(|step| question.starts_with(step))).collect();
        assert_eq!(asked.len(), 4, "{:?}", desk.asked);
        assert!(asked.iter().zip(steps).all(|(question, step)| question.starts_with(step)), "{asked:?}");
        assert!(asked.iter().all(|question| question.ends_with(" 진행하시겠습니까?")), "{asked:?}");
        // What a step costs on this PC is said in its question, from how fast the drive
        // was just read.
        let reads = facts.reads.expect("the drive was not timed");
        assert!(reads.bytes >= 16 * MIB && reads.bytes <= read::SAMPLE);
        assert!(asked.iter().all(|question| question.contains(" 이 PC의 읽기 속도(")), "{asked:?}");
        assert!(asked[0].contains("64 MB 이하 게임 파일 ") && asked[0].contains("로 약 "), "{}", asked[0]);
        assert!(asked[2].contains("1 MB 임시 파일 4개(4 MB)를 썼다가 지우고") && asked[2].contains("로 쓴다면 약 "));
        // The write limit test says what it does and what can come of it before it asks,
        // in two lines.
        assert_eq!(asked[3].lines().count(), 2, "{}", asked[3]);
        assert!(
            asked[3].contains("본 패치만큼(6 MB) 임시 파일을 2 MB씩 실제로 썼다가 모두 지웁니다.")
                && asked[3].contains("다른 좌석과 함께 쓰는 매장에서는 좌석이 재부팅되거나 다른 좌석에 영향을 줄 수"),
            "{}",
            asked[3]
        );
        // The other fixed volumes are listed, and the two that have lines of their own
        // are not among them.
        let own =
            [facts.system_drive.as_ref().unwrap().letter.clone(), facts.game_drive.as_ref().unwrap().letter.clone()];
        assert!(
            facts.other_volumes.iter().all(|volume| !own.contains(&volume.letter) && volume.kind == DriveKind::Fixed)
        );
        assert_eq!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().file_name().to_string_lossy().starts_with(".mfp"))
                .count(),
            0
        );
        // No server was asked.
        assert_eq!(facts.shelf, Err(Unknown::Skipped));
        assert_eq!(facts.traffic.requests, 0);
        // How long it took is noted, without the waiting for answers.
        assert!(facts.took.0 > 0.0 && facts.took.1 >= facts.took.0, "{:?}", facts.took);

        // No file was read. Every file is made-up, and those the table knows the length
        // of are not as long as it says: the folder fits no version.
        let sizes = facts.sizes.as_ref().expect("the files were not looked at");
        assert!(sizes.by_length && sizes.bytes_read == 0 && !sizes.cut_short);
        assert_eq!((sizes.at, sizes.tally.same), (None, 0));
        assert!(!sizes.tally.differs.is_empty() && sizes.tally.missing.is_empty(), "{:?}", sizes.tally.differs.len());
        // Without the server it cannot be told whether that is a newer version or a fault,
        // so the patch is not said to work and not said to fail.
        let verdict = crate::report::verdict_of(&facts, &scrub);
        assert_eq!(verdict.patch, Outlook::Unknown);
        // And the report of it, made on this very PC, says so, and does not hold the name
        // of whoever runs the tests. (A name that is part of the placeholder itself proves
        // nothing.)
        let report = crate::report::render(&facts, &scrub, &Head::Check);
        assert!(report.starts_with(&format!("Maple Fast Patch 검사 {} · ", env!("CARGO_PKG_VERSION"))), "{report}");
        assert!(report.lines().count() >= 40, "{} lines", report.lines().count());
        assert_eq!(said(&report, "검사 기준").as_deref(), Some("파일 크기 비교"), "{report}");
        assert!(said(&report, "검사 대상 버전").is_some_and(|said| said.ends_with(" · 일치 버전 없음")), "{report}");
        assert_eq!(said(&report, "최신 게임 버전").as_deref(), Some("미확인"), "{report}");
        assert_eq!(said(&report, "설치 폴더 쓰기").as_deref(), Some("성공"), "{report}");
        assert_eq!(said(&report, "잔여 파일").as_deref(), Some("없음"), "{report}");
        assert!(said(&report, "검사 소요 시간").is_some() && !report.contains("쓰기 검사"), "{report}");
        if let Ok(user) = std::env::var("USERNAME")
            && user.chars().count() >= 3
            && !"%userprofile%".contains(&user.to_lowercase())
        {
            assert!(!report.to_lowercase().contains(&user.to_lowercase()), "the user's name is in the report");
        }
    }

    #[test]
    fn steps_that_were_agreed_to_are_taken_and_leave_the_folder_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        let options = Options {
            checksums: Choice::Yes,
            full: Choice::Yes,
            write_probe: Choice::Yes,
            write_limit: Choice::Yes,
            ..offline(dir.path())
        };
        let mut desk = Scripted::default();
        let (facts, scrub) = run(&options, &mut desk, &AtomicBool::new(false));

        // The probe wrote its four files into the install folder, with a reading after
        // each, and nowhere else.
        let Asked::Done(landing) = &facts.landing else { panic!("the probe was not run: {:?}", facts.landing) };
        assert_eq!((landing.written, landing.error, landing.deleted), (4 * MIB, None, true));
        assert_eq!((landing.steps.len(), landing.place), (4, Place::Install));
        assert!(facts.other_landings.is_empty());
        // The write limit test wrote its three files, noted what was free after each, and
        // took them away again.
        let WriteLimit::Done(limit) = &facts.limit else { panic!("the limit test was not run: {:?}", facts.limit) };
        assert_eq!((limit.written, limit.wanted, limit.error, limit.deleted), (6 * MIB, 6 * MIB, None, true));
        assert_eq!(limit.steps.iter().map(|step| step.bytes).collect::<Vec<_>>(), [2 * MIB; 3]);
        assert!(limit.steps.iter().all(|step| step.free.len() == limit.volumes.len() && step.memory.is_some()));
        assert_eq!(
            facts.wrote,
            [
                Wrote::EmptyFile { removed: true },
                Wrote::ProbeFile { bytes: 4 * MIB, removed: true },
                Wrote::LimitFiles { bytes: 6 * MIB, removed: true }
            ]
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
        // Six megabytes are not what a main patch writes: the report says what was written
        // where, and the verdict does not go by it.
        let report = crate::report::render(&facts, &scrub, &Head::Check);
        let volume = &facts.game_drive.as_ref().unwrap().letter;
        assert!(
            said(&report, &format!("{volume} 쓰기 검사")).is_some_and(|said| said.starts_with("4 MB · ")),
            "{report}"
        );
        let limit = said(&report, "한도 검사").expect("the limit test has no row");
        assert!(limit.starts_with(&format!("{volume} 6 MB · ")) && !report.contains("에서 중단"), "{report}");
        assert_eq!(
            said(&report, &format!("{volume} 설치 폴더")).as_deref(),
            Some("임시 파일 4 MB / 6 MB 생성 후 삭제")
        );
        assert!(said(&report, "메모리 변화").is_some() && said(&report, "검사 기준").is_some(), "{report}");
        let evidence = Evidence::of(&facts, &scrub);
        assert_eq!(evidence.limit, Some(crate::verdict::Limit::Passed { written: 6 * MIB }));
        let without = Evidence { limit: None, limit_went: None, ..evidence.clone() };
        let (with, without) = (verdict(&evidence), verdict(&without));
        assert_eq!((with.kind, with.patch, with.kept), (without.kind, without.patch, without.kept));
        assert_eq!(with.plain(), without.plain());
        // The checksum comparison read the small files and found them to differ. The full
        // one read what that had told by length alone.
        let quick = facts.quick.as_ref().expect("the checksums were not compared");
        assert!(!quick.by_length && quick.at.is_none() && quick.tally.same == 0);
        assert!(quick.tally.differs.len() > 1000, "{}", quick.tally.differs.len());
        let Asked::Done(full) = &facts.full else { panic!("the full comparison was not run: {:?}", facts.full) };
        assert!(
            full.tally.differs.len() > 1000 && full.tally.same_length == 0 && !full.cut_short,
            "{:?}",
            full.tally.same_length
        );
        assert!(full.bytes_read > quick.bytes_read);
        assert!(desk.asked.is_empty(), "{:?}", desk.asked);
        // The time all of it took holds the time of the part that asks nothing.
        assert!(facts.took.1 > facts.took.0, "{:?}", facts.took);
    }

    #[test]
    fn write_limit_test_is_refused_where_the_pc_has_no_room_or_no_memory_for_it() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        // More than any drive has free. The answer would have been yes.
        let writes = u64::MAX / 4;
        let options = Options { write_limit: Choice::Ask, limit_bytes: writes, ..offline(dir.path()) };
        let mut desk = Scripted { yes: true, ..Scripted::default() };
        let (facts, scrub) = run(&options, &mut desk, &AtomicBool::new(false));
        let WriteLimit::NoRoom { volume, free: Some(free), writes: refused, floor } = &facts.limit else {
            panic!("the limit test was not refused: {:?}", facts.limit)
        };
        assert_eq!(volume, &facts.game_drive.as_ref().unwrap().letter);
        assert!(*free > 0 && *refused == writes && *floor == write::LIMIT_FLOOR);
        // It was not even asked about, and wrote nothing.
        assert!(!desk.asked.iter().any(|question| question.starts_with("한도 검사")), "{:?}", desk.asked);
        assert!(!facts.wrote.iter().any(|wrote| matches!(wrote, Wrote::LimitFiles { .. })));
        let report = crate::report::render(&facts, &scrub, &Head::Check);
        let refused = said(&report, "한도 검사").expect("the refused test has no row");
        assert!(refused.starts_with(&format!("미실행 ({volume} 여유 ")) && refused.contains(" / 필요 "), "{report}");

        // More memory than any PC has is to be left: refused in the same way.
        let keeps = u64::MAX / 4;
        let options = Options { write_limit: Choice::Ask, limit_memory_floor: keeps, ..offline(dir.path()) };
        let mut desk = Scripted { yes: true, ..Scripted::default() };
        let (facts, scrub) = run(&options, &mut desk, &AtomicBool::new(false));
        let WriteLimit::NoMemory { available: Some(available), wanted } = facts.limit else {
            panic!("the limit test was not refused: {:?}", facts.limit)
        };
        assert!(available > 0 && wanted == keeps + 4 * MIB);
        assert!(!desk.asked.iter().any(|question| question.starts_with("한도 검사")), "{:?}", desk.asked);
        let report = crate::report::render(&facts, &scrub, &Head::Check);
        assert!(said(&report, "한도 검사").is_some_and(|said| said.starts_with("미실행 (메모리 ")), "{report}");

        // Told beforehand not to run it, it is left out without a word about room.
        let options = Options { write_limit: Choice::No, limit_bytes: writes, ..offline(dir.path()) };
        let (facts, _) = run(&options, &mut Scripted::default(), &AtomicBool::new(false));
        assert_eq!(facts.limit, WriteLimit::Declined);
    }

    #[test]
    fn write_limit_test_is_run_on_the_games_drive_unless_another_volume_is_picked() {
        let needed = ROOM_NEEDED + write::LIMIT_FLOOR;
        let volume = |letter: &str, game: bool, free: u64| Target {
            letter: letter.to_owned(),
            game,
            free: Some(free * GIB),
            room: free * GIB >= needed,
        };
        // The seat: the game on G:, Windows on C:, and a small D:.
        let seat = [volume("G:", true, 305), volume("C:", false, 154), volume("D:", false, 32)];
        let asking = Options::default();
        let read = Some((102 * MIB) as f64);
        let nobody = Scrub::new(Vec::new(), Vec::new());
        // What the library offers for the volume that was named, and which of them the
        // console then runs it on.
        let picked = |options: &Options, volumes: &[Target], read: Option<f64>, desk: &mut Scripted| {
            let speeds = Speeds { read, write: None };
            let offer =
                limit_offer(volumes.to_vec(), options.limit_on.as_deref(), &options.sizes(), speeds, None, &nobody);
            pick_among(options, &offer, desk)
        };

        // Asked, agreed to, and the second of the volumes that are offered is picked.
        let mut desk = Scripted { yes: true, picks: 1, ..Scripted::default() };
        assert_eq!(picked(&asking, &seat, read, &mut desk), Ok(1));
        assert_eq!(desk.asked.len(), 2, "{:?}", desk.asked);
        assert!(
            desk.asked[0]
                .starts_with("한도 검사: 고정 볼륨 한 곳(다음에 선택, 기본은 게임 드라이브)에 본 패치만큼(36.5 GB)")
        );
        assert!(
            desk.asked[0]
                .contains("1 GB씩 실제로 썼다가 모두 지웁니다. 이 PC의 읽기 속도(102 MB/s)로 쓴다면 약 6분 6초.")
        );
        assert_eq!(desk.asked[0].lines().count(), 2);
        assert_eq!(desk.asked[1], "한도 검사를 할 볼륨을 선택해 주세요.");
        assert_eq!(
            desk.offered,
            [
                "G: 게임 드라이브(설치 폴더) · 여유 305.0 GB",
                "C: · 여유 154.0 GB",
                "D: · 여유 32.0 GB · 쓸 양과 남길 양보다 적음"
            ]
        );
        // The volume without the room is refused with its figures, also when picked.
        let mut desk = Scripted { yes: true, picks: 2, ..Scripted::default() };
        let no_room = WriteLimit::NoRoom {
            volume: "D:".to_owned(),
            free: Some(32 * GIB),
            writes: ROOM_NEEDED,
            floor: write::LIMIT_FLOOR,
        };
        assert_eq!(picked(&asking, &seat, read, &mut desk), Err(Box::new(no_room.clone())));
        // No is no, and nothing is offered after it.
        let mut desk = Scripted::default();
        assert_eq!(picked(&asking, &seat, read, &mut desk), Err(Box::new(WriteLimit::Declined)));
        assert!(desk.offered.is_empty() && desk.asked.len() == 1);

        // Told to run it when the program was started: the game's drive, and no question.
        let told = Options { write_limit: Choice::Yes, ..Options::default() };
        let mut desk = Scripted::default();
        assert_eq!(picked(&told, &seat, read, &mut desk), Ok(0));
        assert!(desk.asked.is_empty());
        // A volume that was named is the one, whatever its letter is written like.
        let on_c = Options { limit_on: Some("c:".to_owned()), ..Options::default() };
        let mut desk = Scripted { yes: true, ..Scripted::default() };
        assert_eq!(picked(&on_c, &seat, None, &mut desk), Ok(1));
        assert!(desk.asked[0].starts_with("한도 검사: C: 볼륨에 본 패치만큼(36.5 GB) 임시 파일을 1 GB씩 실제로"));
        assert!(desk.offered.is_empty() && !desk.asked[0].contains("읽기 속도"));
        let on_d = Options { limit_on: Some("D:".to_owned()), ..told.clone() };
        assert_eq!(picked(&on_d, &seat, read, &mut Scripted::default()), Err(Box::new(no_room.clone())));
        let on_x = Options { limit_on: Some("X:".to_owned()), ..told.clone() };
        let nowhere = WriteLimit::NoPlace { volume: "X:".to_owned(), error: None };
        assert_eq!(picked(&on_x, &seat, read, &mut Scripted::default()), Err(Box::new(nowhere.clone())));
        // What the library says of those two before anything is asked: refused, with the
        // words the report uses.
        let offer =
            limit_offer(seat.to_vec(), Some("D:"), &asking.sizes(), Speeds { read, write: None }, None, &nobody);
        assert_eq!(offer.refused, Some(Refused::Limit(Box::new(no_room))));
        assert_eq!(offer.reason, "D: 여유 32.0 GB / 필요 40.5 GB");
        let offer =
            limit_offer(seat.to_vec(), Some("X:"), &asking.sizes(), Speeds { read, write: None }, None, &nobody);
        assert_eq!(
            (offer.refused, offer.reason.as_str()),
            (Some(Refused::Limit(Box::new(nowhere))), "X: 쓰기 가능한 고정 볼륨 아님")
        );
        // Too little memory refuses it on any volume.
        let no_memory = WriteLimit::NoMemory { available: Some(5 * GIB), wanted: 6 * GIB };
        let offer = limit_offer(
            seat.to_vec(),
            None,
            &asking.sizes(),
            Speeds { read, write: None },
            Some(no_memory.clone()),
            &nobody,
        );
        assert_eq!(offer.refused, Some(Refused::Limit(Box::new(no_memory))));
        assert_eq!(offer.reason, "메모리 5.0 GB / 필요 6.0 GB");

        // A PC with one volume is asked about the install folder, and offered nothing.
        let mut desk = Scripted { yes: true, ..Scripted::default() };
        assert_eq!(picked(&asking, &seat[..1], read, &mut desk), Ok(0));
        assert!(desk.asked[0].starts_with("한도 검사: 설치 폴더에 ") && desk.offered.is_empty());
        // Where no volume has the room, nothing is asked: the game's drive is named.
        let cramped = [volume("G:", true, 30), volume("C:", false, 20)];
        let mut desk = Scripted { yes: true, ..Scripted::default() };
        assert!(matches!(
            *picked(&asking, &cramped, read, &mut desk).unwrap_err(),
            WriteLimit::NoRoom { ref volume, .. } if volume == "G:"
        ));
        assert!(desk.asked.is_empty());
        assert_eq!(picked(&asking, &[], read, &mut desk), Err(Box::new(WriteLimit::NotPossible)));
    }

    #[test]
    fn write_probe_on_another_volume_is_noted_with_where_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(false);
        let plan = Fill::probe(2 * MIB, Duration::from_millis(100));
        let root = [(dir.path().to_path_buf(), Place::Root)];
        let probed = probe_elsewhere("Q:", &root, plan, &stop, &mut |_| {});
        let landing = probed.landing.expect("the folder took no write");
        assert_eq!(
            (probed.volume.as_str(), landing.place, landing.written, landing.deleted),
            ("Q:", Place::Root, 2 * MIB, true)
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // A volume that takes no folder is noted with the error number, and not written to.
        let gone = [(dir.path().join("gone"), Place::Root)];
        assert_eq!(probe_elsewhere("Q:", &gone, plan, &stop, &mut |_| {}).landing, Err(Some(3)));
    }

    #[test]
    fn folder_that_was_not_read_is_placed_by_lengths_and_by_what_the_games_patcher_logged() {
        // Two states whose files are equally long, and one with a longer file.
        let table = Table::parse(
            "client live\nstate 900 0\nstate 900 1\nstate 900 2\n\
             Data\\Base.wz\t0:0000000a:100\n\
             Small.ini\t0:000000c0:21\t1:000000c1:21\t2:000000c2:22\n",
        )
        .unwrap();
        let sheets = table.sheets();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Data")).unwrap();
        std::fs::write(dir.path().join("Data").join("Base.wz"), [0u8; 100]).unwrap();
        std::fs::write(dir.path().join("Small.ini"), [0u8; 21]).unwrap();
        let state = |minor: u32| State { version: 900, minor };
        let walked = game::walk(dir.path(), &AtomicBool::new(false));

        // Lengths alone leave two states open.
        let folder = Folder::look(dir.path(), &sheets);
        let open = by_length(&sheets, &folder, &walked, None);
        assert!(open.by_length && open.bytes_read == 0);
        assert_eq!((open.at, &open.either[..]), (None, &[state(0), state(1)][..]));
        // The version the game's patcher logged last settles it, where it is one of them.
        let settled = by_length(&sheets, &folder, &walked, Some(state(1)));
        assert_eq!((settled.at, settled.either.len()), (Some(state(1)), 0));
        assert_eq!(settled.against.map(|(against, _)| against), Some(state(1)));
        assert_eq!((settled.tally.same_length, settled.tally.against(), settled.not_in_table), (2, 0, 0));
        // A log that names a state the lengths speak against is not gone by.
        assert_eq!(by_length(&sheets, &folder, &walked, Some(state(2))).at, None);

        // One state fits: that is it, whatever the log says.
        std::fs::write(dir.path().join("Small.ini"), [0u8; 22]).unwrap();
        let folder = Folder::look(dir.path(), &sheets);
        assert_eq!(by_length(&sheets, &folder, &walked, Some(state(0))).at, Some(state(2)));
        // A file of a length no state names: nothing fits, and the nearest state is held
        // against.
        std::fs::write(dir.path().join("Small.ini"), [0u8; 23]).unwrap();
        let folder = Folder::look(dir.path(), &sheets);
        let wrong = by_length(&sheets, &folder, &walked, Some(state(2)));
        assert_eq!((wrong.at, wrong.either.len(), wrong.tally.differs.len()), (None, 0, 1));

        assert_eq!(State::logged("00421_04"), Some(State { version: 421, minor: 4 }));
        assert_eq!((State::logged("00421"), State::logged("v421_04")), (None, None));
    }

    #[test]
    fn what_a_step_costs_is_worked_out_from_how_fast_the_drive_reads() {
        // The seat that reads its game over the network at 102 MB/s.
        let read = Some((102 * MIB) as f64);
        let said = |bytes: u64, written: bool| about(bytes, read, written).1;
        assert_eq!(said(5_800 * MIB, false), "이 PC의 읽기 속도(102 MB/s)로 약 57초.");
        assert_eq!(said(633 * GIB / 10, false), "이 PC의 읽기 속도(102 MB/s)로 약 10분 35초.");
        assert_eq!(said(3 * GIB, true), "이 PC의 읽기 속도(102 MB/s)로 쓴다면 약 30초.");
        assert_eq!(said(ROOM_NEEDED, true), "이 PC의 읽기 속도(102 MB/s)로 쓴다면 약 6분 6초.");
        // The same as a figure, for whoever shows it in a window.
        assert!(about(3 * GIB, read, true).0.is_some_and(|seconds| (30.0..30.2).contains(&seconds)));
        // Nothing is said where the drive was not timed.
        assert_eq!(about(GIB, None, false), (None, String::new()));
        assert_eq!(about(GIB, Some(0.0), true), (None, String::new()));
    }

    #[test]
    fn nothing_is_written_when_the_write_test_is_left_out_and_the_probe_declined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Data")).unwrap();
        std::fs::write(dir.path().join("MapleStoryT.exe"), b"the test world").unwrap();
        // A file an earlier run left is taken away, and that is said.
        std::fs::write(dir.path().join(".mfp-check-write-probe-1.tmp"), b"left").unwrap();
        let options = Options { write_test: false, heavy_reads: Choice::No, ..offline(dir.path()) };
        let (facts, _) = run(&options, &mut Scripted::default(), &AtomicBool::new(false));
        assert_eq!(facts.client, Server::Test);
        assert_eq!(facts.write_test, WriteTest::Skipped);
        assert_eq!(facts.wrote, [Wrote::Leftovers { removed: 1, left: 0 }]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn stop_ends_the_run_before_anything_is_read_or_asked() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        let mut desk = Scripted { yes: true, ..Scripted::default() };
        let (facts, _) = run(&offline(dir.path()), &mut desk, &AtomicBool::new(true));
        assert!(facts.quick.is_err() && facts.reads.is_err());
        assert_eq!((&facts.full, &facts.landing), (&Asked::NotPossible, &Asked::NotPossible));
        assert_eq!(facts.limit, WriteLimit::NotPossible);
        assert!(desk.asked.is_empty(), "{:?}", desk.asked);
    }

    /// The request an app makes for a folder, sized for a test: nothing asked of the
    /// server, small writes, and no other volume than the folder's written to.
    fn as_an_app(folder: &Path) -> Request {
        let options = offline(folder);
        // Whether a game happens to run on the PC the tests run on is not theirs to go by.
        Request {
            online: false,
            reads_while_running: true,
            sizes: options.sizes(),
            ..Request::of(folder, Server::Live)
        }
    }

    /// Runs a step and notes every word of its progress.
    fn stepped(diagnosis: &Diagnosis, step: Extra) -> (Diagnosis, Vec<Progress>) {
        let mut heard = Vec::new();
        let next = diagnosis.run(step, None, &AtomicBool::new(false), &mut |progress| heard.push(progress));
        (next, heard)
    }

    /// Progress never goes back, never passes the whole, and ends at the whole.
    fn assert_moves_forward(heard: &[Progress], what: &str) {
        assert!(!heard.is_empty(), "{what}: nothing was heard");
        assert!(heard.windows(2).all(|pair| pair[0].done <= pair[1].done), "{what}: {heard:?}");
        assert!(heard.iter().all(|progress| progress.done <= progress.total), "{what}: {heard:?}");
        let last = heard.last().unwrap();
        assert!(last.total > 0 && last.done == last.total, "{what}: ended at {last:?}");
    }

    #[test]
    fn diagnosis_is_a_value_that_can_be_kept_and_handed_to_another_thread() {
        fn kept_and_shared<T: Clone + Send + Sync + 'static>() {}
        kept_and_shared::<Diagnosis>();
        kept_and_shared::<Offer>();
        kept_and_shared::<Request>();

        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        let mut heard = Vec::new();
        let diagnosis =
            Diagnosis::start(&as_an_app(dir.path()), &AtomicBool::new(false), &mut |progress| heard.push(progress));
        // Its five parts, in their order, and no step that reads or writes.
        assert_moves_forward(&heard, "the part that asks nothing");
        assert_eq!((heard[0].stage, heard[0].done, heard.last().unwrap().total), (Stage::Pc, 0, 5));
        let stages: Vec<Stage> = heard.iter().map(|progress| progress.stage).collect();
        assert!(stages.contains(&Stage::Install) && stages.contains(&Stage::Sizes) && !stages.contains(&Stage::Server));
        assert!(!diagnosis.stopped() && !diagnosis.left_behind());
        assert!(Extra::ALL.iter().all(|step| !diagnosis.ran(*step)));
        // The steps that were not run are in the facts as the report and the verdict
        // know them: left out.
        let facts = diagnosis.facts();
        assert_eq!((&facts.quick, &facts.full), (&Err(Unknown::Skipped), &Asked::Declined));
        assert_eq!((&facts.landing, &facts.limit), (&Asked::Declined, &WriteLimit::Declined));
        // The report is what the console prints, headed as the console's is, and has no
        // row of a step that was not run.
        let report = diagnosis.report();
        assert_eq!(report, crate::report::render(diagnosis.facts(), diagnosis.scrub(), &Head::Check));
        assert!(report.starts_with("Maple Fast Patch 검사 "), "{report}");
        assert_eq!(said(&report, "검사 기준").as_deref(), Some("파일 크기 비교"), "{report}");
        assert!(
            !report.contains("쓰기 검사") && !report.contains("한도 검사") && !report.contains("검사 시간"),
            "{report}"
        );
        // An app that says so has its own name and version in that first line, and
        // nothing else of the report is another.
        let mut apps = diagnosis.clone();
        apps.request = Arc::new(Request { head: Head::App("1.2.3".to_owned()), ..(*diagnosis.request).clone() });
        let (first, rest) = report.split_once('\n').unwrap();
        assert!(first.starts_with(&format!("Maple Fast Patch 검사 {} · ", env!("CARGO_PKG_VERSION"))), "{first}");
        let when = first.rsplit_once(" · ").unwrap().1;
        assert_eq!(apps.report(), format!("Maple Fast Patch 1.2.3 · {when}\n{rest}"));
        // Another thread reads the same value.
        let shared = diagnosis.clone();
        let theirs = std::thread::spawn(move || shared.report()).join().unwrap();
        assert_eq!(theirs, report);
    }

    #[test]
    fn caller_that_knows_the_folder_and_the_server_has_nothing_written_and_nothing_asked() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        let newest = Table::built_in(Server::Live).newest();
        let known = OnShelf {
            newest,
            patch: Some(PatchOnShelf { file_name: "03to04.patch".to_owned(), size: 163_520_879, originals: Some(0) }),
            patch_is_next_main: false,
        };
        // The server may be asked, and is not: the caller said what it has.
        let request =
            Request { online: true, shelf: Some(known), writable: Some(WriteTest::Passed), ..as_an_app(dir.path()) };
        let diagnosis = Diagnosis::start(&request, &AtomicBool::new(false), &mut |_| {});
        let facts = diagnosis.facts();
        assert_eq!((facts.traffic.requests, facts.traffic.received), (0, 0));
        assert!(facts.wrote.is_empty(), "{:?}", facts.wrote);
        assert_eq!(facts.write_test, WriteTest::Passed);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
        let shelf = facts.shelf.as_ref().expect("what the caller knows of the server was not taken");
        assert_eq!((shelf.newest, &shelf.sample), (newest, &Err(Unknown::Skipped)));
        // Nor was the line timed: the caller did not ask for that.
        assert!(!request.time_the_line && shelf.round_trip == Err(Unknown::Skipped));
        // The report says what the caller knew, and of what was not measured that it was not.
        let report = diagnosis.report();
        assert_eq!(said(&report, "최신 게임 버전"), Some(newest.label(&mfp_kms::LIVE)), "{report}");
        assert_eq!(said(&report, "다운로드 속도").as_deref(), Some("미측정"), "{report}");
        assert_eq!(said(&report, "응답 지연 시간").as_deref(), Some("미측정"), "{report}");
        assert_eq!(said(&report, "설치 폴더 쓰기").as_deref(), Some("성공"), "{report}");
        assert_eq!(said(&report, "잔여 파일").as_deref(), Some("없음"), "{report}");
        // The verdict goes by both: the files differ from the newest version the caller
        // named, which the table knows, so they are not Nexon's, and that is said.
        let verdict = diagnosis.verdict();
        assert_eq!(verdict.patch, Outlook::Unlikely);
        assert!(verdict.patch_grounds.iter().any(|fact| fact.ends_with("크기 변경")), "{:?}", verdict.patch_grounds);
        assert!(said(&report, "패치 가능 여부").is_some_and(|said| said.starts_with("불가능 (게임 파일")), "{report}");
        // Without them the verdict says what it lacks: no test of the folder, and no
        // word from the server on whether the files are of a newer version.
        let request = Request { write_test: false, ..as_an_app(dir.path()) };
        let bare = Diagnosis::start(&request, &AtomicBool::new(false), &mut |_| {});
        assert_eq!((bare.facts().write_test, bare.facts().traffic.requests), (WriteTest::Skipped, 0));
        assert!(bare.facts().wrote.is_empty());
        assert_eq!(bare.verdict().patch, Outlook::Unknown);
        let report = bare.report();
        assert_eq!(said(&report, "설치 폴더 쓰기").as_deref(), Some("미실행"), "{report}");
        assert_eq!(said(&report, "최신 게임 버전").as_deref(), Some("미확인"), "{report}");
        assert_eq!(said(&report, "임시 파일 삭제"), None, "{report}");
    }

    #[test]
    fn what_the_waiting_patch_needs_is_handed_in_without_looking_at_the_pc_again() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Data")).unwrap();
        std::fs::write(dir.path().join("MapleStory.exe"), b"the game").unwrap();
        let diagnosis = Diagnosis::start(&as_an_app(dir.path()), &AtomicBool::new(false), &mut |_| {});
        assert_eq!(diagnosis.facts().room, None);
        // More than any drive has: the engine's figures decide what a player is told of
        // the room, where nothing else stops the patch first.
        let room = Room { safe: u64::MAX / 2, immediate: u64::MAX / 4 };
        let told = diagnosis.with_room(Some(room));
        assert_eq!((told.facts().room, diagnosis.facts().room), (Some(room), None));
        // Nothing was looked at again: what the report says of the PC, of its drives and of
        // the game is the same, and so is the careful answer. Its summary holds the drive
        // against the engine's figure now.
        let found = |report: String| {
            let (from, to) = (report.find("[PC 환경]").unwrap(), report.find("[환경 분석]").unwrap());
            report[from..to].to_owned()
        };
        assert_eq!(found(told.report()), found(diagnosis.report()));
        let needs = |report: &str| said(report, "패치 공간").map(|said| said[said.find("필요 ").unwrap()..].to_owned());
        assert_eq!(needs(&diagnosis.report()).as_deref(), Some("필요 36.5 GB)"));
        assert_eq!(needs(&told.report()), Some(format!("필요 {})", gb(room.safe))));
        assert_eq!((told.verdict().patch, told.verdict().kind), (diagnosis.verdict().patch, diagnosis.verdict().kind));
        assert_eq!(told.with_room(None).verdict(), diagnosis.verdict());
        // Handed in with the request, it is there from the start.
        let request = Request { room: Some(room), ..as_an_app(dir.path()) };
        let from_the_start = Diagnosis::start(&request, &AtomicBool::new(false), &mut |_| {});
        assert_eq!(from_the_start.facts().room, Some(room));
    }

    #[test]
    fn stopped_diagnosis_comes_back_at_once_with_what_it_has_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        let mut heard = Vec::new();
        let began = Instant::now();
        let diagnosis =
            Diagnosis::start(&as_an_app(dir.path()), &AtomicBool::new(true), &mut |progress| heard.push(progress));
        assert!(diagnosis.stopped() && began.elapsed() < Duration::from_secs(5), "{:?}", began.elapsed());
        let facts = diagnosis.facts();
        assert!(facts.wrote.is_empty() && facts.reads.is_err() && facts.quick.is_err());
        assert_eq!(
            (&facts.full, &facts.landing, &facts.limit),
            (&Asked::NotPossible, &Asked::NotPossible, &WriteLimit::NotPossible)
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
        // It did not get to its end, and did not say that it had.
        assert!(heard.last().is_none_or(|last| last.done < last.total), "{heard:?}");
        // A step on a diagnosis, with the stop set, does nothing at all.
        let whole = Diagnosis::start(&as_an_app(dir.path()), &AtomicBool::new(false), &mut |_| {});
        let stopped = whole.run(Extra::WriteProbe, None, &AtomicBool::new(true), &mut |_| panic!("it began"));
        assert!(stopped.stopped() && !stopped.ran(Extra::WriteProbe) && stopped.facts().wrote == whole.facts().wrote);
    }

    #[test]
    fn steps_tell_how_far_they_are_in_bytes_that_never_go_back() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        let start = Diagnosis::start(&as_an_app(dir.path()), &AtomicBool::new(false), &mut |_| {});

        // The small files, by as many bytes as the offer said there were to read.
        let offer = start.offer(Extra::Checksums, None);
        let (read, heard) = stepped(&start, Extra::Checksums);
        assert_moves_forward(&heard, "checksums");
        assert!(heard.iter().all(|progress| progress.stage == Stage::Checksums));
        assert_eq!(heard.last().unwrap().total, offer.bytes);
        assert!(read.ran(Extra::Checksums) && !read.ran(Extra::Full) && !start.ran(Extra::Checksums));
        // The rest: the one long file.
        let offer = read.offer(Extra::Full, None);
        assert_eq!((offer.files, offer.bytes, offer.refused.clone()), (1, QUICK_LIMIT + 1, None));
        let (all, heard) = stepped(&read, Extra::Full);
        assert_moves_forward(&heard, "the full comparison");
        assert_eq!(heard.last().unwrap().total, QUICK_LIMIT + 1);
        assert!(heard.len() > 8, "a long file is told of piece by piece: {} times", heard.len());
        assert!(all.ran(Extra::Full));
        // Nothing is left to read: both are refused, and refused steps change nothing.
        for step in [Extra::Checksums, Extra::Full] {
            let offer = all.offer(step, None);
            assert_eq!((offer.refused, offer.reason.as_str()), (Some(Refused::NothingToRead), "읽지 않은 파일 없음"));
            let (same, heard) = stepped(&all, step);
            assert!(heard.is_empty() && same.report() == all.report());
        }

        // The write probe and the write limit test, by the bytes they write.
        let (probed, heard) = stepped(&all, Extra::WriteProbe);
        assert_moves_forward(&heard, "the write probe");
        assert_eq!((heard.last().unwrap().total, heard[0].stage), (4 * MIB, Stage::WriteProbe));
        let (limited, heard) = stepped(&probed, Extra::WriteLimit);
        assert_moves_forward(&heard, "the write limit test");
        assert_eq!((heard.last().unwrap().total, heard[0].stage), (6 * MIB, Stage::WriteLimit));
        assert!(Extra::ALL.iter().all(|step| limited.ran(*step)) && !limited.left_behind());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
        // Each step added to what the one before it had found, and to the time it took.
        assert_eq!(limited.facts().wrote.len(), 3);
        assert!(limited.facts().took.1 > start.facts().took.1 && limited.facts().took.0 == start.facts().took.0);
        // The diagnosis the steps began from is as it was.
        assert!(start.facts().wrote.len() == 1 && !start.ran(Extra::WriteProbe));
    }

    #[test]
    fn write_step_that_is_stopped_has_removed_what_it_wrote_when_it_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        // Long enough to be stopped in the middle of a file: 256 MB in files of 64 MB.
        let sizes = Sizes {
            probe_bytes: 256 * MIB,
            limit_bytes: 256 * MIB,
            limit_step: 64 * MIB,
            ..offline(dir.path()).sizes()
        };
        let request = Request { sizes, ..as_an_app(dir.path()) };
        let start = Diagnosis::start(&request, &AtomicBool::new(false), &mut |_| {});
        for step in [Extra::WriteProbe, Extra::WriteLimit] {
            let stop = AtomicBool::new(false);
            let (mut heard, mut stopped_at) = (Vec::new(), None);
            let began = Instant::now();
            let cut = start.run(step, None, &stop, &mut |progress| {
                heard.push(progress.done);
                // In the middle of the second file.
                if progress.done >= 100 * MIB && stopped_at.is_none() {
                    stop.store(true, Ordering::Relaxed);
                    stopped_at = Some(began.elapsed());
                }
            });
            let after_stop = began.elapsed() - stopped_at.expect("the step did not get that far");
            assert!(after_stop < Duration::from_secs(2), "{step:?}: {after_stop:?} after the stop");
            assert!(cut.stopped() && !cut.left_behind(), "{step:?}");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before, "{step:?} left something");
            assert!(heard.windows(2).all(|pair| pair[0] <= pair[1]) && *heard.last().unwrap() < 256 * MIB, "{step:?}");
            // What it wrote until then is in the record, as written and removed, and the
            // report says that it was cut short. The verdict goes by none of it.
            let written = match (step, cut.facts().wrote.last()) {
                (Extra::WriteProbe, Some(Wrote::ProbeFile { bytes, removed: true })) => *bytes,
                (Extra::WriteLimit, Some(Wrote::LimitFiles { bytes, removed: true })) => *bytes,
                other => panic!("{other:?}"),
            };
            assert!((100 * MIB..256 * MIB).contains(&written), "{step:?}: {written}");
            let report = cut.report();
            assert!(report.contains("에서 중단 · 사용자 중단"), "{report}");
            assert_eq!(cut.verdict(), start.verdict(), "{step:?}");
        }
    }

    /// The question without what the step costs, which is worked out from a speed that
    /// is measured anew by every diagnosis.
    fn without_cost(question: &str) -> String {
        let Some(from) = question.find(" 이 PC의 ") else { return question.to_owned() };
        let cost = &question[from..];
        let until = cost.find("약 ").and_then(|about| cost[about..].find('.').map(|end| about + end + 1));
        format!("{}{}", &question[..from], until.map_or("", |until| &cost[until..]))
    }

    #[test]
    fn console_asks_with_the_words_of_the_offers() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        // What the console asks, with nobody saying yes.
        let mut desk = Scripted::default();
        run(&offline(dir.path()), &mut desk, &AtomicBool::new(false));
        // What the library offers for the same folder.
        let diagnosis = Diagnosis::start(&as_an_app(dir.path()), &AtomicBool::new(false), &mut |_| {});
        let offers: Vec<Offer> = Extra::ALL.iter().map(|step| diagnosis.offer(*step, None)).collect();
        assert_eq!(desk.asked.len(), 4, "{:?}", desk.asked);
        for (asked, offer) in desk.asked.iter().zip(&offers) {
            // Of the write limit test the console asks about a volume that is yet to be
            // picked, on a PC that has several: from there on the words are the same.
            let from = |question: &str| question[question.find("에 본 패치만큼").unwrap_or(0)..].to_owned();
            assert_eq!(without_cost(&from(asked)), without_cost(&from(&offer.question)), "{:?}", offer.step);
            // The question is made of the parts a window shows, and of nothing else.
            assert!(offer.question.starts_with(&offer.what) && offer.what.starts_with(offer.step.label()));
            assert!(offer.question.contains(&offer.cost) && offer.cost.starts_with("이 PC의 읽기 속도("));
            assert!(offer.warning.iter().all(|line| offer.question.contains(line)), "{offer:?}");
            assert!(
                offer.question.ends_with(" 진행하시겠습니까?") && offer.seconds.is_some_and(|seconds| seconds > 0.0)
            );
            let parts = offer.what.len() + offer.cost.len() + offer.warning.iter().map(String::len).sum::<usize>();
            assert!(offer.question.len() - parts <= " 진행하시겠습니까?".len() + 3, "{:?}", offer.question);
        }
        // The two that write say so, and what can come of the one that writes much.
        assert_eq!(offers.iter().map(|offer| offer.writes).collect::<Vec<_>>(), [false, false, true, true]);
        assert_eq!(offers[3].warning.len(), 1);
        assert!(offers[3].warning[0].starts_with("PC방 좌석에서는 ") && offers[3].question.lines().count() == 2);
        assert_eq!((offers[2].bytes, offers[3].bytes), (4 * MIB, 6 * MIB));
        assert!(offers[2].volumes.len() == 1 && offers[2].volumes[0].game && offers[3].volumes[0].game);
    }

    /// A write probe of four files of 256 MB at these megabytes a second, after each of
    /// which this many megabytes less memory were available than before.
    fn probe_at(speeds: [f64; 4], down: [u64; 4]) -> crate::facts::Landing {
        use crate::facts::{Landing, Memory, Moved, Step};
        let now = |available: u64| Some(Memory { available, cache: 0, nonpaged: 0, commit: 0 });
        let before = 46 * GIB / 10;
        let steps: Vec<Step> = speeds
            .iter()
            .zip(down)
            .map(|(speed, down)| Step {
                bytes: 256 * MIB,
                seconds: 256.0 / speed,
                free: vec![Some(2000 * GIB)],
                memory: now(before - down * MIB),
            })
            .collect();
        Landing {
            place: Place::Install,
            written: GIB,
            wanted: GIB,
            seconds: steps.iter().map(|step| step.seconds).sum(),
            error: None,
            halted: None,
            steps,
            volumes: vec![Moved {
                letter: "D:".to_owned(),
                written_to: true,
                before: Some(2001 * GIB),
                after_write: Some(2000 * GIB),
                after_delete: Some(2001 * GIB),
                later: Some(2001 * GIB),
            }],
            memory: [now(before), now(before - down[3] * MIB), now(before - down[3] * MIB), now(before)],
            waited: 3.0,
            deleted: true,
        }
    }

    /// The second seat had 4.5 GB of memory available, and the test asked for 6: it was
    /// refused, on a seat whose probe had shown that its writes do not go into memory.
    #[test]
    fn write_limit_test_asks_for_little_memory_where_the_probe_showed_writes_not_going_there() {
        use crate::facts::Adapter;
        let sizes = Sizes::default();
        let mut facts = Facts::none_yet(0);
        let wired = Adapter {
            model: "Realtek Gaming GbE Family Controller".to_owned(),
            wireless: false,
            speed: Some(1_000_000_000),
        };
        facts.adapters = Ok(vec![wired]);
        // Nothing known of where the writes go: all of what the test is sized to leave.
        let unknown = limit_plan(&sizes, &facts, None);
        assert_eq!((unknown.memory_floor, unknown.memory_to_begin()), (write::MEMORY_FLOOR, 6 * GIB));
        // Every step at what the 1 Gbps line carries, and memory where it was: the writes
        // go to the shop's server, and the test needs no more than the PC does.
        let to_the_server = probe_at([64.0, 91.0, 93.0, 94.0], [111, 0, 0, 0]);
        let small = limit_plan(&sizes, &facts, Some(&to_the_server));
        assert_eq!((small.memory_floor, small.memory_to_begin()), (write::MEMORY_OWN, 3 * GIB));
        assert!(small.memory_to_begin() < 45 * GIB / 10, "the seat had 4.5 GB");
        assert_eq!((small.floor, small.slow_below), (unknown.floor, unknown.slow_below));
        // Memory that went down with every step: it is where the writes go first, and
        // the test keeps its distance from the end of it.
        let into_memory = probe_at([400.0, 410.0, 405.0, 400.0], [260, 520, 780, 1040]);
        assert_eq!(limit_plan(&sizes, &facts, Some(&into_memory)).memory_floor, write::MEMORY_FLOOR);
        // Fast steps and memory that did not move say nothing of where the writes go.
        let unseen = probe_at([1800.0; 4], [3, 5, 4, 6]);
        assert_eq!(limit_plan(&sizes, &facts, Some(&unseen)).memory_floor, write::MEMORY_FLOOR);
        // Without a wired line there is no such speed to hold the steps against, and a
        // probe that was cut short says nothing.
        let mut cut = to_the_server.clone();
        cut.written = 300 * MIB;
        assert_eq!(limit_plan(&sizes, &facts, Some(&cut)).memory_floor, write::MEMORY_FLOOR);
        facts.adapters = Ok(Vec::new());
        assert_eq!(limit_plan(&sizes, &facts, Some(&to_the_server)).memory_floor, write::MEMORY_FLOOR);
        // A test that asks nothing of memory stays so.
        let none = Sizes { limit_memory_floor: 0, ..Sizes::default() };
        assert_eq!(limit_plan(&none, &Facts::none_yet(0), Some(&to_the_server)).memory_floor, 0);
    }

    #[test]
    fn write_limit_test_costs_what_the_probe_wrote_at_where_that_is_known() {
        // The seat: read at 106 MB/s, and the probe's steps settled at 106 MB/s too.
        let both = Speeds { read: Some((106 * MIB) as f64), write: Some((106 * MIB) as f64) };
        let (seconds, cost) = both.cost_of_writing(ROOM_NEEDED);
        assert!(seconds.is_some_and(|seconds| (352.0..353.0).contains(&seconds)));
        assert_eq!(cost, "이 PC의 쓰기 속도(106 MB/s)로 약 5분 53초.");
        // Without a probe it is a guess from how fast the drive reads, and said as one.
        let read_only = Speeds { write: None, ..both };
        assert_eq!(read_only.cost_of_writing(ROOM_NEEDED).1, "이 PC의 읽기 속도(106 MB/s)로 쓴다면 약 5분 53초.");
        assert_eq!(Speeds::default().cost_of_writing(ROOM_NEEDED), (None, String::new()));
        // The question says it in the same place.
        let nobody = Scrub::new(Vec::new(), Vec::new());
        let seat = vec![Target { letter: "G:".to_owned(), game: true, free: Some(305 * GIB), room: true }];
        let offer = limit_offer(seat, None, &Sizes::default(), both, None, &nobody);
        assert!(
            offer.question.starts_with("한도 검사: 설치 폴더에 본 패치만큼(36.5 GB) 임시 파일을 1 GB씩 실제로 썼다가 모두 지웁니다. 이 PC의 쓰기 속도(106 MB/s)로 약 5분 53초.\nPC방 좌석에서는 "),
            "{}",
            offer.question
        );
    }

    #[test]
    fn app_gets_a_verdict_for_a_folder_without_anything_being_written_or_read() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        let verdict = quick_verdict(dir.path(), WriteTest::Passed);
        // Whatever kind of PC the tests run on, each answer comes with its one reason, and
        // the one about a restart with what it rests on.
        assert!(verdict.plain().iter().all(|row| !row.answer.is_empty() && !row.reason.is_empty()), "{verdict:?}");
        assert!(!verdict.kept_grounds.is_empty(), "{verdict:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
        // A folder that takes no writes is said to take no patch.
        assert_eq!(quick_verdict(dir.path(), WriteTest::Failed(Some(5))).patch, Outlook::Unlikely);
    }
}
