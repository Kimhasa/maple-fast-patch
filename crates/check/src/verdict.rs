//! Says what kind of PC this looks like and whether a patch ahead of time is expected to
//! work on it, each with the facts it rests on.
//!
//! The rules are one function without side effects, [`verdict`], so that every one of them
//! can be tried on made-up facts. They are cautious on purpose: a verdict names a kind only
//! when the facts point one way, and says that it cannot tell when they do not.
//!
//! What decides is where the game's files are kept. On a PC with a disk of its own, what a
//! patch writes stays. On a PC room seat without one, Windows and the game are read from a
//! server over the network, everything the seat writes goes to a hidden space of limited
//! size (its memory, or a disk in the seat that is kept for it) and is gone at the next
//! start; a main patch writes about 34 GB, which mostly does not fit there, and how large
//! the space is cannot be seen from the seat. On a seat with a disk and a program that puts
//! the disk back at every start, the patch works and whether it stays depends on a setting
//! the seat cannot see.
//!
//! How a disk says it is attached does not make it a disk of the PC. One seat's server
//! disks were seen to call themselves Fibre Channel disks, and another's NVMe disks that
//! answer about seeking as an NVMe disk does. A drive is taken for a disk of the PC only
//! when it is attached the way such disks are (NVMe, SATA, ATA, a RAID controller, or USB
//! and memory cards), answers what Windows asks a disk about seeking, has nothing in its
//! name that marks it as made up, and no sign about a drive speaks against it.
//!
//! Signs that the files come from another computer. Each is a fact in the verdict's
//! grounds. The first five are about a drive, the last four about the PC as a whole:
//!
//! - the disk's name marks it as made up (a maker of such disks, a Windows release in the
//!   name of an image, `virtual` and the like);
//! - the disk is a virtual one by its bus;
//! - the game's files are read at the speed of the wired line, the second time no faster,
//!   and a read at a far place takes what a trip over the line takes, or is at least no
//!   hard disk's while the long reads are slower than any disk without moving parts
//!   reads (a hard disk reads at such speeds too, and these reads go past the file cache,
//!   so the far read is what tells the two apart);
//! - the same bytes are read several times faster the second time, past Windows' own cache,
//!   and bytes that were not read before are not as fast as that;
//! - the write probe wrote far faster than the drive is read: what was written did not go
//!   where the reads come from;
//! - the port driver of Windows' iSCSI initiator is loaded;
//! - the driver of a product that hands a seat its disks over the network is loaded;
//! - a PC room's program runs;
//! - storage drivers for server adapters are loaded, two or more.
//!
//! The kind of PC, by the first of these that holds:
//!
//! 1. A program that serves seats without a disk runs: a seat without a disk.
//! 2. Windows or the game is on a disk attached by a storage network (iSCSI, Fibre
//!    Channel): a seat without a disk.
//! 3. Both are on disks of the PC, no sign is about a drive, and a restore program runs: a
//!    disk with a restore program.
//! 4. What the write probe wrote took room on another drive than the one it was written
//!    to: a seat without a disk.
//! 5. A sign about a drive, and one more sign of any kind: a seat without a disk.
//! 6. Both are on disks of the PC and there is no sign at all, nor a name that hints at a
//!    PC room: an ordinary PC.
//! 7. Anything else: not told.
//!
//! Whether a main patch goes through, by the first of these that holds:
//!
//! 1. No install folder: not told.
//! 2. The folder takes no writes, or its files are known not to be Nexon's: unlikely.
//! 3. The write limit test was run in the install folder, which writes as much as a main
//!    patch does: expected where it wrote all of it, with where its writes were seen to go
//!    ([`went`]); unlikely where it stopped on an error, or ended itself because the drive,
//!    another volume or memory went down with what it wrote and was nearly used up. This
//!    holds for any kind of PC: it is the one thing that was tried and not reasoned. A test
//!    that ended itself because the writing got far slower than the line, that the user
//!    ended, or that was run on another volume, decides nothing; how far the first got is
//!    among the grounds. Nothing in a verdict says that this test should be run.
//! 4. A seat without a disk: not told, with what the write probe showed ([`keeps`]) among
//!    the grounds. The probe writes a gigabyte, and that tells where a seat's writes go
//!    first, not how many of them it takes: a seat may take them into memory, or onto a
//!    small disk of its own, and pass them on to its shop's server when that is full. So
//!    where memory went down with the writes, or another volume did, what was available
//!    there is stated as the figure it is, and is no limit. Where the probe's steps
//!    settled at what the wired line carries and memory did not go with them, the writes
//!    are carried to the shop's server: the seat's memory is not the limit, and what room
//!    the server has cannot be seen. Where nothing at all shows, the limit cannot be seen
//!    from the seat. What is free on the game's drive is never gone by: it is the
//!    server's figure.
//! 5. The room on the game's drive cannot be read: not told. Too little room: unlikely.
//! 6. A sign about a drive, or a drive that is not known to be a disk of the PC: not told.
//! 7. The game is on a disk of the PC and the folder took the empty file: expected.
//! 8. What was expected is not told where the files fit no known version and may be of a
//!    version this program does not know.
//!
//! The verdict stands on what a run finds that asks nothing. What such a run did not look
//! at is not made more of than it is: files that were held against the table by their
//! lengths and the game's logged version are not said to be sound.
//!
//! Whether the patch stays after a restart follows the kind: expected on an ordinary PC,
//! not expected on a seat without a disk, and not told for the other two.
//!
//! Each answer comes with its grounds ([`Grounds`]): the few facts it was decided by, as
//! short phrases the report prints. They state what was read off the PC and nothing else:
//! no second answer, and nothing about what is expected.

use crate::facts::{
    Asked, Bus, Comparison, DriveKind, Facts, Halt, Landing, Place, Programs, Reads, Room, Unknown, Volume, WriteLimit,
    WriteTest,
};
use crate::privacy::Scrub;
use crate::units::{count, gb, size};

/// Bytes of new files a main patch writes: 34.5 GiB for the patch to 1.2.421.
pub const MAIN_PATCH_WRITES: u64 = 69 << 29;
/// Room the game's drive needs for a main patch the safe way: the new files, the patch file
/// itself (about 1 GiB), and the 1 GiB the engine keeps free.
pub const ROOM_NEEDED: u64 = MAIN_PATCH_WRITES + (2 << 30);

/// A second read this many times faster than the first means something below Windows kept
/// the bytes. A disk of the PC itself gives about one; this is far enough from that to be
/// no accident of timing.
pub const KEPT_BELOW: f64 = 3.0;
/// Reads come at the speed of the line when both passes lie between these shares of what
/// the wired link carries. A seat was measured at 0.86 and 0.88 of a 1 Gbps link; a server
/// with other seats to feed gives less, and nothing gives more than the link.
pub const LINE_SHARE: (f64, f64) = (0.5, 1.05);
/// The milliseconds a read at a far place takes when it is a trip over a local network: a
/// seat was measured at 0.4. A disk of the PC is outside it on either side: an SSD answers
/// in 0.2 or less, a hard disk in 8 or more.
pub const LINE_MS: (f64, f64) = (0.25, 3.0);
/// Writes go at the speed of the line when a step is written at between these shares of
/// what the wired link carries. A seat that passes its writes on to its shop's server was
/// measured at 0.88 to 1.04 of a 1 Gbps link, step after step.
pub const LINE_WRITES: (f64, f64) = (0.5, 1.1);
/// The milliseconds from which a read at a far place is a hard disk's: its arm has to
/// move. Under this, the disk is no hard disk.
pub const HARD_DISK_MS: f64 = 5.0;
/// Bytes a second under which no disk without moving parts reads a long file: 200 MB/s. A
/// SATA disk reads at 400 to 550, an NVMe disk at thousands. A drive that answers a far
/// read at once, as such a disk does, and still reads a long file at less than this, twice
/// over, at just what the line carries, is not handing over what it holds itself.
pub const NO_DISK_THIS_SLOW: f64 = (200u64 << 20) as f64;
/// A write this many times faster than the same drive reads did not go where the reads
/// come from. A seat was measured at 4.
pub const WRITES_FASTER: f64 = 2.0;
/// Memory goes with the writes "about one for one" when what is gone from it lies between
/// these shares of what was written. A seat lost 808 MB for 512 MB written, which is 1.6:
/// what keeps the writes needs some memory of its own besides.
pub const ONE_FOR_ONE: (f64, f64) = (0.5, 2.5);
/// From this many steps on, a probe tells memory that goes down with the writing from
/// memory that moved for another reason: it has to go down so after three steps in four.
/// With fewer steps there is one reading, and one reading may be an accident.
pub const STEADY_STEPS: usize = 3;
/// Storage drivers for server adapters say something from this many on: one may be there
/// for an adapter the PC really has.
pub const SPARE_DRIVERS: usize = 2;

/// How a drive is attached, as far as it matters for where writes end up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attached {
    /// A disk in or at the PC itself.
    Disk,
    /// A disk another computer hands over a storage network.
    Network,
    /// A disk that is really a file or a driver's making. A seat without a disk can look
    /// like this, and so can a virtual disk somebody mounted at home.
    Virtual,
    /// A folder another computer shares.
    Share,
    /// Nothing says whose disk it is: Windows names a way of attaching that disks of a PC
    /// do not use or names none, the disk does not answer what a disk answers, or its name
    /// marks it as made up.
    Unclear,
}

/// Words in the name a disk gives itself that mark it as made up, whatever bus it claims.
const MADE_UP: [&str; 11] =
    ["virtual", "vdisk", "iscsi", "diskless", "nohard", "netdisk", "ccboot", "vhd", "ramdisk", "logical", "nxddisk"];
/// Makers whose disks are a server's: the name a disk gives itself begins with the maker.
/// `NSM` named both disks of a PC room seat, `NSM 22H2-H610 232GB` and
/// `NSM Logical_G 3722GB`. `NetZone NxDDisk` named all three of another seat's, which
/// said of themselves that they were NVMe disks and answered about seeking as one does.
const SERVER_MAKERS: [(&str, &str); 2] = [("nsm", "NSM"), ("netzone", "NetZone")];

/// What in the name a disk gives itself marks it as no disk of the PC, if anything does:
/// the maker, the word, or the Windows release an image is named after.
pub fn mark_in(model: &str) -> Option<String> {
    let lower = model.to_lowercase();
    let maker = lower.split_whitespace().next().unwrap_or_default();
    if let Some((_, written)) = SERVER_MAKERS.iter().find(|(known, _)| *known == maker) {
        return Some((*written).to_owned());
    }
    if let Some(word) = MADE_UP.iter().find(|word| lower.contains(*word)) {
        return Some((*word).to_owned());
    }
    // `22H2`, `23H2`: a release of Windows. No disk is called after one; an image of a
    // Windows installation is.
    let release = |part: &&str| matches!(part.as_bytes(), [year @ .., b'h', b'1' | b'2'] if year.len() == 2 && year.iter().all(u8::is_ascii_digit));
    lower.split(|c: char| !c.is_ascii_alphanumeric()).find(release).map(str::to_uppercase)
}

/// Sorts a drive by how it is attached. `answers_seek` says whether the disk answered what
/// Windows asks a disk about seeking; a disk of a PC does, and what a driver makes up
/// often does not.
pub fn attached(kind: DriveKind, bus: Option<Bus>, model: &str, answers_seek: bool) -> Attached {
    match kind {
        DriveKind::Remote => return Attached::Share,
        DriveKind::RamDisk | DriveKind::Disc | DriveKind::Other => return Attached::Unclear,
        DriveKind::Fixed | DriveKind::Removable => {}
    }
    let marked = mark_in(model).is_some();
    match bus.map(|bus| bus.0) {
        // Fibre Channel and iSCSI are how a disk comes from a storage server.
        Some(6 | 9) => Attached::Network,
        Some(14 | 15) => Attached::Virtual,
        _ if marked => Attached::Unclear,
        // ATA, SATA and NVMe are how a PC's own disks are attached, and a RAID controller
        // is how many home PCs present their one NVMe disk.
        Some(3 | 8 | 11 | 17) if answers_seek => Attached::Disk,
        // USB, SD and MMC: disks and cards that are plugged in. Many of them do not answer
        // about seeking, so that is not asked of them.
        Some(7 | 12 | 13) => Attached::Disk,
        // SCSI, SAS and the rest are used by real and by made-up disks alike.
        _ => Attached::Unclear,
    }
}

/// One drive, as the verdict looks at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// `C:`, or nothing for a shared folder without a letter. Fit to show.
    pub letter: String,
    pub attached: Attached,
    /// How it is attached, in the words the report uses: `NVMe`, `iSCSI`.
    pub how: String,
    /// What in the name the disk gives itself marks it as no disk of the PC ([`mark_in`]).
    /// `None` for a name without such a mark.
    pub marked: Option<String>,
    /// The disk did not answer what Windows asks a disk about seeking.
    pub seek_unanswered: bool,
    pub free: Option<u64>,
    pub read_only: bool,
}

impl Drive {
    /// What the verdict needs of a volume. Its letter goes through `scrub`, since the
    /// grounds of the verdict name the volume by it.
    pub fn of(volume: &Volume, scrub: &Scrub) -> Drive {
        let bus = volume.bus.ok();
        let on_a_disk = matches!(volume.kind, DriveKind::Fixed | DriveKind::Removable);
        let how = match (on_a_disk, bus) {
            (true, Some(bus)) => bus.label(),
            _ => volume.kind.label().to_owned(),
        };
        Drive {
            letter: scrub.text(&volume.letter),
            attached: attached(volume.kind, bus, &volume.model, volume.seek_penalty.is_ok()),
            how,
            marked: mark_in(&volume.model),
            seek_unanswered: on_a_disk && bus.is_some() && volume.seek_penalty.is_err(),
            free: volume.free.ok(),
            read_only: volume.read_only,
        }
    }
}

/// Where a PC keeps what is written to it, as far as a write probe shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keeps {
    /// On another volume than the one written to, whose free room went down with the
    /// writes: its letter, and what it had free before. That is about what the PC takes.
    Volume { letter: String, free: Option<u64> },
    /// In memory, about a byte of it for each byte written. What was available before is
    /// about what the PC takes. `steady` says that memory went down so step after step;
    /// without it there is one reading, which may be memory that moved for another reason.
    Memory { steady: bool },
    /// Nowhere that shows: nothing went down with the writes but the volume written to.
    /// What takes them (a device the PC does not show, or the shop's server) and how much
    /// of them it takes cannot be seen from the PC.
    Unseen,
}

/// Reads where a PC keeps what is written to it out of what a write noted: what every
/// volume had free and what memory was available, before, after each step, and after.
/// Changes nothing and reads nothing.
///
/// - A volume that was not written to lost at least half of what was written, and after
///   every step at least half of what was written until then: the writes are kept there.
/// - Memory lost about as much as was written ([`ONE_FOR_ONE`]). With [`STEADY_STEPS`] or
///   more steps it has to have done so at three steps in four, each held against what
///   that step wrote, and is then `steady`. With fewer steps the one reading after the
///   write is all there is.
/// - Otherwise nothing shows where they are kept.
pub fn keeps(landing: &Landing) -> Keeps {
    let written = landing.written;
    for (at, volume) in landing.volumes.iter().enumerate().filter(|(_, volume)| !volume.written_to) {
        let (Some(before), Some(after_write)) = (volume.before, volume.after_write) else { continue };
        let mut until_then = 0u64;
        let in_step = landing.steps.iter().all(|step| {
            until_then += step.bytes;
            step.free.get(at).copied().flatten().is_some_and(|now| before.saturating_sub(now) >= until_then / 2)
        });
        if written > 0 && before.saturating_sub(after_write) >= written / 2 && in_step {
            return Keeps::Volume { letter: volume.letter.clone(), free: Some(before) };
        }
    }
    let one_for_one = |gone: u64, written: u64| {
        written > 0 && (ONE_FOR_ONE.0..=ONE_FOR_ONE.1).contains(&(gone as f64 / written as f64))
    };
    if let [Some(before), Some(after_write), ..] = landing.memory {
        let gone = before.available.saturating_sub(after_write.available);
        if landing.steps.len() < STEADY_STEPS {
            if one_for_one(gone, written) {
                return Keeps::Memory { steady: false };
            }
        } else {
            let mut last = before.available;
            let with_the_step = landing.steps.iter().filter(|step| {
                let Some(now) = step.memory.map(|memory| memory.available) else { return false };
                let went = one_for_one(last.saturating_sub(now), step.bytes);
                last = now;
                went
            });
            if with_the_step.count() * 4 >= landing.steps.len() * 3 && one_for_one(gone, written) {
                return Keeps::Memory { steady: true };
            }
        }
    }
    Keeps::Unseen
}

/// Where the writes of the write limit test were seen to go.
#[derive(Debug, Clone, PartialEq)]
pub enum Went {
    /// Onto another volume than the one written to, whose free room went down with them.
    Volume { letter: String },
    /// From `from` bytes on, every step was written at about what the wired line carries,
    /// at `speed` bytes a second: on a seat, that is writes passed on to the shop's
    /// server. `memory_held` says that memory no longer went down with them from there.
    /// Nought for `from` is a write that went at that speed from its first step.
    Line { from: u64, speed: f64, memory_held: bool },
    /// Into memory, about a byte of it for each byte written, to the end.
    Memory,
    /// Nowhere that shows.
    Unseen,
}

/// Reads where the writes of a long write went out of what it noted after each step.
/// `line` is what the wired link carries, in bits a second. Changes nothing and reads
/// nothing.
///
/// - Another volume went down with the writes ([`keeps`]).
/// - From some step on, every whole step was written within [`LINE_WRITES`] of what the
///   line carries, two steps or more. The steps before it were written to somewhere
///   nearer.
/// - Memory went down by about what each step wrote at three steps in four.
pub fn went(landing: &Landing, line: Option<u64>) -> Went {
    if let Keeps::Volume { letter, .. } = keeps(landing) {
        return Went::Volume { letter };
    }
    let steps = &landing.steps;
    let speed_of = |step: &crate::facts::Step| step.bytes as f64 / step.seconds.max(1e-6);
    // The last step may be shorter than the others, and is then no measure of speed.
    let whole = match steps.as_slice() {
        [first, .., last] if last.bytes < first.bytes => &steps[..steps.len() - 1],
        all => all,
    };
    if let Some(carries) = line.map(|bits| bits as f64 / 8.0).filter(|carries| *carries > 0.0) {
        let on_the_line =
            |step: &crate::facts::Step| (LINE_WRITES.0..=LINE_WRITES.1).contains(&(speed_of(step) / carries));
        let from = whole.iter().rposition(|step| !on_the_line(step)).map_or(0, |last_off| last_off + 1);
        if whole.len() - from >= 2 {
            let mut speeds: Vec<f64> = whole[from..].iter().map(speed_of).collect();
            speeds.sort_by(f64::total_cmp);
            let available = |memory: Option<crate::facts::Memory>| memory.map(|memory| memory.available);
            let then = if from == 0 { available(landing.memory[0]) } else { available(steps[from - 1].memory) };
            let at_the_end = steps.last().and_then(|last| available(last.memory));
            let since: u64 = steps[from..].iter().map(|step| step.bytes).sum();
            let memory_held =
                matches!((then, at_the_end), (Some(then), Some(end)) if then.saturating_sub(end) < since / 2);
            return Went::Line {
                from: steps[..from].iter().map(|step| step.bytes).sum(),
                speed: speeds[speeds.len() / 2],
                memory_held,
            };
        }
    }
    if steps.len() >= STEADY_STEPS && keeps(landing) == (Keeps::Memory { steady: true }) {
        return Went::Memory;
    }
    Went::Unseen
}

/// The most that memory was down while a write went on, against what was available before
/// it began: by the readings after each step and the one after the write. What was read
/// after the files were removed does not come into it. `None` where memory was not read.
pub fn dip(landing: &Landing) -> Option<u64> {
    let before = landing.memory[0]?.available;
    let during = landing.steps.iter().filter_map(|step| step.memory).chain(landing.memory[1]);
    during.map(|now| before.saturating_sub(now.available)).max()
}

/// What a write probe found, as the verdict looks at it.
#[derive(Debug, Clone, PartialEq)]
pub struct Landed {
    /// Bytes the probe wrote, and the bytes a second it wrote them at, flush included.
    pub written: u64,
    pub speed: f64,
    /// Where what was written is kept.
    pub keeps: Keeps,
    /// Where its writes went by the speed of its steps: whether they settled at what the
    /// wired line carries.
    pub went: Went,
    /// The memory that was available before the write, and how much less was available
    /// after it. Zero where no less was. `None` where memory was not read before and after.
    pub memory: Option<(u64, u64)>,
    /// The most that memory was down while the probe wrote ([`dip`]).
    pub dip: Option<u64>,
}

impl Landed {
    /// What the verdict needs of a probe on a PC whose wired line carries `line` bits a
    /// second. `None` for one that wrote less than half of what it set out to write: that
    /// says nothing.
    pub fn of(landing: &Landing, line: Option<u64>) -> Option<Landed> {
        if landing.written < landing.wanted / 2 || landing.written == 0 {
            return None;
        }
        let memory = match landing.memory {
            [Some(before), Some(after_write), ..] => {
                Some((before.available, before.available.saturating_sub(after_write.available)))
            }
            _ => None,
        };
        let speed = landing.written as f64 / landing.seconds.max(1e-6);
        Some(Landed {
            written: landing.written,
            speed,
            keeps: keeps(landing),
            went: went(landing, line),
            memory,
            dip: dip(landing),
        })
    }

    /// How many times faster the write went than the same drive is read at `read_speed`
    /// bytes a second, when that is [`WRITES_FASTER`] times or more: what was written then
    /// did not go where the reads come from.
    pub fn times_faster_than(&self, read_speed: Option<f64>) -> Option<f64> {
        let times = self.speed / read_speed.filter(|read| *read > 0.0)?;
        (times >= WRITES_FASTER).then_some(times)
    }
}

/// What the write limit test came to: the step that writes as much as a main patch does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// Everything it set out to write was written, made safe and removed: this many bytes.
    Passed { written: u64 },
    /// It ended after this many bytes: on an error with this Windows number, or because
    /// the diagnostic ended it (`halted`) by what it read after a step.
    Stopped { written: u64, error: Option<i32>, halted: Option<Halt> },
}

impl Limit {
    /// Reads how the test ended out of what it noted. `None` for a test the user ended:
    /// that says nothing about how far it would have got.
    pub fn of(landing: &Landing) -> Option<Limit> {
        let written = landing.written;
        match (landing.error, landing.halted) {
            (Some(code), _) => {
                Some(Limit::Stopped { written, error: Some(code).filter(|code| *code != 0), halted: None })
            }
            (None, Some(halt)) => Some(Limit::Stopped { written, error: None, halted: Some(halt) }),
            (None, None) if written >= landing.wanted && written > 0 => Some(Limit::Passed { written }),
            (None, None) => None,
        }
    }
}

/// How the folder's files stand against what Nexon published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Files {
    NotCompared,
    /// The folder is at a state the table knows. `unread` is how many of its files were
    /// not read, and are taken for right by their length or by their being there.
    At {
        unread: usize,
    },
    /// It could be at either of several states.
    Either,
    /// It fits no state, and every state it could be at is known. Against the nearest one,
    /// this many files are something else than they should be, and this many are not there.
    Differs {
        differs: usize,
        missing: usize,
    },
    /// It fits no state that is known, and it may be at one that is not: the server has a
    /// version newer than the table built into this program (`newer` names it), or the
    /// server could not be asked which version is the newest (`newer` is `None`). The
    /// counts are against the nearest known state, as for [`Files::Differs`].
    Unsettled {
        differs: usize,
        missing: usize,
        newer: Option<String>,
    },
}

/// The comparison of the folder's files that is gone by: the full one says more than the
/// one of the small files, and that one more than the one that read nothing. One that was
/// cut short has read too little to be gone by.
pub fn gone_by(facts: &Facts) -> Option<&Comparison> {
    match &facts.full {
        Asked::Done(full) if !full.cut_short => Some(full),
        _ => facts.quick.as_ref().ok().filter(|quick| !quick.cut_short).or(facts.sizes.as_ref().ok()),
    }
}

/// The facts the verdict is worked out from.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    pub install_found: bool,
    pub system: Option<Drive>,
    pub game: Option<Drive>,
    /// The game is on the drive Windows is on.
    pub same_drive: bool,
    /// Names of programs and drivers that matter, already fit to show.
    pub programs: Programs,
    /// What could not be listed of what runs on the PC, in the words of the report: the
    /// programs, the drivers, each with why. Where one is named here, [`Evidence::programs`]
    /// says nothing of what it would have held.
    pub unlisted: Vec<String>,
    pub write_test: WriteTest,
    /// What the write probe in the install folder found.
    pub landed: Option<Landed>,
    /// What the write limit test came to, when it was run to its end in the install
    /// folder. One that was run on another volume says nothing of the game's drive.
    pub limit: Option<Limit>,
    /// Where its writes were seen to go.
    pub limit_went: Option<Went>,
    /// How fast a large file of the game was read, twice, past Windows' own file cache.
    /// `None` when it was not timed.
    pub reads: Option<Reads>,
    /// What the wired link carries, in bits a second. `None` without a cable.
    pub line: Option<u64>,
    /// What the patch that is waiting needs of the game's drive, where the caller said.
    pub room: Option<Room>,
    pub files: Files,
    /// No file was read: [`Evidence::files`] is by which files are there, how long they
    /// are, and which version the game's own patcher logged last.
    pub files_by_length: bool,
}

impl Evidence {
    /// The evidence in what was found. Names of programs and of volumes go through `scrub`
    /// here, since the grounds of the verdict quote them.
    pub fn of(facts: &Facts, scrub: &Scrub) -> Evidence {
        let shown = |names: &[String]| names.iter().map(|name| scrub.text(name)).collect();
        let found = facts.programs.clone().unwrap_or_default();
        let programs = Programs {
            diskless: shown(&found.diskless),
            restore: shown(&found.restore),
            seat: shown(&found.seat),
            iscsi: shown(&found.iscsi),
            netdisk: shown(&found.netdisk),
            spare: shown(&found.spare),
            hints: shown(&found.hints),
            noted: shown(&found.noted),
            game: shown(&found.game),
        };
        // A list that was not asked for is not one that could not be read. Two that could
        // not be read for one reason are said in one.
        let unread = |why: Option<Unknown>| why.filter(|why| *why != Unknown::Skipped);
        let unlisted = match (unread(facts.programs.as_ref().err().copied()), unread(facts.drivers_listed.err())) {
            (Some(programs), Some(drivers)) if programs == drivers => {
                vec![format!("프로그램·드라이버 목록 {}", programs.label())]
            }
            (programs, drivers) => [("프로그램 목록", programs), ("드라이버 목록", drivers)]
                .into_iter()
                .filter_map(|(what, why)| Some(format!("{what} {}", why?.label())))
                .collect(),
        };
        let (system, game) = (facts.system_drive.as_ref().ok(), facts.game_drive.as_ref().ok());
        let client = facts.client.client();
        let compared = gone_by(facts);
        let newest = facts.shelf.as_ref().ok().map(|shelf| shelf.newest);
        let files = match compared {
            None => Files::NotCompared,
            Some(compared) => match (&compared.at, &compared.either[..], &compared.against) {
                (Some(_), _, _) => Files::At { unread: compared.tally.same_length + compared.tally.unread },
                (None, [_, ..], _) => Files::Either,
                (None, [], Some(_)) => {
                    let (differs, missing) = (compared.tally.differs.len(), compared.tally.missing.len());
                    // The table knows what the files are up to its newest state. A patch on
                    // the server only says what it starts from, so nothing knows the files
                    // of a version that came after the table was made.
                    let known_up_to = facts.tables[usize::from(facts.client == mfp_kms::Server::Test)];
                    match newest {
                        Some(newest) if newest.order() <= known_up_to.order() => Files::Differs { differs, missing },
                        Some(newest) => Files::Unsettled { differs, missing, newer: Some(newest.label(client)) },
                        None => Files::Unsettled { differs, missing, newer: None },
                    }
                }
                (None, [], None) => Files::NotCompared,
            },
        };
        // The cards are listed with the cabled ones first, the fastest of them first.
        let wired = facts.adapters.as_ref().ok().and_then(|cards| cards.iter().find(|card| !card.wireless));
        Evidence {
            install_found: facts.install.is_some(),
            same_drive: matches!((system, game), (Some(system), Some(game)) if system.letter == game.letter && !game.letter.is_empty()),
            system: system.map(|volume| Drive::of(volume, scrub)),
            game: game.map(|volume| Drive::of(volume, scrub)),
            programs,
            unlisted,
            write_test: facts.write_test,
            landed: match &facts.landing {
                // The verdict names the volume that took the writes, so its name is made
                // fit to show: a shared folder is named by the computer that shares it.
                Asked::Done(landing) => Landed::of(landing, wired.and_then(|card| card.speed)).map(|landed| Landed {
                    keeps: match landed.keeps {
                        Keeps::Volume { letter, free } => Keeps::Volume { letter: scrub.text(&letter), free },
                        other => other,
                    },
                    ..landed
                }),
                _ => None,
            },
            limit: match &facts.limit {
                WriteLimit::Done(landing) if landing.place == Place::Install => Limit::of(landing),
                _ => None,
            },
            limit_went: match &facts.limit {
                WriteLimit::Done(landing) if landing.place == Place::Install => {
                    Some(match went(landing, wired.and_then(|card| card.speed)) {
                        Went::Volume { letter } => Went::Volume { letter: scrub.text(&letter) },
                        other => other,
                    })
                }
                _ => None,
            },
            reads: facts.reads.ok(),
            line: wired.and_then(|card| card.speed),
            room: facts.room,
            files,
            files_by_length: compared.is_some_and(|compared| compared.by_length),
        }
    }
}

/// The kinds of PC the verdict tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A PC like the one at home: its own disk, and nothing that puts it back.
    Ordinary,
    /// A seat with a disk of its own and a program that puts the disk back at every start.
    DiskWithRestore,
    /// A seat without a disk of its own.
    Diskless,
    /// The facts do not decide.
    Unknown,
}

/// What is expected of something that has not been tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outlook {
    Expected,
    Unlikely,
    /// The facts do not decide.
    Unknown,
}

/// One of the three answers in everyday words, for a page that a player reads and for the
/// top of the report: what the row is called, the answer, and the one reason for it.
///
/// Nothing in these names a product, a bus or a driver: a player does not know what those
/// mean. They are short phrases without a full stop, and they state things outright. The
/// answer is one of a few fixed words; the reason is a fact, or the one condition the
/// answer hangs on. Where the rules do not decide, the answer is that it could not be
/// checked, with what kept it from being.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plain {
    /// What the row is called: `PC 종류`, `클라이언트 패치`, `재부팅 후`.
    pub name: &'static str,
    pub answer: &'static str,
    pub reason: String,
}

/// What an answer rests on, as the report prints it in a row: a few short facts that stand
/// side by side. Facts only: what was read off the PC, in so many words, and nothing that
/// answers a second time or says what is expected. Empty where no such fact was gone by.
pub type Grounds = Vec<String>;

/// The verdict: each answer as the rules decide it, in everyday words as a page shows it,
/// and with the facts it rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub kind: Kind,
    /// What the kind was told by: what the disks call themselves and how they are
    /// attached, and the programs and drivers that were gone by, by name.
    pub kind_grounds: Grounds,
    /// What the speeds that were measured say of where the game's files come from, where
    /// that is a sign: reads at what the line carries, a second read that is many times
    /// faster, writes that are far faster than reads.
    pub speed_grounds: Grounds,
    pub kind_plain: Plain,
    /// Whether a main patch is expected to go through here.
    pub patch: Outlook,
    pub patch_grounds: Grounds,
    pub patch_plain: Plain,
    /// Whether what the patch wrote is expected to be there after a restart.
    pub kept: Outlook,
    pub kept_grounds: Grounds,
    pub kept_plain: Plain,
}

impl Kind {
    /// The kind in everyday words, as a page names it.
    pub fn plain(self) -> &'static str {
        match self {
            Kind::Ordinary => "일반 PC",
            Kind::Diskless => "노하드 PC방 좌석",
            Kind::DiskWithRestore => "복구 프로그램이 있는 PC",
            Kind::Unknown => "확인 불가",
        }
    }
}

impl Verdict {
    /// The three answers in everyday words, in the order a page shows them: the kind of
    /// PC, the main patch, and what a restart leaves of it.
    pub fn plain(&self) -> [&Plain; 3] {
        [&self.kind_plain, &self.patch_plain, &self.kept_plain]
    }
}

/// Works out the verdict. Changes nothing and asks nothing: the same evidence gives the same
/// verdict.
pub fn verdict(evidence: &Evidence) -> Verdict {
    let signs = Signs::of(evidence);
    let (kind, kind_grounds) = kind_of(evidence, &signs);
    let patch = patch_outlook(evidence, kind, &signs);
    let kept = kept_outlook(kind);
    let (patch_plain, patch_grounds) = plain_patch(evidence, &signs, kind, patch);
    Verdict {
        kind,
        kind_grounds,
        speed_grounds: signs.speeds.clone(),
        kind_plain: plain_kind(evidence, kind),
        patch,
        patch_grounds,
        patch_plain,
        kept,
        kept_grounds: kept_grounds(evidence, kind),
        kept_plain: plain_kept(kind, kept),
    }
}

/// The kind of PC in everyday words, with the one reason for it.
fn plain_kind(evidence: &Evidence, kind: Kind) -> Plain {
    let drives = drives(evidence);
    let any = |attached: Attached| drives.iter().any(|drive| drive.attached == attached);
    let reason = match kind {
        Kind::Ordinary => "게임이 이 PC의 디스크에 설치됨",
        Kind::Diskless => {
            // Told only by where the write probe's writes landed: that says where the
            // PC writes, and nothing of a server.
            let served = !evidence.programs.diskless.is_empty() || any(Attached::Network);
            let landed = matches!(&evidence.landed, Some(Landed { keeps: Keeps::Volume { .. }, .. }));
            if landed && !served {
                "게임 폴더에 쓴 내용이 다른 곳에 따로 저장됨"
            } else {
                "게임 디스크를 매장 서버에서 네트워크로 받아 사용"
            }
        }
        Kind::DiskWithRestore => "재부팅 시 변경 내용을 되돌리는 프로그램 실행 중",
        Kind::Unknown if !evidence.install_found => "설치 폴더 없음",
        Kind::Unknown if evidence.game.is_none() || evidence.system.is_none() => "디스크 정보를 읽지 못함",
        Kind::Unknown if any(Attached::Unclear) || any(Attached::Share) => "디스크 종류를 알 수 없음",
        Kind::Unknown => "PC방 좌석인지 가릴 근거 부족",
    };
    Plain { name: "PC 종류", answer: kind.plain(), reason: reason.to_owned() }
}

/// What is said of a main patch in everyday words, with the one reason for it and the
/// facts that reason rests on: what decided it, by the rule that did.
///
/// The room is held against what the engine says the waiting patch needs, where the caller
/// handed that in ([`Evidence::room`]). Only with those figures can a drive that is too
/// small for the usual way be said to take the immediate way, or neither: without them it
/// is said that it could not be checked, with what is free and what the usual way needs.
/// A seat without a disk whose limit cannot be seen is said to take the patch, with the
/// one condition it hangs on. Of the files it is said that they are sound only where every
/// one of them was read.
fn plain_patch(evidence: &Evidence, signs: &Signs, kind: Kind, outlook: Outlook) -> (Plain, Grounds) {
    // Whole gigabytes: what is free is rounded down, what is needed up.
    let whole = |bytes: u64| format!("{} GB", count(bytes >> 30));
    let up = |bytes: u64| format!("{} GB", count(bytes.div_ceil(1 << 30)));
    let said = |answer: &'static str, reason: String, facts: Grounds| {
        (Plain { name: "클라이언트 패치", answer, reason }, facts)
    };
    if !evidence.install_found {
        return said("확인 불가", "설치 폴더 없음".to_owned(), Vec::new());
    }
    let letter = evidence.game.as_ref().map_or("", |game| game.letter.as_str());
    // What stops a patch whatever the drive is.
    if let WriteTest::Failed(code) = evidence.write_test {
        let failed =
            code.map_or("빈 파일 생성 실패".to_owned(), |code| format!("빈 파일 생성 실패 (오류 {code})"));
        return said("불가", "설치 폴더에 쓸 수 없음".to_owned(), vec![failed]);
    }
    if evidence.game.as_ref().is_some_and(|game| game.read_only) {
        return said("불가", "게임 디스크가 읽기 전용".to_owned(), vec![on(letter, "읽기 전용 볼륨")]);
    }
    if let Files::Differs { differs, missing } = evidence.files {
        let reason = if missing > 0 {
            format!("게임 파일 없음({}개)", count(missing as u64))
        } else {
            format!("게임 파일이 원본과 다름({}개)", count(differs as u64))
        };
        return said("불가", reason, off_the_list(evidence.files_by_length, differs, missing));
    }
    // The test that wrote as much as a main patch does, where it settled it.
    match evidence.limit {
        Some(Limit::Passed { written }) if written >= MAIN_PATCH_WRITES => {
            let mut facts = vec![format!("한도 검사 {} 쓰기 성공", size(written))];
            facts.extend(evidence.limit_went.as_ref().and_then(|went| went_fact(went, kind == Kind::Diskless)));
            return said("가능", "패치 크기만큼 쓰기 확인".to_owned(), facts);
        }
        Some(Limit::Stopped { written, error, halted }) if !matches!(halted, Some(Halt::Slow | Halt::TimedOut)) => {
            let why = match (error, halted, &evidence.limit_went) {
                (Some(code), ..) => format!("쓰기 실패 (오류 {code})"),
                (None, Some(Halt::Drive), _) => on(letter, "여유 부족"),
                // The volume that went down with the writing is named where it was seen.
                (None, Some(Halt::Elsewhere), Some(Went::Volume { letter: taken })) => on(taken, "여유 부족"),
                (None, Some(Halt::Elsewhere), _) => "다른 드라이브 여유 부족".to_owned(),
                (None, Some(Halt::Memory), _) => "메모리 부족".to_owned(),
                (None, ..) => "쓰기 실패".to_owned(),
            };
            let facts = vec![format!("한도 검사 {}에서 중단", size(written)), why];
            return said("불가", format!("쓰기 공간 부족({}에서 중단)", whole(written)), facts);
        }
        _ => {}
    }
    // A write limit test that settled nothing, as the fact it is: it ended itself because
    // the writing got slow, or it was sized smaller than a main patch.
    let unsettled = match evidence.limit {
        Some(Limit::Stopped { written, .. }) => Some(format!("한도 검사 {}에서 중단", size(written))),
        Some(Limit::Passed { written }) => Some(format!("한도 검사는 {}만 씀", size(written))),
        None => None,
    };
    let wrote = matches!(evidence.write_test, WriteTest::Passed | WriteTest::LeftBehind(_));
    // A seat without a disk: players do finish patches on such seats, and what ends one
    // is the room the shop gives its seats to write to. That is its server's where the
    // write probe showed the writes going there.
    if kind == Kind::Diskless {
        let to_the_server = matches!(
            &evidence.landed,
            Some(Landed { keeps: Keeps::Unseen, went: Went::Line { memory_held: true, .. }, .. })
        );
        let ends = if to_the_server {
            "매장 서버 공간이 부족하면 중단"
        } else {
            "매장의 쓰기 공간이 부족하면 중단"
        };
        // What the write probe showed of where the seat's writes go, and how far the
        // writing was tried. Without a probe nothing was measured, and nothing is said.
        let mut facts = Vec::new();
        match &evidence.landed {
            None => facts.extend(unsettled),
            Some(Landed { keeps: Keeps::Volume { letter: taken, free }, .. }) => {
                facts.push(format!("쓴 만큼 {taken} 여유 감소"));
                facts.extend(free.map(|free| on(taken, &format!("여유 {}", gb(free)))));
                facts.extend(unsettled);
            }
            Some(landed @ Landed { keeps: Keeps::Memory { steady }, .. }) => {
                facts.push(match (steady, landed.memory) {
                    (true, _) => "단계마다 쓴 만큼 메모리 감소".to_owned(),
                    (false, Some((_, lost))) => format!("{} 쓰기에 메모리 {} 감소", size(landed.written), size(lost)),
                    (false, None) => "쓴 만큼 메모리 감소".to_owned(),
                });
                facts.extend(landed.memory.map(|(available, _)| format!("메모리 여유 {}", gb(available))));
                facts.extend(unsettled);
            }
            // The steps settled at what the line carries.
            Some(landed @ Landed { went: Went::Line { from, .. }, .. }) => {
                let settled = if *from == 0 {
                    "쓰기 속도가 네트워크 전송 속도와 유사"
                } else {
                    "쓰기 후반 속도가 네트워크 전송 속도와 유사"
                };
                facts.push(settled.to_owned());
                facts.extend(landed.dip.map(|dip| match dip {
                    0..HALF_A_MEGABYTE => "쓰기 중 메모리 감소 없음".to_owned(),
                    dip => format!("쓰기 중 메모리 {} 감소", size(dip)),
                }));
                facts.extend(unsettled);
            }
            // Nothing that can be seen from the seat moved with the writes.
            Some(_) => {
                facts.push("쓴 만큼 줄어든 메모리·드라이브 없음".to_owned());
                facts.extend(unsettled);
            }
        }
        return said("가능", ends.to_owned(), facts);
    }
    // Any other PC: by the game's drive.
    let Some(game) = &evidence.game else {
        return said("확인 불가", "디스크 정보를 읽지 못함".to_owned(), vec!["게임 디스크 정보 확인 실패".to_owned()]);
    };
    let Some(free) = game.free else {
        return said("확인 불가", "디스크 여유 공간을 읽지 못함".to_owned(), vec![on(letter, "여유 확인 실패")]);
    };
    let room = on(letter, &format!("여유 {}", gb(free)));
    if game.attached != Attached::Disk {
        return said("확인 불가", "디스크 종류를 알 수 없음".to_owned(), vec![not_a_disk(game)]);
    }
    // Something says the files come from elsewhere, and not enough to call it a seat. What
    // says so is with the kind already.
    if signs.about_a_drive() > 0 {
        return said("확인 불가", "PC 종류를 알 수 없음".to_owned(), vec![room]);
    }
    let (safe, immediate) = evidence.room.map_or((ROOM_NEEDED, None), |room| (room.safe, Some(room.immediate)));
    if free < safe {
        let short = format!("여유 공간 {}(일반 패치는 약 {} 필요)", whole(free), up(safe));
        let mut facts = vec![room, format!("일반 패치 필요 {}", gb(safe))];
        facts.extend(immediate.map(|immediate| format!("즉시 패치 필요 {}", gb(immediate))));
        return match immediate {
            Some(immediate) if free >= immediate => said("즉시 패치로 가능", short, facts),
            Some(_) => said("불가", format!("여유 공간 부족({})", whole(free)), facts),
            // How much the immediate way needs is in the patch file, which is not here.
            None => said("확인 불가", short, facts),
        };
    }
    if let Files::Unsettled { differs, missing, newer } = &evidence.files {
        let mut facts = off_the_list(evidence.files_by_length, *differs, *missing);
        facts.push(match newer {
            Some(newer) => format!("서버 최신 {newer}의 파일 목록 없음"),
            None => "서버 최신 버전 미확인".to_owned(),
        });
        return said("확인 불가", "게임 파일이 알려진 버전과 다름".to_owned(), facts);
    }
    if matches!(evidence.write_test, WriteTest::Skipped) {
        return said("확인 불가", "설치 폴더 쓰기 확인 안 함".to_owned(), vec![room]);
    }
    // The careful answer agrees, or the engine's figures say more than it could.
    if outlook != Outlook::Expected && evidence.room.is_none() {
        return said("확인 불가", "확인에 필요한 정보 부족".to_owned(), vec![room]);
    }
    let read = matches!(&evidence.files, Files::At { unread: 0 }) && !evidence.files_by_length;
    let enough = format!("여유 공간 충분({})", whole(free));
    let mut facts = vec![room];
    facts.extend(wrote.then(|| "설치 폴더 쓰기 성공".to_owned()));
    facts.extend(unsettled);
    facts.extend(read.then(|| "모든 게임 파일 내용 일치".to_owned()));
    said("가능", if read { format!("{enough} · 게임 파일 정상") } else { enough }, facts)
}

/// What a restart leaves of a patch in everyday words, with the one reason for it.
fn plain_kept(kind: Kind, outlook: Outlook) -> Plain {
    let (answer, reason) = match (outlook, kind) {
        (Outlook::Expected, _) => ("패치 유지", "이 PC의 디스크에 저장됨"),
        (Outlook::Unlikely, _) => ("패치 초기화", "재부팅·자리 이동 시 다시 패치 필요"),
        (Outlook::Unknown, Kind::DiskWithRestore) => ("확인 불가", "복구 프로그램 설정에 따라 다름"),
        (Outlook::Unknown, _) => ("확인 불가", "PC 종류를 알 수 없음"),
    };
    Plain { name: "재부팅 후", answer, reason: reason.to_owned() }
}

/// Memory that is down by less than this is memory that did not move.
const HALF_A_MEGABYTE: u64 = 1 << 19;

/// A fact about a drive with the drive's letter in front, where it has one:
/// `D: 여유 200.0 GB`.
fn on(letter: &str, fact: &str) -> String {
    if letter.is_empty() { fact.to_owned() } else { format!("{letter} {fact}") }
}

/// Facts in the order they come in, each once: both drives of a seat are mostly marked by
/// one name.
fn unique(facts: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for fact in facts {
        if !kept.contains(&fact) {
            kept.push(fact);
        }
    }
    kept
}

/// That the name a disk gives itself marks it as no disk of the PC, as a fact: by its
/// maker, or by a word in the name.
fn marked_fact(mark: &str) -> String {
    if SERVER_MAKERS.iter().any(|(_, written)| *written == mark) {
        format!("{mark} 디스크")
    } else {
        format!("디스크 이름에 {mark} 포함")
    }
}

/// What a drive is that is not known to be a disk of the PC, as a fact.
fn not_a_disk(drive: &Drive) -> String {
    match (&drive.marked, drive.attached) {
        (_, Attached::Share) => on(&drive.letter, "공유 폴더"),
        (_, Attached::Virtual) => on(&drive.letter, &format!("{} 디스크", drive.how)),
        (Some(mark), _) => marked_fact(mark),
        (None, _) if drive.seek_unanswered => {
            on(&drive.letter, &format!("{} 드라이브, 탐색 지연 응답 없음", drive.how))
        }
        (None, _) => on(&drive.letter, &format!("{} 드라이브", drive.how)),
    }
}

/// How many of the game's files are not what the list they are held against names, as
/// facts: those that are not there, and those that are something else. Files that were not
/// read differ by their lengths, which is all that was looked at.
fn off_the_list(by_length: bool, differs: usize, missing: usize) -> Vec<String> {
    let mut facts = Vec::new();
    if missing > 0 {
        facts.push(format!("게임 파일 {}개 누락", count(missing as u64)));
    }
    if differs > 0 {
        let how = if by_length { "크기 변경" } else { "변경" };
        facts.push(format!("게임 파일 {}개 {how}", count(differs as u64)));
    }
    facts
}

/// Where the writes of the write limit test went, as a fact. `None` where nothing was
/// seen. `seat` says that the PC is a seat without a disk: only there is a write that went
/// at the speed of the line from its first step worth a word. On any other PC that is the
/// speed of its drive.
fn went_fact(went: &Went, seat: bool) -> Option<String> {
    match went {
        Went::Volume { letter } => Some(format!("쓴 만큼 {letter} 여유 감소")),
        Went::Memory => Some("쓴 만큼 메모리 감소".to_owned()),
        Went::Line { from: 0, .. } if !seat => None,
        Went::Line { from: 0, .. } => Some("처음부터 네트워크 전송 속도와 유사".to_owned()),
        Went::Line { from, .. } => Some(format!("{}부터 네트워크 전송 속도와 유사", size(*from))),
        Went::Unseen => None,
    }
}

/// The drives of the evidence: the two as one where the game is on the system drive.
fn drives(evidence: &Evidence) -> Vec<&Drive> {
    match (&evidence.system, &evidence.game) {
        (Some(system), Some(_)) if evidence.same_drive => vec![system],
        (system, game) => system.iter().chain(game).collect(),
    }
}

/// The signs that the files of Windows or of the game come from another computer.
#[derive(Debug, Default)]
struct Signs {
    /// Those that are about a drive of this PC by what the disk is, each as the fact it is
    /// stated in: a name that marks it as made up, a bus that says so.
    devices: Vec<String>,
    /// Those that are about a drive of this PC by how fast it is read and written, each
    /// as the fact it is stated in.
    speeds: Vec<String>,
    /// Those that are about the PC as a whole, each as the names of the programs or
    /// drivers it is.
    of_the_pc: Vec<String>,
    /// How many storage drivers for server adapters are loaded, where that is a sign. It
    /// is one about the PC. The drivers come with Windows, and are counted and not named.
    spare: Option<usize>,
}

impl Signs {
    fn of(evidence: &Evidence) -> Signs {
        let mut signs = Signs::default();
        let programs = &evidence.programs;
        for drive in drives(evidence) {
            if let Some(mark) = &drive.marked {
                signs.devices.push(marked_fact(mark));
            }
            if drive.attached == Attached::Virtual {
                signs.devices.push(not_a_disk(drive));
            }
        }
        if let Some(reads) = evidence.reads {
            // Read at what the line carries, both times, and each far read a trip over it.
            let line = evidence.line.map(|bits| bits as f64 / 8.0).filter(|line| *line > 0.0);
            let at_line_speed = line.filter(|line| {
                [reads.first, reads.again].iter().all(|read| (LINE_SHARE.0..=LINE_SHARE.1).contains(&(read / line)))
            });
            // A hard disk reads at such a speed too, and no faster the second time, for
            // these reads go past Windows' file cache. What tells it apart is the read at
            // a far place, which takes a hard disk's arm 8 ms and more. So that read has
            // to take what a trip over a local network takes, or else be no hard disk's
            // while the long reads are slower than any disk without moving parts reads.
            let over_the_line = reads.seek_ms.filter(|ms| {
                let slow = reads.first.max(reads.again) < NO_DISK_THIS_SLOW;
                (LINE_MS.0..=LINE_MS.1).contains(ms) || (*ms < HARD_DISK_MS && slow)
            });
            if at_line_speed.is_some() && over_the_line.is_some() {
                signs.speeds.push("네트워크 전송 속도와 유사한 읽기 패턴".to_owned());
            }
            // Bytes that were not read before came about as fast as the second read: the
            // drive is that fast, and the first read was held up by something else.
            let held_up = reads.fresh.is_some_and(|fresh| reads.again < fresh * KEPT_BELOW);
            if reads.first > 0.0 && reads.again / reads.first >= KEPT_BELOW && !held_up {
                signs.speeds.push(format!("반복 읽기가 {:.0}배 빠름", reads.again / reads.first));
            }
        }
        // The probe wrote far faster than the drive is read: what it wrote was taken in
        // somewhere near, and did not go to where the reads come from.
        let read_speed = evidence.reads.map(|reads| reads.first);
        if let Some(times) = evidence.landed.as_ref().and_then(|landed| landed.times_faster_than(read_speed)) {
            signs.speeds.push(format!("쓰기 속도가 읽기 속도의 {times:.1}배"));
        }
        let shop: Vec<&str> = programs.seat.iter().chain(&programs.restore).map(String::as_str).collect();
        if !shop.is_empty() {
            signs.of_the_pc.push(shop.join(", "));
        }
        if !programs.iscsi.is_empty() {
            signs.of_the_pc.push(programs.iscsi.join(", "));
        }
        if !programs.netdisk.is_empty() {
            signs.of_the_pc.push(programs.netdisk.join(", "));
        }
        signs.spare = Some(programs.spare.len()).filter(|loaded| *loaded >= SPARE_DRIVERS);
        signs
    }

    /// How many signs are about a drive.
    fn about_a_drive(&self) -> usize {
        self.devices.len() + self.speeds.len()
    }

    fn count(&self) -> usize {
        self.about_a_drive() + self.of_the_pc.len() + usize::from(self.spare.is_some())
    }

    /// The signs about the PC as the grounds name them: programs first, then drivers. The
    /// storage drivers for servers are said, as a number, only where no other sign about
    /// the PC stands beside them.
    fn names(&self) -> Vec<String> {
        match self.spare {
            Some(loaded) if self.of_the_pc.is_empty() => vec![format!("서버용 저장 드라이버 {loaded}개")],
            _ => self.of_the_pc.clone(),
        }
    }
}

fn kind_of(evidence: &Evidence, signs: &Signs) -> (Kind, Grounds) {
    let programs = &evidence.programs;
    let drives = drives(evidence);
    let those = |attached: Attached| drives.iter().filter(move |drive| drive.attached == attached);
    // What decides comes first, and every sign there is follows it: what the disks are,
    // and then the programs and drivers by name. What the speeds say has a row of its own.
    let grounds = |facts: Vec<String>, names: Vec<String>| -> Grounds {
        let signs = signs.devices.iter().cloned().chain(signs.names());
        unique(facts.into_iter().chain(names).chain(signs).chain(evidence.unlisted.iter().cloned()))
    };
    // The drives that come over a storage network, by how they say they are attached:
    // `Fibre Channel 디스크`.
    let served =
        || -> Vec<String> { those(Attached::Network).map(|drive| format!("{} 디스크", drive.how)).collect() };
    // That the game is on a disk of the PC.
    let own = || -> Vec<String> {
        evidence.game.iter().map(|game| format!("이 PC의 {} 디스크에 설치", game.how)).collect()
    };

    // 1. A program that serves seats without a disk is running.
    if !programs.diskless.is_empty() {
        return (Kind::Diskless, grounds(served(), vec![programs.diskless.join(", ")]));
    }
    // 2. Windows or the game is on a disk that comes over a storage network.
    if those(Attached::Network).next().is_some() {
        return (Kind::Diskless, grounds(served(), Vec::new()));
    }
    let local = |drive: &Option<Drive>| drive.as_ref().is_some_and(|drive| drive.attached == Attached::Disk);
    let both_local = evidence.install_found && local(&evidence.game) && local(&evidence.system);
    // 3. Disks of the PC, and a program that puts a disk back at every start.
    if both_local && signs.about_a_drive() == 0 && !programs.restore.is_empty() {
        let names = std::iter::once(programs.restore.join(", ")).chain(evidence.unlisted.iter().cloned());
        return (Kind::DiskWithRestore, own().into_iter().chain(names).collect());
    }
    // 4. What was written to the game's drive took room on another one.
    if let Some(Landed { written, keeps: Keeps::Volume { letter, .. }, .. }) = &evidence.landed {
        let fact = format!("설치 폴더에 쓴 {}만큼 {letter} 여유 감소", size(*written));
        return (Kind::Diskless, grounds(vec![fact], Vec::new()));
    }
    // 5. A sign about a drive, and one more sign of any kind.
    if signs.about_a_drive() > 0 && signs.count() >= 2 {
        return (Kind::Diskless, grounds(Vec::new(), Vec::new()));
    }
    // 6. Disks of the PC and nothing that speaks of a PC room.
    if both_local && signs.count() == 0 && programs.hints.is_empty() {
        return (Kind::Ordinary, grounds(own(), Vec::new()));
    }
    // 7. The facts do not decide. What is in the way is said.
    let mut facts = Vec::new();
    if !evidence.install_found {
        facts.push("설치 폴더를 찾지 못함".to_owned());
    } else if evidence.game.is_none() {
        facts.push("게임 디스크 정보 확인 실패".to_owned());
    }
    if evidence.system.is_none() {
        facts.push("시스템 디스크 정보 확인 실패".to_owned());
    }
    // A drive whose name gives it away, and one that is made up, are among the signs already.
    let unclear = |drive: &&&Drive| match drive.attached {
        Attached::Share => true,
        Attached::Unclear => drive.marked.is_none(),
        Attached::Disk | Attached::Network | Attached::Virtual => false,
    };
    facts.extend(drives.iter().filter(unclear).map(|drive| not_a_disk(drive)));
    // How a disk says it is attached does not make it the PC's own where a sign about a
    // drive speaks against that.
    if both_local && signs.about_a_drive() == 0 {
        facts.extend(own());
    }
    // Names that hold a word a shop's programs hold keep the PC from being an ordinary one.
    let hints: Vec<String> =
        programs.hints.iter().map(|name| format!("{name} (이름이 매장 프로그램과 유사)")).collect();
    let signs = signs.devices.iter().cloned().chain(signs.names());
    (Kind::Unknown, unique(facts.into_iter().chain(signs).chain(hints).chain(evidence.unlisted.iter().cloned())))
}

/// What is expected of a main patch, by the rules at the top of this file.
fn patch_outlook(evidence: &Evidence, kind: Kind, signs: &Signs) -> Outlook {
    if !evidence.install_found {
        return Outlook::Unknown;
    }
    // What stops a patch whatever the drive is: a folder that takes no write, a drive
    // that takes none, and files that are known not to be Nexon's.
    if matches!(evidence.write_test, WriteTest::Failed(_))
        || evidence.game.as_ref().is_some_and(|game| game.read_only)
        || matches!(evidence.files, Files::Differs { .. })
    {
        return Outlook::Unlikely;
    }
    // What is expected where no test that wrote as much as a main patch settles it. On a
    // seat without a disk that is never more than that the limit is not known: the probe
    // writes a gigabyte, and a seat that takes that into memory, or onto a disk of its
    // own, may pass the rest on to its shop's server. What the game's drive has free is
    // not gone by there either: that is the server's figure.
    let by_the_kind = || if kind == Kind::Diskless { Outlook::Unknown } else { by_the_games_drive(evidence, signs) };
    // The test that writes as much as a main patch does settles it where it was run to
    // its end, whatever kind of PC this is.
    let outlook = match evidence.limit {
        // The writing got slow. How much the PC takes was not found out.
        Some(Limit::Stopped { error: None, halted: Some(Halt::Slow | Halt::TimedOut), .. }) => by_the_kind(),
        Some(Limit::Stopped { .. }) => Outlook::Unlikely,
        Some(Limit::Passed { written }) if written >= MAIN_PATCH_WRITES => Outlook::Expected,
        // No test, or one that was sized smaller than a main patch.
        _ => by_the_kind(),
    };
    // Files that may or may not be Nexon's are no ground to expect a patch to go through.
    if matches!(evidence.files, Files::Unsettled { .. }) && outlook == Outlook::Expected {
        return Outlook::Unknown;
    }
    outlook
}

/// What is expected of a main patch on a PC that is no seat without a disk: by what the
/// game's drive has free, where the drive is a disk of the PC, the folder took the empty
/// file, and nothing says the files come from elsewhere.
fn by_the_games_drive(evidence: &Evidence, signs: &Signs) -> Outlook {
    let Some(Drive { free: Some(free), attached, .. }) = &evidence.game else { return Outlook::Unknown };
    if *free < ROOM_NEEDED {
        return Outlook::Unlikely;
    }
    // Something says the files may come from elsewhere: what is free is then not known
    // to be what the PC can take.
    if signs.about_a_drive() > 0 {
        return Outlook::Unknown;
    }
    match (attached, evidence.write_test) {
        (Attached::Disk, WriteTest::Passed | WriteTest::LeftBehind(_)) => Outlook::Expected,
        _ => Outlook::Unknown,
    }
}

/// Whether what a patch wrote is expected to be there after a restart: that follows the
/// kind of PC.
fn kept_outlook(kind: Kind) -> Outlook {
    match kind {
        Kind::Ordinary => Outlook::Expected,
        Kind::Diskless => Outlook::Unlikely,
        Kind::DiskWithRestore | Kind::Unknown => Outlook::Unknown,
    }
}

/// What the answer about a restart rests on.
fn kept_grounds(evidence: &Evidence, kind: Kind) -> Grounds {
    let restore = &evidence.programs.restore;
    let running = || format!("{} 실행 중", restore.join(", "));
    match kind {
        Kind::Diskless => vec!["노하드 좌석은 재부팅·로그아웃 시 변경 내용 초기화".to_owned()],
        Kind::DiskWithRestore => vec![running(), "게임 디스크의 복구 제외 여부 미확인".to_owned()],
        Kind::Unknown if !restore.is_empty() => vec![running()],
        // Nothing is said to be absent where what runs could not be listed.
        Kind::Ordinary | Kind::Unknown if !evidence.unlisted.is_empty() => evidence.unlisted.clone(),
        Kind::Ordinary => vec!["노하드·복구 프로그램 없음".to_owned()],
        Kind::Unknown => vec!["알려진 복구 프로그램 없음".to_owned()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Fact, Memory, Unknown};

    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;
    /// A megabyte a second, as speeds are counted here.
    const MB_S: f64 = MIB as f64;

    fn drive(letter: &str, attached: Attached, how: &str, free_gib: u64) -> Drive {
        Drive {
            letter: letter.to_owned(),
            attached,
            how: how.to_owned(),
            marked: None,
            seek_unanswered: false,
            free: Some(free_gib * GIB),
            read_only: false,
        }
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn reads(first: f64, again: f64, seek_ms: f64) -> Option<Reads> {
        Some(Reads { bytes: 256 << 20, first: first * MB_S, again: again * MB_S, fresh: None, seek_ms: Some(seek_ms) })
    }

    /// A PC at home: Windows and the game on one NVMe disk with room, a 1 Gbps cable,
    /// nothing of a shop running, the folder writable, and its files those of the newest
    /// version.
    fn home() -> Evidence {
        Evidence {
            install_found: true,
            system: Some(drive("C:", Attached::Disk, "NVMe", 200)),
            game: Some(drive("C:", Attached::Disk, "NVMe", 200)),
            same_drive: true,
            programs: Programs::default(),
            write_test: WriteTest::Passed,
            landed: None,
            limit: None,
            limit_went: None,
            unlisted: Vec::new(),
            reads: reads(1518.0, 1598.0, 0.1),
            line: Some(1_000_000_000),
            room: None,
            files: Files::At { unread: 0 },
            files_by_length: false,
        }
    }

    /// A seat whose game disk comes over the network, next to a system disk of its own.
    fn seat_with_network_game_disk() -> Evidence {
        Evidence { game: Some(drive("D:", Attached::Network, "iSCSI", 855)), same_drive: false, ..home() }
    }

    /// A volume as the first PC room seat this was run on reported its two: a disk that
    /// says it is attached by Fibre Channel, named by its server, and silent about seeking.
    fn served(letter: &str, bus: Bus, model: &str, free: u64) -> Volume {
        Volume {
            letter: letter.to_owned(),
            kind: DriveKind::Fixed,
            file_system: Ok("NTFS".to_owned()),
            label: String::new(),
            read_only: false,
            bus: Ok(bus),
            model: model.to_owned(),
            seek_penalty: Err(Unknown::Windows(1)),
            total: Ok(3722 * GIB),
            free: Ok(free),
            disk: Ok(1),
        }
    }

    /// What that seat showed, without the write probe: both disks from the server, reads at
    /// the speed of its 1 Gbps line, the iSCSI port driver loaded, 피카's programs running.
    /// `bus` is what its disks say they are attached by; the seat's said Fibre Channel.
    fn pc_room_seat(bus: Bus) -> Evidence {
        let nobody = Scrub::new(Vec::new(), Vec::new());
        Evidence {
            system: Some(Drive::of(&served("C:", bus, "NSM 22H2-H610 232GB", 154 * GIB + 307 * MIB), &nobody)),
            game: Some(Drive::of(&served("G:", bus, "NSM Logical_G 3722GB", 305 * GIB + 205 * MIB), &nobody)),
            same_drive: false,
            programs: Programs {
                seat: names(&["PicaigreenClient.exe", "PicaVncSvc.exe", "pmclient.exe"]),
                iscsi: names(&["iScsiPrt"]),
                spare: names(&["cht4iscsi"]),
                noted: names(&["ahcache", "NVHDA"]),
                ..Programs::default()
            },
            reads: reads(102.0, 105.0, 0.4),
            line: Some(1_000_000_000),
            files: Files::At { unread: 0 },
            ..home()
        }
    }

    /// What that seat's write probe showed: 512 MB in one file at 414 MB/s, the room back
    /// on the drive after the removal, and 808 and then 831 MB gone from the 20.1 GB of
    /// memory. One reading: memory may have gone with the write, or for another reason.
    fn seats_probe() -> Option<Landed> {
        let before = 20 * GIB + 102 * MIB;
        Some(Landed {
            written: 512 * MIB,
            speed: 414.0 * MB_S,
            keeps: Keeps::Memory { steady: false },
            went: Went::Unseen,
            memory: Some((before, 808 * MIB)),
            dip: Some(808 * MIB),
        })
    }

    /// What the probe as it is now shows on a seat that keeps its writes in memory: a
    /// gigabyte in four files, and memory down by about what each wrote, after each.
    fn steady_probe(available: u64) -> Option<Landed> {
        Some(Landed {
            written: GIB,
            speed: 414.0 * MB_S,
            keeps: Keeps::Memory { steady: true },
            went: Went::Unseen,
            memory: Some((available, 1100 * MIB)),
            dip: Some(1100 * MIB),
        })
    }

    #[test]
    fn home_pc_is_ordinary_takes_the_patch_and_keeps_it() {
        let verdict = verdict(&home());
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Ordinary, Outlook::Expected, Outlook::Expected));
        // Each answer with the facts it was decided by, and no sign of anything.
        assert_eq!(verdict.kind_grounds, ["이 PC의 NVMe 디스크에 설치"]);
        assert!(verdict.speed_grounds.is_empty());
        assert_eq!(verdict.patch_grounds, ["C: 여유 200.0 GB", "설치 폴더 쓰기 성공", "모든 게임 파일 내용 일치"]);
        assert_eq!(verdict.kept_grounds, ["노하드·복구 프로그램 없음"]);
        // The game on a second disk of the PC changes nothing.
        let two = Evidence { game: Some(drive("D:", Attached::Disk, "SATA", 100)), same_drive: false, ..home() };
        let verdict = super::verdict(&two);
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Ordinary, Outlook::Expected, Outlook::Expected));
        assert_eq!(verdict.kind_grounds, ["이 PC의 SATA 디스크에 설치"]);
        assert_eq!(verdict.patch_grounds[..2], ["D: 여유 100.0 GB", "설치 폴더 쓰기 성공"]);
    }

    /// The seat the rules were corrected on, as it was reported without the write probe.
    #[test]
    fn pc_room_seat_is_a_seat_without_a_disk_and_its_free_room_promises_nothing() {
        let verdict = verdict(&pc_room_seat(Bus::FIBRE_CHANNEL));
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely));
        // What decides comes first, and every sign there is follows it: what the disks
        // are, and the programs and the driver by name. The one driver for a server's
        // adapter is no sign, and is not named.
        assert_eq!(
            verdict.kind_grounds,
            ["Fibre Channel 디스크", "NSM 디스크", "PicaigreenClient.exe, PicaVncSvc.exe, pmclient.exe", "iScsiPrt",]
        );
        assert_eq!(verdict.speed_grounds, ["네트워크 전송 속도와 유사한 읽기 패턴"]);
        // The 305 GB the game's drive shows as free are not what the seat can take.
        // Nothing was measured of that, and nothing is said.
        assert!(verdict.patch_grounds.is_empty(), "{:?}", verdict.patch_grounds);
        assert_eq!(verdict.kept_grounds, ["노하드 좌석은 재부팅·로그아웃 시 변경 내용 초기화"]);
    }

    /// The same seat with what its write probe showed then: one file, and memory down by
    /// more than was written. One reading may be an accident, and memory would only be
    /// where the writes go first: nothing is told of the patch.
    #[test]
    fn pc_room_seat_whose_probe_read_memory_once_is_told_nothing_of_the_patch() {
        let verdict = verdict(&Evidence { landed: seats_probe(), ..pc_room_seat(Bus::FIBRE_CHANNEL) });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely));
        // That the write did not go where the reads come from is one more sign of the seat.
        assert_eq!(verdict.speed_grounds, ["네트워크 전송 속도와 유사한 읽기 패턴", "쓰기 속도가 읽기 속도의 4.1배"]);
        // What the one reading showed is stated as the figures it is.
        assert_eq!(verdict.patch_grounds, ["512 MB 쓰기에 메모리 808 MB 감소", "메모리 여유 20.1 GB"]);
    }

    /// Where memory went down by what was written after each of the probe's steps, the
    /// writes go into memory first. That is all a gigabyte tells: some seats pass their
    /// writes on to the shop's server when memory is full, so what memory there is is the
    /// first stretch and no limit, and the probe by itself never makes a patch unlikely.
    #[test]
    fn seat_whose_writes_go_into_memory_first_is_not_judged_by_the_memory_it_has() {
        let seat = pc_room_seat(Bus::FIBRE_CHANNEL);
        let tight = verdict(&Evidence { landed: steady_probe(20 * GIB + 102 * MIB), ..seat.clone() });
        assert_eq!((tight.kind, tight.patch, tight.kept), (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely));
        assert_eq!(tight.patch_grounds, ["단계마다 쓴 만큼 메모리 감소", "메모리 여유 20.1 GB"]);
        // With more memory than the patch writes it is the same answer.
        let roomy = verdict(&Evidence { landed: steady_probe(48 * GIB), ..seat.clone() });
        assert_eq!((roomy.kind, roomy.patch), (Kind::Diskless, Outlook::Unknown));
        assert_eq!(roomy.patch_grounds[1], "메모리 여유 48.0 GB");
        // In none of these is the test that writes as much as a patch said to be needed,
        // and the answer a player reads is the same.
        for landed in [steady_probe(20 * GIB), steady_probe(48 * GIB), seats_probe()] {
            let verdict = super::verdict(&Evidence { landed, ..seat.clone() });
            assert_eq!(verdict.patch, Outlook::Unknown);
            assert!(
                !verdict.patch_grounds.iter().any(|fact| fact.contains("한도 검사")),
                "{:?}",
                verdict.patch_grounds
            );
            assert_eq!(verdict.patch_plain.reason, "매장의 쓰기 공간이 부족하면 중단");
        }
    }

    /// The second PC room seat this was run on, as 0.2.0 reported it after a run that asked
    /// nothing: three volumes that call themselves NVMe disks and answer about seeking,
    /// named `NetZone NxDDisk`, the game on one of 7.4 TB; reads at 108 MB/s twice on a
    /// 1 Gbps line, a far read in 0.2 ms; the iSCSI port driver and eleven storage drivers
    /// for servers loaded; and GreenGuard.exe, the seat program of a 게토 PC room, which
    /// 0.2.0 listed without knowing it for one.
    fn second_seat() -> Evidence {
        let nobody = Scrub::new(Vec::new(), Vec::new());
        let disk = |letter: &str, free: u64| Volume {
            seek_penalty: Ok(false),
            ..served(letter, Bus::NVME, "NetZone NxDDisk", free)
        };
        let drivers = [
            "3ware",
            "ADP80XX",
            "arcsas",
            "cht4iscsi",
            "megasas",
            "megasas2i",
            "megasas35i",
            "megasr",
            "percsas2i",
            "percsas3i",
            "stexstor",
        ];
        Evidence {
            system: Some(Drive::of(&disk("C:", 43 * GIB), &nobody)),
            game: Some(Drive::of(&disk("D:", 2563 * GIB + 307 * MIB), &nobody)),
            same_drive: false,
            programs: Programs {
                iscsi: names(&["iScsiPrt"]),
                spare: names(&drivers),
                seat: names(&["GreenGuard.exe"]),
                noted: names(&["ahcache", "ShaderCacheClient.exe"]),
                game: names(&["MapleStory.exe"]),
                ..Programs::default()
            },
            reads: Some(Reads {
                bytes: 216 << 20,
                first: 108.0 * MB_S,
                again: 108.0 * MB_S,
                fresh: None,
                seek_ms: Some(0.2),
            }),
            line: Some(1_000_000_000),
            files_by_length: true,
            ..home()
        }
    }

    /// 0.2.0 said of that seat that its kind could not be told and that it would take a
    /// patch, for its disks say they are NVMe disks. It is a seat without a disk, and a
    /// run that asks nothing tells so: by the name of the disks, by the reads, and by
    /// what is loaded.
    #[test]
    fn second_seat_whose_disks_say_they_are_nvme_is_a_seat_without_a_disk_all_the_same() {
        let seat = second_seat();
        // A name that is known for a server's disks outweighs what the disk says it is
        // attached by.
        assert_eq!(seat.game.as_ref().map(|game| game.attached), Some(Attached::Unclear));
        assert_eq!(mark_in("NetZone NxDDisk").as_deref(), Some("NetZone"));
        assert_eq!(mark_in("NxDDisk").as_deref(), Some("nxddisk"));
        let verdict = verdict(&seat);
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely));
        // The eleven drivers for servers are a sign, and are not named beside the others.
        assert_eq!(verdict.kind_grounds, ["NetZone 디스크", "GreenGuard.exe", "iScsiPrt"]);
        assert_eq!(verdict.speed_grounds, ["네트워크 전송 속도와 유사한 읽기 패턴"]);
        assert!(verdict.patch_grounds.is_empty(), "{:?}", verdict.patch_grounds);
        // What a player is told of it.
        assert_eq!(
            verdict.plain().map(|row| (row.answer, row.reason.as_str())),
            [
                ("노하드 PC방 좌석", "게임 디스크를 매장 서버에서 네트워크로 받아 사용"),
                ("가능", "매장의 쓰기 공간이 부족하면 중단"),
                ("패치 초기화", "재부팅·자리 이동 시 다시 패치 필요"),
            ]
        );
        // The name alone, on a disk that says it is an NVMe disk and reads like one, is one
        // sign: not enough for a seat, and enough not to call the disk the PC's own.
        let named = Evidence { game: seat.game.clone(), same_drive: false, ..home() };
        let by_name = super::verdict(&named);
        assert_eq!((by_name.kind, by_name.patch), (Kind::Unknown, Outlook::Unknown));
        assert_eq!(by_name.kind_grounds, ["NetZone 디스크"]);
        // On both of its disks, the name is two signs about a drive: a seat.
        let both = Evidence { game: seat.game.clone(), system: seat.system.clone(), same_drive: false, ..home() };
        assert_eq!(super::verdict(&both).kind, Kind::Diskless);
        // The reads alone, on disks without a name that tells: one sign as well. With
        // what is loaded on that seat beside them, a seat.
        let nvme = |letter: &str, free: u64| Some(drive(letter, Attached::Disk, "NVMe", free));
        let unnamed = Evidence { system: nvme("C:", 43), game: nvme("D:", 2563), ..second_seat() };
        assert_eq!(super::verdict(&unnamed).kind, Kind::Diskless);
        let reads_only = Evidence { programs: Programs::default(), ..unnamed.clone() };
        let by_reads = super::verdict(&reads_only);
        assert_eq!(by_reads.kind, Kind::Unknown);
        // Its disks are not called the PC's own, whatever they say they are attached by.
        assert!(by_reads.kind_grounds.is_empty(), "{:?}", by_reads.kind_grounds);
        assert_eq!(by_reads.speed_grounds, ["네트워크 전송 속도와 유사한 읽기 패턴"]);

        // With its write probe: every step at what the line carries from the first one,
        // and memory where it was. The writes go straight to the shop's server.
        let line = Some(1_000_000_000);
        let on_d = probed([64.0, 91.0, 93.0, 94.0], [111, 0, 0, 0]);
        assert!(matches!(went(&on_d, line), Went::Line { from: 0, memory_held: true, .. }), "{:?}", went(&on_d, line));
        let probed_seat = super::verdict(&Evidence { landed: Landed::of(&on_d, line), ..second_seat() });
        assert_eq!((probed_seat.kind, probed_seat.patch), (Kind::Diskless, Outlook::Unknown));
        assert_eq!(probed_seat.patch_grounds, ["쓰기 속도가 네트워크 전송 속도와 유사", "쓰기 중 메모리 111 MB 감소"]);
        assert_eq!(probed_seat.patch_plain.reason, "매장 서버 공간이 부족하면 중단");
        for (speeds, settled) in [([95.0, 94.0, 93.0, 93.0], 94.0), ([95.0, 96.0, 96.0, 96.0], 96.0)] {
            let went = went(&probed(speeds, [7, 2, 0, 0]), line);
            assert!(
                matches!(went, Went::Line { from: 0, speed, .. } if (speed / MB_S - settled).abs() < 0.1),
                "{went:?}"
            );
        }
    }

    /// The other way round: disks of a PC at home that read slowly are not caught by the
    /// sign that told that seat. The reads of the sample go past Windows' file cache, so a
    /// hard disk reads no faster the second time either; what tells it from a disk that
    /// comes over the line is its read at a far place.
    #[test]
    fn slow_disks_of_a_home_pc_are_not_taken_for_a_line() {
        let on = |megabytes: (f64, f64), seek_ms: Option<f64>, line: u64| Evidence {
            game: Some(drive("D:", Attached::Disk, "SATA", 900)),
            same_drive: false,
            reads: Some(Reads {
                bytes: 256 << 20,
                first: megabytes.0 * MB_S,
                again: megabytes.1 * MB_S,
                fresh: None,
                seek_ms,
            }),
            line: Some(line),
            ..home()
        };
        let gigabit = 1_000_000_000;
        // A hard disk at about what a 1 Gbps line carries, both times, with a far read
        // that takes its arm 11 ms.
        for speeds in [(110.0, 112.0), (118.0, 116.0), (95.0, 97.0)] {
            let verdict = verdict(&on(speeds, Some(11.0), gigabit));
            assert_eq!(
                (verdict.kind, verdict.patch, verdict.kept),
                (Kind::Ordinary, Outlook::Expected, Outlook::Expected)
            );
            assert_eq!(verdict.kind_plain.answer, "일반 PC");
        }
        // The same reads where the far read could not be timed say nothing either way.
        assert_eq!(verdict(&on((110.0, 112.0), None, gigabit)).kind, Kind::Ordinary);
        // The same reads with a far read in a fraction of a millisecond are no hard
        // disk's: that is the seat's sign.
        assert_eq!(verdict(&on((110.0, 112.0), Some(0.2), gigabit)).kind, Kind::Unknown);
        // A SATA disk without moving parts on a 2.5 Gbps line, which carries 298 MB/s:
        // it reads at about that, and as fast as such disks do. No sign.
        assert_eq!(verdict(&on((290.0, 300.0), Some(0.1), 2_500_000_000)).kind, Kind::Ordinary);
        // The NVMe disk of the PC at home, at 1,500 and at 3,500 MB/s.
        assert_eq!(verdict(&home()).kind, Kind::Ordinary);
        let fast = Evidence { reads: reads(3498.0, 3534.0, 0.1), ..home() };
        assert_eq!(verdict(&fast).kind, Kind::Ordinary);
    }

    /// A write probe of four files of 256 MB onto G:, written at `speeds` MB/s, after each
    /// of which `down` MB less memory was available than the 14.1 GB there was before.
    fn probed(speeds: [f64; 4], down: [u64; 4]) -> Landing {
        use crate::facts::{Moved, Step};
        let now = |available: u64| Some(Memory { available, cache: 0, nonpaged: 0, commit: 0 });
        let before = 141 * GIB / 10;
        let steps: Vec<Step> = speeds
            .iter()
            .zip(down)
            .zip(1..)
            .map(|((speed, down), at)| Step {
                bytes: 256 * MIB,
                seconds: 256.0 / speed,
                free: vec![Some(305 * GIB - at * 256 * MIB)],
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
            volumes: vec![Moved {
                letter: "G:".to_owned(),
                written_to: true,
                before: Some(305 * GIB),
                after_write: Some(304 * GIB),
                after_delete: Some(305 * GIB),
                later: Some(305 * GIB),
            }],
            memory: [now(before), now(before - down[3] * MIB), now(before - down[3] * MIB), now(before - 101 * MIB)],
            steps,
            waited: 3.0,
            deleted: true,
        }
    }

    /// What 0.2.0 read on the seat with the probe of four steps, on a 1 Gbps line. On G:
    /// the first half gigabyte went at 1,799 MB/s and the rest at 106 and 105; on C: and
    /// D: every step went at about 110. Memory hardly moved. The writes are carried to the
    /// shop's server at what the line carries, and the verdict says so.
    #[test]
    fn seat_whose_probe_settles_at_the_speed_of_the_line_writes_to_its_shops_server() {
        let line = Some(1_000_000_000);
        let on_g = probed([1799.0, 1799.0, 106.0, 105.0], [3, 47, 162, 154]);
        assert_eq!(keeps(&on_g), Keeps::Unseen);
        let Went::Line { from, speed, memory_held } = went(&on_g, line) else {
            panic!("the line was not seen: {:?}", went(&on_g, line))
        };
        assert_eq!((from, memory_held), (512 * MIB, true));
        assert!((105.9 * MB_S..106.1 * MB_S).contains(&speed), "{speed}");
        // The most memory was down while it wrote, by the readings after its steps.
        assert_eq!(dip(&on_g), Some(162 * MIB));
        // The system's volume and the third one: at the speed of the line from the first
        // step on.
        for (speeds, down, settled) in [
            ([124.0, 114.0, 110.0, 109.0], [54, 53, 78, 85], 114.0),
            ([121.0, 110.0, 108.0, 111.0], [89, 123, 183, 150], 111.0),
        ] {
            let went = went(&probed(speeds, down), line);
            assert!(
                matches!(went, Went::Line { from: 0, speed, memory_held: true } if (speed / MB_S - settled).abs() < 0.1),
                "{went:?}"
            );
        }

        let seat = pc_room_seat(Bus::FIBRE_CHANNEL);
        let verdict = verdict(&Evidence { landed: Landed::of(&on_g, line), ..seat.clone() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely));
        // What was seen is said: where the writing settled, and what memory did.
        assert_eq!(verdict.patch_grounds, ["쓰기 후반 속도가 네트워크 전송 속도와 유사", "쓰기 중 메모리 162 MB 감소"]);
        assert_eq!(verdict.patch_plain.reason, "매장 서버 공간이 부족하면 중단");
        // Without a wired line, and for a probe that is fast in every step, the speed
        // shows nothing, and that nothing moved with the writes is all there is to say.
        assert_eq!(went(&on_g, None), Went::Unseen);
        let fast = probed([1799.0; 4], [3, 5, 4, 6]);
        assert_eq!(went(&fast, line), Went::Unseen);
        let unseen = super::verdict(&Evidence { landed: Landed::of(&fast, line), ..seat });
        assert_eq!(unseen.patch_grounds, ["쓴 만큼 줄어든 메모리·드라이브 없음"]);
        assert_eq!(unseen.patch_plain.reason, "매장의 쓰기 공간이 부족하면 중단");
    }

    /// The seat's own run of the write limit test, as 0.2.0 saw its first four steps:
    /// 1,291 MB/s, then 107, 106 and 107 on a 1 Gbps line, with memory where it was.
    /// 0.2.0 took that for a collapse and ended the run at 4 GB. It is the hand-over to
    /// the shop's server: the run goes on at that speed, to the end.
    #[cfg(windows)]
    #[test]
    fn the_seats_own_limit_run_goes_on_past_the_hand_over_to_the_end() {
        let before = 138 * GIB / 10;
        let seen = |at: u64| match at {
            1 => 1291.0,
            2 | 4 => 107.0,
            _ => 106.0,
        };
        let landing = limit_run(before, |at| (seen(at), before - 37 * MIB));
        assert_eq!((landing.written, landing.halted, landing.steps.len()), (ROOM_NEEDED, None, 37));
        let verdict = after_the_limit_test(&landing);
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Expected, Outlook::Unlikely));
        assert_eq!(verdict.patch_grounds, ["한도 검사 36.5 GB 쓰기 성공", "1.0 GB부터 네트워크 전송 속도와 유사"]);
    }

    /// The write limit test as it would go on a seat whose readings are made up, on a
    /// 1 Gbps line: `reads` gives, for each step counted from one, the megabytes a second
    /// it is written at and the memory that is available after it. The steps are taken
    /// for as long as the rules of the writing let it go on, as the test itself takes them.
    #[cfg(windows)]
    fn limit_run(before: u64, reads: impl Fn(u64) -> (f64, u64)) -> Landing {
        use crate::facts::{Moved, Step};
        use crate::probe::write::{Fill, LIMIT_STEP, MEMORY_FLOOR, SETTLE, Start, halts};
        let now = |available: u64| Some(Memory { available, cache: 0, nonpaged: 0, commit: 0 });
        let plan = Fill::limit(ROOM_NEEDED, LIMIT_STEP, MEMORY_FLOOR, SETTLE).on_line(Some(1_000_000_000));
        let free = [Some(305 * GIB)];
        let start = Start { free: &free, written_to: Some(0), memory: now(before) };
        let (mut steps, mut written, mut halted) = (Vec::<Step>::new(), 0u64, None);
        while written < plan.total {
            let next = plan.step.min(plan.total - written);
            halted = halts(&plan, &start, &steps, next);
            if halted.is_some() {
                break;
            }
            let (megabytes, available) = reads(steps.len() as u64 + 1);
            written += next;
            steps.push(Step {
                bytes: next,
                seconds: next as f64 / (megabytes * MB_S),
                free: vec![Some(305 * GIB - written)],
                memory: now(available),
            });
        }
        let last = steps.last().and_then(|step| step.memory);
        Landing {
            place: Place::Install,
            written,
            wanted: plan.total,
            seconds: steps.iter().map(|step| step.seconds).sum(),
            error: None,
            halted,
            volumes: vec![Moved {
                letter: "G:".to_owned(),
                written_to: true,
                before: Some(305 * GIB),
                after_write: Some(305 * GIB - written),
                after_delete: Some(305 * GIB),
                later: Some(305 * GIB),
            }],
            memory: [now(before), last, last, last],
            steps,
            waited: 3.0,
            deleted: true,
        }
    }

    /// The verdict on the seat after such a run of the write limit test.
    #[cfg(windows)]
    fn after_the_limit_test(landing: &Landing) -> Verdict {
        let seat = pc_room_seat(Bus::FIBRE_CHANNEL);
        verdict(&Evidence { limit: Limit::of(landing), limit_went: Some(went(landing, seat.line)), ..seat })
    }

    /// A seat that takes its writes into memory for the first three gigabytes and then
    /// passes them on to its shop's server: memory stops falling, and the writing goes on
    /// at what the line carries. The test does not end itself for that, writes all of it,
    /// and says where the writes went.
    #[cfg(windows)]
    #[test]
    fn seat_that_passes_its_writes_on_to_the_server_takes_the_whole_test_and_the_patch() {
        let before = 20 * GIB + 102 * MIB;
        let landing =
            limit_run(before, |at| if at <= 3 { (414.0, before - at * GIB) } else { (105.0, before - 3 * GIB) });
        assert_eq!((landing.written, landing.halted, landing.steps.len()), (ROOM_NEEDED, None, 37));
        assert_eq!(Limit::of(&landing), Some(Limit::Passed { written: ROOM_NEEDED }));
        let Went::Line { from, speed, memory_held } = went(&landing, Some(1_000_000_000)) else {
            panic!("the hand-over was not seen: {:?}", went(&landing, Some(1_000_000_000)))
        };
        assert_eq!((from, memory_held), (3 * GIB, true));
        assert!((104.9 * MB_S..105.1 * MB_S).contains(&speed), "{speed}");
        let verdict = after_the_limit_test(&landing);
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Expected, Outlook::Unlikely));
        assert_eq!(verdict.patch_grounds, ["한도 검사 36.5 GB 쓰기 성공", "3.0 GB부터 네트워크 전송 속도와 유사"]);
        // The same speeds on a PC with a disk of its own are a disk that got slower:
        // stated as the figure it is, on either kind of PC.
        let at_home = super::verdict(&Evidence {
            limit: Limit::of(&landing),
            limit_went: Some(went(&landing, Some(1_000_000_000))),
            ..home()
        });
        assert_eq!(at_home.patch, Outlook::Expected);
        assert_eq!(at_home.patch_grounds[1], "3.0 GB부터 네트워크 전송 속도와 유사");
        // Without a wired line there is no such speed to hold the steps against.
        assert_eq!(went(&landing, None), Went::Unseen);

        // A seat with more memory than a patch writes, which keeps it all there: memory
        // falls with every step to the end, never to the floor, and the test goes through.
        let roomy = 48 * GIB;
        let in_memory = limit_run(roomy, |at| (414.0, roomy - at * GIB));
        assert_eq!((in_memory.written, in_memory.halted), (ROOM_NEEDED, None));
        assert_eq!(went(&in_memory, Some(1_000_000_000)), Went::Memory);
        let verdict = after_the_limit_test(&in_memory);
        assert_eq!(verdict.patch, Outlook::Expected);
        assert_eq!(verdict.patch_grounds[1], "쓴 만큼 메모리 감소");
        // Written at the line's speed from the first step on: said of a seat, and not of
        // a PC whose own disk is that fast.
        let served = limit_run(before, |_| (105.0, before - 200 * MIB));
        assert_eq!(after_the_limit_test(&served).patch_grounds[1], "처음부터 네트워크 전송 속도와 유사");
        let at_home = super::verdict(&Evidence {
            limit: Limit::of(&served),
            limit_went: Some(went(&served, Some(1_000_000_000))),
            ..home()
        });
        assert_eq!(at_home.patch_grounds, ["한도 검사 36.5 GB 쓰기 성공"]);
        // And a run of which nothing can be said but that it went through.
        let plain = limit_run(before, |_| (414.0, before - 200 * MIB));
        assert_eq!(went(&plain, Some(1_000_000_000)), Went::Unseen);
        let verdict = after_the_limit_test(&plain);
        assert_eq!(verdict.patch, Outlook::Expected);
        assert_eq!(verdict.patch_grounds, ["한도 검사 36.5 GB 쓰기 성공"]);
    }

    /// Memory falls by what is written, step after step, all the way to what the test
    /// leaves of it: memory is the one place the seat has, and it holds less than a patch
    /// writes. The test ends itself there, and that is a measurement.
    #[cfg(windows)]
    #[test]
    fn seat_whose_memory_falls_to_the_floor_is_measured_and_takes_no_patch() {
        let before = 20 * GIB + 102 * MIB;
        let landing = limit_run(before, |at| (414.0, before - at * GIB));
        assert_eq!((landing.written, landing.halted), (16 * GIB, Some(Halt::Memory)));
        let verdict = after_the_limit_test(&landing);
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Unlikely, Outlook::Unlikely));
        assert_eq!(verdict.patch_grounds, ["한도 검사 16.0 GB에서 중단", "메모리 부족"]);
    }

    /// The writing falls far under what the line carries, for two steps: the test ends
    /// itself. How much the seat takes was not found out: only how far it got is stated.
    #[cfg(windows)]
    #[test]
    fn seat_whose_writing_all_but_stops_ends_the_test_and_is_told_nothing() {
        let before = 20 * GIB + 102 * MIB;
        let landing = limit_run(before, |at| (if at <= 5 { 414.0 } else { 15.0 }, before - 300 * MIB));
        assert_eq!((landing.written, landing.halted), (7 * GIB, Some(Halt::Slow)));
        let verdict = after_the_limit_test(&landing);
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely));
        // How far it got is a fact. That it settled nothing shows in the answer.
        assert_eq!(verdict.patch_grounds, ["한도 검사 7.0 GB에서 중단"]);
        assert_eq!(verdict.patch_plain.answer, "가능");
    }

    /// Nothing but the volume written to moved: what takes the seat's writes is out of
    /// sight, and that nothing moved is all that is stated.
    #[test]
    fn seat_whose_writes_show_nowhere_has_a_limit_that_cannot_be_seen() {
        let unseen = Landed {
            keeps: Keeps::Unseen,
            went: Went::Unseen,
            memory: Some((20 * GIB, 12 * MIB)),
            dip: Some(12 * MIB),
            ..steady_probe(0).unwrap()
        };
        let verdict = verdict(&Evidence { landed: Some(unseen), ..pc_room_seat(Bus::FIBRE_CHANNEL) });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely));
        assert_eq!(verdict.patch_grounds, ["쓴 만큼 줄어든 메모리·드라이브 없음"]);
    }

    /// The test that writes as much as a main patch does is the one thing that was tried.
    /// Where it was run to its end, it decides, on any kind of PC.
    #[test]
    fn write_limit_test_decides_the_patch_whatever_kind_of_pc_it_is() {
        let wrote_it_all = Some(Limit::Passed { written: ROOM_NEEDED });
        let stopped = Some(Limit::Stopped { written: 21 * GIB, error: Some(112), halted: None });
        for (pc, kind, kept) in [
            (pc_room_seat(Bus::FIBRE_CHANNEL), Kind::Diskless, Outlook::Unlikely),
            (Evidence { landed: seats_probe(), ..pc_room_seat(Bus::SCSI) }, Kind::Diskless, Outlook::Unlikely),
            (home(), Kind::Ordinary, Outlook::Expected),
            (
                Evidence { game: Some(drive("D:", Attached::Unclear, "SAS", 500)), same_drive: false, ..home() },
                Kind::Unknown,
                Outlook::Unknown,
            ),
        ] {
            let passed = verdict(&Evidence { limit: wrote_it_all, ..pc.clone() });
            assert_eq!((passed.kind, passed.patch, passed.kept), (kind, Outlook::Expected, kept), "{pc:?}");
            assert_eq!(passed.patch_grounds, ["한도 검사 36.5 GB 쓰기 성공"]);
            let failed = verdict(&Evidence { limit: stopped, ..pc.clone() });
            assert_eq!((failed.kind, failed.patch, failed.kept), (kind, Outlook::Unlikely, kept), "{pc:?}");
            assert_eq!(failed.patch_grounds, ["한도 검사 21.0 GB에서 중단", "쓰기 실패 (오류 112)"]);
        }
        // The test ended itself because what took its writes was nearly used up: that is
        // a measurement of how much the PC takes, and it was less than a patch writes.
        let ended = |written: u64, halt: Halt| Some(Limit::Stopped { written, error: None, halted: Some(halt) });
        let seat = pc_room_seat(Bus::FIBRE_CHANNEL);
        for (halt, why) in
            [(Halt::Memory, "메모리 부족"), (Halt::Elsewhere, "다른 드라이브 여유 부족"), (Halt::Drive, "G: 여유 부족")]
        {
            let verdict = verdict(&Evidence { limit: ended(16 * GIB, halt), ..seat.clone() });
            assert_eq!(verdict.patch, Outlook::Unlikely);
            assert_eq!(verdict.patch_grounds, ["한도 검사 16.0 GB에서 중단", why]);
        }
        // The volume that went down with the writing is named where it was seen.
        let on_d = Evidence {
            limit: ended(16 * GIB, Halt::Elsewhere),
            limit_went: Some(Went::Volume { letter: "D:".to_owned() }),
            ..seat.clone()
        };
        assert_eq!(verdict(&on_d).patch_grounds[1], "D: 여유 부족");
        // It ended itself because the writing got slow: how much the PC takes was not
        // found out. A PC at home whose disk slows down when its fast part is full takes
        // the patch all the same, and a seat is as open as without the test.
        for halt in [Halt::Slow, Halt::TimedOut] {
            let at_home = verdict(&Evidence { limit: ended(20 * GIB, halt), ..home() });
            assert_eq!(at_home.patch, Outlook::Expected);
            assert_eq!(
                at_home.patch_grounds,
                ["C: 여유 200.0 GB", "설치 폴더 쓰기 성공", "한도 검사 20.0 GB에서 중단", "모든 게임 파일 내용 일치"]
            );
            let on_a_seat = verdict(&Evidence { limit: ended(20 * GIB, halt), ..seat.clone() });
            assert_eq!(on_a_seat.patch, Outlook::Unknown);
            assert_eq!(on_a_seat.patch_grounds, ["한도 검사 20.0 GB에서 중단"]);
        }
        // A test that was run with less than a main patch writes decides nothing: what
        // the drive says decides, as without it.
        let small = Some(Limit::Passed { written: 300 * MIB });
        let seat = verdict(&Evidence { limit: small, ..pc_room_seat(Bus::FIBRE_CHANNEL) });
        assert_eq!(seat.patch, Outlook::Unknown);
        assert_eq!(seat.patch_grounds, ["한도 검사는 300 MB만 씀"]);
        assert_eq!(verdict(&Evidence { limit: small, ..home() }).patch, Outlook::Expected);
        // Files that are not Nexon's stop a patch even where the writing went through.
        let files = Files::Differs { differs: 2, missing: 0 };
        let differs = verdict(&Evidence { limit: wrote_it_all, files, ..home() });
        assert_eq!(differs.patch, Outlook::Unlikely);
        assert_eq!(differs.patch_grounds, ["게임 파일 2개 변경"]);
    }

    #[test]
    fn how_the_write_limit_test_ended_is_read_from_what_it_noted() {
        let test = |written: u64, error: Option<i32>, halted: Option<Halt>| Landing {
            place: Place::Install,
            written,
            wanted: ROOM_NEEDED,
            seconds: 100.0,
            error,
            halted,
            steps: Vec::new(),
            volumes: Vec::new(),
            memory: [None; 4],
            waited: 5.0,
            deleted: true,
        };
        assert_eq!(Limit::of(&test(ROOM_NEEDED, None, None)), Some(Limit::Passed { written: ROOM_NEEDED }));
        assert_eq!(
            Limit::of(&test(21 * GIB, Some(112), None)),
            Some(Limit::Stopped { written: 21 * GIB, error: Some(112), halted: None })
        );
        // An error without a number, and the floor.
        assert_eq!(
            Limit::of(&test(GIB, Some(0), None)),
            Some(Limit::Stopped { written: GIB, error: None, halted: None })
        );
        assert_eq!(
            Limit::of(&test(30 * GIB, None, Some(Halt::Drive))),
            Some(Limit::Stopped { written: 30 * GIB, error: None, halted: Some(Halt::Drive) })
        );
        // Ended by the user: that says nothing of how far it would have got.
        assert_eq!(Limit::of(&test(12 * GIB, None, None)), None);
        assert_eq!(Limit::of(&test(0, None, None)), None);
    }

    /// The seat's disks claimed Fibre Channel. Had they claimed anything else, the signs
    /// would have told all the same, with the probe and without it.
    #[test]
    fn pc_room_seat_is_told_by_its_signs_whatever_its_disks_say_they_are_attached_by() {
        for bus in [Bus::SCSI, Bus::SATA, Bus::NVME, Bus(10), Bus(0)] {
            let seat = pc_room_seat(bus);
            let without = verdict(&seat);
            assert_eq!(
                (without.kind, without.patch, without.kept),
                (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely),
                "{bus:?}"
            );
            // No bus that a storage network uses is named: the name of the disks is first.
            assert_eq!(without.kind_grounds[0], "NSM 디스크", "{bus:?}");
            let with = verdict(&Evidence { landed: seats_probe(), ..seat });
            assert_eq!(
                (with.kind, with.patch, with.kept),
                (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely),
                "{bus:?}"
            );
        }
        // And without any name this program knows: the disks' names, the reads and the
        // iSCSI port driver are three signs, two of them about a drive.
        let programs = Programs { iscsi: names(&["iScsiPrt"]), ..Programs::default() };
        assert_eq!(verdict(&Evidence { programs, ..pc_room_seat(Bus::SCSI) }).kind, Kind::Diskless);
    }

    #[test]
    fn one_sign_alone_does_not_make_a_seat_and_takes_away_what_was_expected() {
        // Each of these on a PC that otherwise looks like one at home.
        let marked = Drive { marked: Some("virtual".to_owned()), ..drive("D:", Attached::Unclear, "SAS", 500) };
        let alone = [
            // Reads at the speed of the cable with the latency of one: a game on a slow
            // stick does that too.
            Evidence { reads: reads(102.0, 105.0, 0.4), ..home() },
            // A second read many times faster: a cache below Windows.
            Evidence { reads: reads(80.0, 1856.0, 0.4), line: None, ..home() },
            // A disk whose name marks it as made up.
            Evidence { game: Some(marked), same_drive: false, ..home() },
            // A write far faster than the drive reads, by one probe: a drive that is told
            // not to wait for its own cache does that too.
            Evidence { reads: reads(150.0, 152.0, 9.0), landed: seats_probe(), ..home() },
        ];
        let stated = [
            "네트워크 전송 속도와 유사한 읽기 패턴",
            "반복 읽기가 23배 빠름",
            "디스크 이름에 virtual 포함",
            "쓰기 속도가 읽기 속도의 2.8배",
        ];
        for (evidence, sign) in alone.into_iter().zip(stated) {
            let verdict = verdict(&evidence);
            assert_eq!(
                (verdict.kind, verdict.patch, verdict.kept),
                (Kind::Unknown, Outlook::Unknown, Outlook::Unknown),
                "{evidence:?}"
            );
            // The one sign is stated, and the disks are not called the PC's own beside it.
            let grounds: Vec<&String> = verdict.kind_grounds.iter().chain(&verdict.speed_grounds).collect();
            assert_eq!(grounds, [sign], "{evidence:?}");
        }
        // With a PC room's program beside it, each of them is a seat without a disk.
        let programs = Programs { seat: names(&["pmclient.exe"]), ..Programs::default() };
        let in_a_shop = verdict(&Evidence { reads: reads(80.0, 1856.0, 0.4), line: None, programs, ..home() });
        assert_eq!(
            (in_a_shop.kind, in_a_shop.patch, in_a_shop.kept),
            (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely)
        );
        assert_eq!(in_a_shop.kind_grounds, ["pmclient.exe"]);
        assert_eq!(in_a_shop.speed_grounds, ["반복 읽기가 23배 빠름"]);
    }

    /// The other way round: nothing that a home PC does by itself makes it a seat.
    #[test]
    fn home_pc_is_not_taken_for_a_seat_by_what_home_pcs_do() {
        // Memory dipped by more than the probe wrote and stayed down, as it does on a PC
        // where a game runs. The write was no faster than the NVMe disk reads.
        let dipped = Landed {
            written: 512 * MIB,
            speed: 1291.0 * MB_S,
            keeps: Keeps::Memory { steady: false },
            went: Went::Unseen,
            memory: Some((3 * GIB, 900 * MIB)),
            dip: Some(900 * MIB),
        };
        assert!(dipped.times_faster_than(Some(1518.0 * MB_S)).is_none());
        let verdict = verdict(&Evidence { landed: Some(dipped.clone()), ..home() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Ordinary, Outlook::Expected, Outlook::Expected));
        assert!(!verdict.kind_grounds.iter().chain(&verdict.patch_grounds).any(|fact| fact.contains("메모리")));
        assert!(verdict.speed_grounds.is_empty());
        // A first read that was held up, on a disk that was busy just then: the second is
        // three and a half times as fast, and so are bytes that were not read before.
        let held_up = Reads { fresh: Some(1650.0 * MB_S), ..reads(482.0, 1700.0, 0.1).unwrap() };
        let verdict = super::verdict(&Evidence { reads: Some(held_up), ..home() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Ordinary, Outlook::Expected, Outlook::Expected));
        // Where the bytes that were not read before come as slowly as the first ones did,
        // something below Windows kept what was read: that is a sign.
        let kept = Reads { fresh: Some(85.0 * MB_S), ..reads(80.0, 1856.0, 0.4).unwrap() };
        let verdict = super::verdict(&Evidence { reads: Some(kept), line: None, ..home() });
        assert_eq!(verdict.kind, Kind::Unknown);
        assert_eq!(verdict.speed_grounds, ["반복 읽기가 23배 빠름"]);
        // A hard disk that happens to read as fast as the 1 Gbps cable carries, and seeks
        // like a hard disk; and the same with a probe that wrote as fast as it reads.
        let hard_disk = Evidence {
            game: Some(drive("D:", Attached::Disk, "SATA", 900)),
            same_drive: false,
            reads: reads(108.0, 110.0, 12.5),
            ..home()
        };
        assert_eq!(super::verdict(&hard_disk).kind, Kind::Ordinary);
        let as_fast = Landed { speed: 140.0 * MB_S, ..dipped };
        assert_eq!(super::verdict(&Evidence { landed: Some(as_fast), ..hard_disk }).kind, Kind::Ordinary);
        // Reads over Wi-Fi are held against no line, and reads that were not timed against
        // nothing.
        let wireless = Evidence { line: None, reads: reads(102.0, 105.0, 0.4), ..home() };
        assert_eq!(super::verdict(&wireless).kind, Kind::Ordinary);
        assert_eq!(super::verdict(&Evidence { reads: None, ..home() }).kind, Kind::Ordinary);
    }

    #[test]
    fn reads_count_as_coming_over_the_line_only_inside_the_band_and_with_its_latency() {
        let kind = |first: f64, again: f64, ms: f64, line: u64| {
            let programs = Programs { seat: names(&["pmclient.exe"]), ..Programs::default() };
            verdict(&Evidence { reads: reads(first, again, ms), line: Some(line), programs, ..home() }).kind
        };
        const GBIT: u64 = 1_000_000_000;
        // 1 Gbps carries 119 MB/s as speeds are counted here. A busy server gives 70.
        assert_eq!(kind(102.0, 105.0, 0.4, GBIT), Kind::Diskless);
        assert_eq!(kind(70.0, 72.0, 0.9, GBIT), Kind::Diskless);
        assert_eq!(kind(250.0, 255.0, 0.3, 2_500_000_000), Kind::Diskless);
        // A far read that is answered as fast as an SSD answers, on a drive that reads a
        // long file at what a 1 Gbps line carries: no SSD reads that slowly, twice over.
        // The second seat answered in 0.2 ms.
        assert_eq!(kind(102.0, 105.0, 0.1, GBIT), Kind::Diskless);
        assert_eq!(kind(108.0, 108.0, 0.2, GBIT), Kind::Diskless);
        // Faster than the line carries, far below it, and as slow as a hard disk at a far
        // place: none of these is a read over the line. A PC room's program without a
        // sign about a drive leaves the kind open.
        for (first, again, ms) in [(500.0, 510.0, 0.4), (30.0, 31.0, 0.4), (102.0, 105.0, 9.0), (102.0, 105.0, 5.0)] {
            assert_eq!(kind(first, again, ms, GBIT), Kind::Unknown, "{first} {again} {ms}");
        }
        // On a faster line, what it carries is what an SSD reads at anyway: there the far
        // read has to take what a trip over the line takes.
        assert_eq!(kind(250.0, 255.0, 0.1, 2_500_000_000), Kind::Unknown);
        assert_eq!(kind(190.0, 195.0, 0.1, 2_500_000_000), Kind::Diskless);
    }

    #[test]
    fn signs_about_the_pc_alone_never_make_a_seat_of_a_pc_with_its_own_disks() {
        // A seat with a disk of its own in a room that reaches other disks by iSCSI: 피카's
        // program, the iSCSI port driver, and drivers of a Windows made for many PCs.
        let programs = Programs {
            seat: names(&["pmclient.exe"]),
            iscsi: names(&["iScsiPrt"]),
            spare: names(&["3ware", "cht4iscsi"]),
            ..Programs::default()
        };
        let verdict = verdict(&Evidence { programs: programs.clone(), ..home() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Unknown, Outlook::Expected, Outlook::Unknown));
        // The drivers that come with Windows are not named beside the others.
        assert_eq!(verdict.kind_grounds, ["이 PC의 NVMe 디스크에 설치", "pmclient.exe", "iScsiPrt"]);
        assert_eq!(verdict.kept_grounds, ["알려진 복구 프로그램 없음"]);
        // By themselves they are counted: they are what keeps the PC from being ordinary.
        let two = Programs { spare: names(&["3ware", "cht4iscsi"]), ..Programs::default() };
        let by_drivers = super::verdict(&Evidence { programs: two, ..home() });
        assert_eq!(by_drivers.kind, Kind::Unknown);
        assert_eq!(by_drivers.kind_grounds, ["이 PC의 NVMe 디스크에 설치", "서버용 저장 드라이버 2개"]);
        // One such driver may be there for an adapter the PC has: it is no sign, and a
        // home PC stays one. The iSCSI port driver alone leaves the kind open.
        let one = Programs { spare: names(&["megasas35i"]), ..Programs::default() };
        assert_eq!(super::verdict(&Evidence { programs: one, ..home() }).kind, Kind::Ordinary);
        let iscsi = Programs { iscsi: names(&["iScsiPrt"]), ..Programs::default() };
        let verdict = super::verdict(&Evidence { programs: iscsi, ..home() });
        assert_eq!((verdict.kind, verdict.patch), (Kind::Unknown, Outlook::Expected));
        // With a restore program known by name, such a seat is a disk with a restore program.
        let restored = Programs { restore: names(&["DFServ.exe"]), ..programs };
        assert_eq!(super::verdict(&Evidence { programs: restored, ..home() }).kind, Kind::DiskWithRestore);
    }

    #[test]
    fn disk_that_comes_over_the_network_makes_a_diskless_seat() {
        for evidence in [
            seat_with_network_game_disk(),
            // Windows itself from the network, and both on one such disk.
            Evidence { system: Some(drive("C:", Attached::Network, "iSCSI", 30)), same_drive: false, ..home() },
            Evidence {
                system: Some(drive("C:", Attached::Network, "iSCSI", 30)),
                game: Some(drive("C:", Attached::Network, "iSCSI", 30)),
                ..home()
            },
        ] {
            let verdict = verdict(&evidence);
            assert_eq!(verdict.kind, Kind::Diskless, "{evidence:?}");
            // How much such a seat takes cannot be seen from the seat, whatever is free.
            assert_eq!(verdict.patch, Outlook::Unknown, "{evidence:?}");
            assert_eq!(verdict.kept, Outlook::Unlikely, "{evidence:?}");
            assert_eq!(verdict.kind_grounds, ["iSCSI 디스크"]);
            // The 855 GB the server's disk has free are not said to be the seat's.
            assert!(verdict.patch_grounds.is_empty(), "{:?}", verdict.patch_grounds);
        }
    }

    #[test]
    fn known_diskless_program_makes_a_diskless_seat_whatever_the_disks_claim() {
        let programs = Programs { diskless: names(&["PFGameDisk.exe"]), ..Programs::default() };
        let verdict = verdict(&Evidence { programs, ..home() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely));
        assert_eq!(verdict.kind_grounds, ["PFGameDisk.exe"]);
    }

    #[test]
    fn real_disk_with_a_restore_program_takes_the_patch_and_may_lose_it() {
        let programs =
            Programs { restore: names(&["DFServ.exe"]), seat: names(&["pmclient.exe"]), ..Programs::default() };
        let verdict = verdict(&Evidence { programs, ..home() });
        assert_eq!(verdict.kind, Kind::DiskWithRestore);
        assert_eq!(verdict.patch, Outlook::Expected);
        assert_eq!(verdict.kept, Outlook::Unknown);
        assert_eq!(verdict.kind_grounds, ["이 PC의 NVMe 디스크에 설치", "DFServ.exe"]);
        assert_eq!(verdict.kept_grounds, ["DFServ.exe 실행 중", "게임 디스크의 복구 제외 여부 미확인"]);
    }

    #[test]
    fn made_up_disk_is_a_seat_only_with_another_sign_beside_it() {
        let on_virtual =
            Evidence { game: Some(drive("D:", Attached::Virtual, "가상", 500)), same_drive: false, ..home() };
        // At home it may be a virtual disk somebody mounted: the facts do not decide.
        let alone = verdict(&on_virtual);
        assert_eq!((alone.kind, alone.patch, alone.kept), (Kind::Unknown, Outlook::Unknown, Outlook::Unknown));
        assert_eq!(alone.kind_grounds, ["D: 가상 디스크"]);

        let programs = Programs { seat: names(&["pmclient.exe"]), ..Programs::default() };
        let in_a_shop = verdict(&Evidence { programs, ..on_virtual });
        assert_eq!((in_a_shop.kind, in_a_shop.kept), (Kind::Diskless, Outlook::Unlikely));
        assert_eq!(in_a_shop.kind_grounds, ["D: 가상 디스크", "pmclient.exe"]);
    }

    #[test]
    fn shop_program_on_real_disks_leaves_the_kind_open_and_the_patch_possible() {
        // A seat with a disk: the program that manages the seat runs, and no restore
        // program this tool knows by name. One may run all the same.
        let programs = Programs { seat: names(&["pmclient.exe", "pmAgent.exe"]), ..Programs::default() };
        let verdict = verdict(&Evidence { programs, ..home() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Unknown, Outlook::Expected, Outlook::Unknown));
        assert_eq!(verdict.kind_grounds, ["이 PC의 NVMe 디스크에 설치", "pmclient.exe, pmAgent.exe"]);
        assert_eq!(verdict.kept_grounds, ["알려진 복구 프로그램 없음"]);
    }

    #[test]
    fn name_that_only_hints_at_a_shop_keeps_a_pc_from_being_called_ordinary() {
        let programs = Programs { hints: names(&["HDRestore.exe"]), ..Programs::default() };
        let verdict = verdict(&Evidence { programs, ..home() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Unknown, Outlook::Expected, Outlook::Unknown));
        assert_eq!(verdict.kind_grounds, ["이 PC의 NVMe 디스크에 설치", "HDRestore.exe (이름이 매장 프로그램과 유사)"]);
        // A name that is known to be something else is no hint.
        let programs = Programs { noted: names(&["wireguard.exe", "ahcache"]), ..Programs::default() };
        assert_eq!(super::verdict(&Evidence { programs, ..home() }).kind, Kind::Ordinary);
    }

    #[test]
    fn drive_that_could_be_either_leaves_everything_open() {
        let silent = Drive { seek_unanswered: true, ..drive("D:", Attached::Unclear, "SATA", 500) };
        for (unclear, stated) in [
            (drive("D:", Attached::Unclear, "SCSI", 500), "D: SCSI 드라이브"),
            (drive("", Attached::Share, "네트워크", 500), "공유 폴더"),
            (drive("R:", Attached::Unclear, "램 디스크", 500), "R: 램 디스크 드라이브"),
            // A disk that would not answer about seeking is said to be that.
            (silent, "D: SATA 드라이브, 탐색 지연 응답 없음"),
        ] {
            let verdict = verdict(&Evidence { game: Some(unclear.clone()), same_drive: false, ..home() });
            assert_eq!(
                (verdict.kind, verdict.patch, verdict.kept),
                (Kind::Unknown, Outlook::Unknown, Outlook::Unknown),
                "{unclear:?}"
            );
            // What stands in the way is said, for the kind and for the patch.
            assert_eq!(verdict.kind_grounds, [stated]);
            assert_eq!(verdict.patch_grounds, [stated]);
        }
        // Nor is anything said of a drive that could not be looked at.
        let verdict = super::verdict(&Evidence { game: None, ..home() });
        assert_eq!((verdict.kind, verdict.patch), (Kind::Unknown, Outlook::Unknown));
        assert_eq!(verdict.kind_grounds, ["게임 디스크 정보 확인 실패"]);
        assert_eq!(verdict.patch_grounds, ["게임 디스크 정보 확인 실패"]);
    }

    #[test]
    fn nothing_is_said_of_a_patch_without_an_install_folder() {
        let verdict = verdict(&Evidence { install_found: false, game: None, files: Files::NotCompared, ..home() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Unknown, Outlook::Unknown, Outlook::Unknown));
        assert_eq!(verdict.kind_grounds, ["설치 폴더를 찾지 못함"]);
        assert!(verdict.patch_grounds.is_empty());
        // A system disk from the network is a seat without a disk all the same.
        let seat = Evidence {
            install_found: false,
            game: None,
            system: Some(drive("C:", Attached::Network, "iSCSI", 30)),
            ..home()
        };
        assert_eq!(super::verdict(&seat).kind, Kind::Diskless);
    }

    #[test]
    fn too_little_room_makes_the_patch_unlikely() {
        let tight = Evidence { game: Some(drive("C:", Attached::Disk, "NVMe", 36)), ..home() };
        let verdict = verdict(&tight);
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Ordinary, Outlook::Unlikely, Outlook::Expected));
        assert_eq!(verdict.patch_grounds, ["C: 여유 36.0 GB", "일반 패치 필요 36.5 GB"]);
        // With the engine's figures for the waiting patch, both ways of patching are said.
        let room = Some(Room { safe: 71 * GIB / 2, immediate: 9 * GIB });
        let short = Evidence { room, game: Some(drive("C:", Attached::Disk, "NVMe", 20)), ..home() };
        assert_eq!(
            super::verdict(&short).patch_grounds,
            ["C: 여유 20.0 GB", "일반 패치 필요 35.5 GB", "즉시 패치 필요 9.0 GB"]
        );
        // Exactly enough is enough.
        let enough = Drive { free: Some(ROOM_NEEDED), ..drive("C:", Attached::Disk, "NVMe", 0) };
        assert_eq!(super::verdict(&Evidence { game: Some(enough), ..home() }).patch, Outlook::Expected);
        // Room that cannot be read decides nothing.
        let unread = Drive { free: None, ..drive("C:", Attached::Disk, "NVMe", 0) };
        let verdict = super::verdict(&Evidence { game: Some(unread), ..home() });
        assert_eq!(verdict.patch, Outlook::Unknown);
        assert_eq!(verdict.patch_grounds, ["C: 여유 확인 실패"]);
    }

    #[test]
    fn folder_that_cannot_be_written_or_differs_from_nexons_files_stops_a_patch_anywhere() {
        let refused = verdict(&Evidence { write_test: WriteTest::Failed(Some(5)), ..home() });
        assert_eq!(refused.patch, Outlook::Unlikely);
        assert_eq!(refused.patch_grounds, ["빈 파일 생성 실패 (오류 5)"]);

        let read_only = Drive { read_only: true, ..drive("C:", Attached::Disk, "NVMe", 200) };
        let locked = verdict(&Evidence { game: Some(read_only), ..home() });
        assert_eq!(locked.patch, Outlook::Unlikely);
        assert_eq!(locked.patch_grounds, ["C: 읽기 전용 볼륨"]);

        let files = Files::Differs { differs: 3, missing: 1 };
        let differs = verdict(&Evidence { files: files.clone(), ..home() });
        assert_eq!((differs.kind, differs.patch, differs.kept), (Kind::Ordinary, Outlook::Unlikely, Outlook::Expected));
        assert_eq!(differs.patch_grounds, ["게임 파일 1개 누락", "게임 파일 3개 변경"]);
        // On a seat without a disk too: this is known, where the write limit is not.
        let seat = verdict(&Evidence { files, ..seat_with_network_game_disk() });
        assert_eq!((seat.kind, seat.patch), (Kind::Diskless, Outlook::Unlikely));
    }

    #[test]
    fn folder_that_may_be_at_a_version_nothing_knows_leaves_the_patch_open() {
        let unsettled =
            |newer: Option<&str>| Files::Unsettled { differs: 9, missing: 0, newer: newer.map(str::to_owned) };
        // The server has a minor the table was made before.
        let newer = verdict(&Evidence { files: unsettled(Some("Ver 1.2.421 마이너버전(5)")), ..home() });
        assert_eq!((newer.kind, newer.patch, newer.kept), (Kind::Ordinary, Outlook::Unknown, Outlook::Expected));
        assert_eq!(newer.patch_grounds, ["게임 파일 9개 변경", "서버 최신 Ver 1.2.421 마이너버전(5)의 파일 목록 없음"]);
        // The server could not be asked.
        let unasked = verdict(&Evidence { files: unsettled(None), ..home() });
        assert_eq!(unasked.patch, Outlook::Unknown);
        assert_eq!(unasked.patch_grounds, ["게임 파일 9개 변경", "서버 최신 버전 미확인"]);
        // What was unlikely for another reason stays unlikely.
        let tight = Evidence { game: Some(drive("C:", Attached::Disk, "NVMe", 10)), files: unsettled(None), ..home() };
        assert_eq!(verdict(&tight).patch, Outlook::Unlikely);
    }

    #[test]
    fn write_test_that_was_left_out_leaves_the_patch_open() {
        let verdict = verdict(&Evidence { write_test: WriteTest::Skipped, ..home() });
        assert_eq!((verdict.kind, verdict.patch), (Kind::Ordinary, Outlook::Unknown));
        // Nothing is said of a folder that was not written to.
        assert_eq!(verdict.patch_grounds, ["C: 여유 200.0 GB"]);
    }

    #[test]
    fn what_the_files_are_is_said_and_only_a_mismatch_decides() {
        let said = |files: Files| verdict(&Evidence { files, ..home() });
        let sound = "모든 게임 파일 내용 일치".to_owned();
        // Every file read and found right: that is said.
        let read = said(Files::At { unread: 0 });
        assert_eq!(read.patch, Outlook::Expected);
        assert!(read.patch_grounds.contains(&sound));
        // The quick comparison goes by length for the large files, several versions may
        // fit, and nothing may have been compared at all: none of these is in the way, and
        // of none is it said that every file is right.
        for files in [Files::At { unread: 248 }, Files::Either, Files::NotCompared] {
            let verdict = said(files.clone());
            assert_eq!(verdict.patch, Outlook::Expected, "{files:?}");
            assert!(!verdict.patch_grounds.contains(&sound), "{files:?}");
        }
    }

    /// The three answers in everyday words, as a page shows them to a player: definite,
    /// none of them empty, none with a word of the report's in it.
    #[test]
    fn each_answer_is_also_said_outright_in_everyday_words_with_one_reason() {
        // The answer and the reason of each of the three rows, checked for what a player
        // must not be shown.
        let plain = |evidence: &Evidence| -> [(String, String); 3] {
            let verdict = verdict(evidence);
            let rows = verdict.plain();
            assert_eq!(rows.map(|row| row.name), ["PC 종류", "클라이언트 패치", "재부팅 후"]);
            assert_eq!(verdict.kind_plain.answer, verdict.kind.plain());
            assert!(["일반 PC", "노하드 PC방 좌석", "복구 프로그램이 있는 PC", "확인 불가"].contains(&rows[0].answer));
            assert!(["가능", "즉시 패치로 가능", "불가", "확인 불가"].contains(&rows[1].answer), "{:?}", rows[1]);
            assert!(["패치 유지", "패치 초기화", "확인 불가"].contains(&rows[2].answer), "{:?}", rows[2]);
            for text in rows.iter().flat_map(|row| [row.name, row.answer, row.reason.as_str()]) {
                let short = !text.is_empty() && text.chars().count() <= 40 && !text.ends_with('.');
                assert!(short && !text.contains('\n'), "{text:?} of {evidence:?}");
                // No Latin letter but in the units and in PC.
                let latin = text.replace("PC", "").replace("GB", "").replace("MB", "");
                assert!(!latin.chars().any(|letter| letter.is_ascii_alphabetic()), "{text:?}");
                // Nothing that hedges, and none of the report's careful words.
                for hedge in ["로 보임", "것 같음", "예상", "추정", "해 봐야", "어려움", "수 있음", "아마", "판단 불가"]
                {
                    assert!(!text.contains(hedge), "{text:?} hedges with {hedge:?}");
                }
            }
            rows.map(|row| (row.answer.to_owned(), row.reason.clone()))
        };
        let said = |answer: &str, reason: &str| (answer.to_owned(), reason.to_owned());
        let by_length = |evidence: Evidence| Evidence { files_by_length: true, ..evidence };
        let seat = pc_room_seat(Bus::FIBRE_CHANNEL);
        let line = Some(1_000_000_000);

        // The PC at home after a run that asked nothing: its files were not read, so
        // nothing is said of them.
        assert_eq!(
            plain(&by_length(home())),
            [
                said("일반 PC", "게임이 이 PC의 디스크에 설치됨"),
                said("가능", "여유 공간 충분(200 GB)"),
                said("패치 유지", "이 PC의 디스크에 저장됨"),
            ]
        );
        // With every file read they are said to be sound; with the long ones unread, not.
        assert_eq!(plain(&home())[1], said("가능", "여유 공간 충분(200 GB) · 게임 파일 정상"));
        let partly = Files::At { unread: 248 };
        assert_eq!(plain(&Evidence { files: partly, ..home() })[1], said("가능", "여유 공간 충분(200 GB)"));

        // The seat after a run that asked nothing. The careful answer is that it is not
        // told; a player is told that it goes, and the one thing that ends it.
        let unasked = by_length(seat.clone());
        assert_eq!(verdict(&unasked).patch, Outlook::Unknown);
        assert_eq!(
            plain(&unasked),
            [
                said("노하드 PC방 좌석", "게임 디스크를 매장 서버에서 네트워크로 받아 사용"),
                said("가능", "매장의 쓰기 공간이 부족하면 중단"),
                said("패치 초기화", "재부팅·자리 이동 시 다시 패치 필요"),
            ]
        );
        // With its write probe: where the writes were seen to go to the shop's server, it
        // is the server's room that ends it. Where they went into memory first, or
        // nowhere that shows, nothing is said of a server.
        let probe = probed([1799.0, 1799.0, 106.0, 105.0], [3, 47, 162, 154]);
        let carried = Evidence { landed: Landed::of(&probe, line), ..seat.clone() };
        assert_eq!(plain(&carried)[1], said("가능", "매장 서버 공간이 부족하면 중단"));
        for landed in [seats_probe(), steady_probe(20 * GIB), Landed::of(&probed([1799.0; 4], [3, 5, 4, 6]), line)] {
            let seat = Evidence { landed, ..seat.clone() };
            assert_eq!(plain(&seat)[1], said("가능", "매장의 쓰기 공간이 부족하면 중단"));
        }
        // A seat that is told by its signs alone is the same seat to a player.
        assert_eq!(plain(&pc_room_seat(Bus::SATA))[0].0, "노하드 PC방 좌석");
        // The test that writes as much as a patch: where it went through, and where it
        // ended short of that, by its own rules or on an error.
        let passed = Evidence { limit: Some(Limit::Passed { written: ROOM_NEEDED }), ..seat.clone() };
        assert_eq!(plain(&passed)[1], said("가능", "패치 크기만큼 쓰기 확인"));
        let ended = |halt: Halt| Some(Limit::Stopped { written: 16 * GIB, error: None, halted: Some(halt) });
        let in_memory = Evidence { limit: ended(Halt::Memory), ..seat.clone() };
        assert_eq!(plain(&in_memory)[1], said("불가", "쓰기 공간 부족(16 GB에서 중단)"));
        let failed = Some(Limit::Stopped { written: 21 * GIB, error: Some(112), halted: None });
        assert_eq!(plain(&Evidence { limit: failed, ..home() })[1], said("불가", "쓰기 공간 부족(21 GB에서 중단)"));
        // It got slow: nothing was settled, and the answer is the one without the test.
        let slow = Evidence { limit: ended(Halt::Slow), ..seat.clone() };
        assert_eq!(plain(&slow)[1], said("가능", "매장의 쓰기 공간이 부족하면 중단"));

        // What stops a patch anywhere.
        let missing = Files::Differs { differs: 0, missing: 932 };
        assert_eq!(plain(&by_length(Evidence { files: missing, ..home() }))[1], said("불가", "게임 파일 없음(932개)"));
        let wrong = Files::Differs { differs: 3, missing: 0 };
        assert_eq!(plain(&Evidence { files: wrong, ..home() })[1], said("불가", "게임 파일이 원본과 다름(3개)"));
        let locked = Evidence { write_test: WriteTest::Failed(Some(5)), ..home() };
        assert_eq!(plain(&locked)[1], said("불가", "설치 폴더에 쓸 수 없음"));

        // Too little room for the usual way. Whether the immediate way fits is in the
        // patch file: with the engine's figures for it, it is said; without, it is said
        // that it was not checked, with what is free and what the usual way needs.
        let with_free =
            |gigabytes: u64| Evidence { game: Some(drive("C:", Attached::Disk, "NVMe", gigabytes)), ..home() };
        assert_eq!(plain(&with_free(20))[1], said("확인 불가", "여유 공간 20 GB(일반 패치는 약 37 GB 필요)"));
        let room = Some(Room { safe: 71 * GIB / 2, immediate: 9 * GIB });
        let by_the_engine = |gigabytes: u64| Evidence { room, ..with_free(gigabytes) };
        assert_eq!(
            plain(&by_the_engine(20))[1],
            said("즉시 패치로 가능", "여유 공간 20 GB(일반 패치는 약 36 GB 필요)")
        );
        assert_eq!(plain(&by_the_engine(1))[1], said("불가", "여유 공간 부족(1 GB)"));
        assert_eq!(plain(&by_the_engine(8))[1], said("불가", "여유 공간 부족(8 GB)"));
        assert_eq!(plain(&by_the_engine(9))[1].0, "즉시 패치로 가능");
        assert_eq!(plain(&by_the_engine(160))[1], said("가능", "여유 공간 충분(160 GB) · 게임 파일 정상"));
        // A patch that needs less than the one this program reckons with: the engine's
        // figure is the one that counts.
        let small = Evidence { room: Some(Room { safe: 2 * GIB, immediate: GIB }), ..with_free(20) };
        assert_eq!(
            (verdict(&small).patch, plain(&small)[1].clone()),
            (Outlook::Unlikely, said("가능", "여유 공간 충분(20 GB) · 게임 파일 정상"))
        );

        // What could not be checked, with what kept it from being.
        let unsettled = Files::Unsettled { differs: 40, missing: 0, newer: None };
        assert_eq!(
            plain(&Evidence { files: unsettled, ..home() })[1],
            said("확인 불가", "게임 파일이 알려진 버전과 다름")
        );
        let untried = Evidence { write_test: WriteTest::Skipped, ..home() };
        assert_eq!(plain(&untried)[1], said("확인 불가", "설치 폴더 쓰기 확인 안 함"));
        // A PC that cannot be told: a drive that could be either, and one sign alone.
        let unclear = Evidence { game: Some(drive("D:", Attached::Unclear, "SAS", 500)), same_drive: false, ..home() };
        assert_eq!(
            plain(&unclear),
            [
                said("확인 불가", "디스크 종류를 알 수 없음"),
                said("확인 불가", "디스크 종류를 알 수 없음"),
                said("확인 불가", "PC 종류를 알 수 없음"),
            ]
        );
        let one_sign = Evidence { reads: reads(102.0, 105.0, 0.4), ..home() };
        assert_eq!(plain(&one_sign)[0], said("확인 불가", "PC방 좌석인지 가릴 근거 부족"));
        assert_eq!(plain(&one_sign)[1], said("확인 불가", "PC 종류를 알 수 없음"));
        let nowhere = Evidence { install_found: false, game: None, ..home() };
        assert_eq!(plain(&nowhere)[..2], [said("확인 불가", "설치 폴더 없음"), said("확인 불가", "설치 폴더 없음")]);
        // A disk with a restore program: the patch goes through, and whether it stays
        // hangs on how that program is set.
        let programs = Programs { restore: names(&["DFServ.exe"]), ..Programs::default() };
        assert_eq!(
            plain(&Evidence { programs, ..home() }),
            [
                said("복구 프로그램이 있는 PC", "재부팅 시 변경 내용을 되돌리는 프로그램 실행 중"),
                said("가능", "여유 공간 충분(200 GB) · 게임 파일 정상"),
                said("확인 불가", "복구 프로그램 설정에 따라 다름"),
            ]
        );
        // Told to be a seat only by where the probe's writes landed: nothing of a server.
        let two_disks = Evidence { game: Some(drive("D:", Attached::Disk, "SATA", 855)), same_drive: false, ..home() };
        let landed =
            Landed { keeps: Keeps::Volume { letter: "C:".to_owned(), free: Some(31 * GIB) }, ..seats_probe().unwrap() };
        let redirected = plain(&Evidence { landed: Some(landed), ..two_disks });
        assert_eq!(redirected[0], said("노하드 PC방 좌석", "게임 폴더에 쓴 내용이 다른 곳에 따로 저장됨"));
        assert_eq!(redirected[1], said("가능", "매장의 쓰기 공간이 부족하면 중단"));
    }

    /// The driver of a product that hands a seat its disks is a sign about the PC, as the
    /// iSCSI port driver is: beside a sign about a drive it makes a seat, and alone it
    /// only keeps the PC from being called an ordinary one.
    #[test]
    fn driver_of_a_network_disk_product_is_a_sign_about_the_pc() {
        let programs = Programs { netdisk: names(&["nxddsk"]), ..Programs::default() };
        let alone = verdict(&Evidence { programs: programs.clone(), ..home() });
        assert_eq!((alone.kind, alone.kept), (Kind::Unknown, Outlook::Unknown));
        assert_eq!(alone.kind_grounds, ["이 PC의 NVMe 디스크에 설치", "nxddsk"]);
        // On the second seat, whose disks' name tells already, it is one sign more.
        let seat = verdict(&Evidence {
            programs: Programs { netdisk: names(&["nxddsk"]), ..second_seat().programs },
            ..second_seat()
        });
        assert_eq!(seat.kind, Kind::Diskless);
        assert_eq!(seat.kind_grounds, ["NetZone 디스크", "GreenGuard.exe", "iScsiPrt", "nxddsk"]);
        // With reads at the speed of the line beside it, on disks that say nothing: a seat.
        let with_reads = Evidence { programs, reads: reads(108.0, 108.0, 0.2), ..home() };
        assert_eq!(verdict(&with_reads).kind, Kind::Diskless);
    }

    /// A run that asks nothing reads no file. What it tells of the files is by their
    /// lengths and the game's logged version: the verdict stands on that, and says nothing of
    /// what the files hold.
    #[test]
    fn files_that_were_not_read_are_told_by_length_and_version_and_not_said_to_be_sound() {
        let said = |files: Files| verdict(&Evidence { files, files_by_length: true, ..home() });
        let at = said(Files::At { unread: 1212 });
        assert_eq!((at.kind, at.patch, at.kept), (Kind::Ordinary, Outlook::Expected, Outlook::Expected));
        // Nothing is said of what the files hold: none was read.
        assert_eq!(at.patch_grounds, ["C: 여유 200.0 GB", "설치 폴더 쓰기 성공"]);
        assert_eq!(said(Files::Either).patch, Outlook::Expected);
        // A file of the wrong length, or one that is not there, stops a patch as surely as
        // a wrong checksum does. That it is the length that differs is said.
        let differs = said(Files::Differs { differs: 3, missing: 1 });
        assert_eq!(differs.patch, Outlook::Unlikely);
        assert_eq!(differs.patch_grounds, ["게임 파일 1개 누락", "게임 파일 3개 크기 변경"]);
        // On the seat, too, the answer needs none of the steps that are asked for.
        let seat = Evidence { files_by_length: true, ..pc_room_seat(Bus::FIBRE_CHANNEL) };
        let verdict = verdict(&seat);
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely));
    }

    #[test]
    fn writes_that_land_on_another_drive_make_a_seat_and_that_drives_room_the_limit() {
        // 512 MB went to D: and C: lost them: C: had this much free.
        let landed = |free_gib: u64| {
            Some(Landed {
                written: 512 * MIB,
                speed: 300.0 * MB_S,
                keeps: Keeps::Volume { letter: "C:".to_owned(), free: Some(free_gib * GIB) },
                went: Went::Unseen,
                memory: None,
                dip: None,
            })
        };
        let two_disks = Evidence { game: Some(drive("D:", Attached::Disk, "SATA", 855)), same_drive: false, ..home() };
        let tight = verdict(&Evidence { landed: landed(31), ..two_disks.clone() });
        assert_eq!((tight.kind, tight.patch, tight.kept), (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely));
        assert_eq!(tight.kind_grounds, ["설치 폴더에 쓴 512 MB만큼 C: 여유 감소"]);
        // What that drive has free is where the writes go first, and no limit: a seat may
        // pass them on when it is full. It is stated as the figure it is.
        assert_eq!(tight.patch_grounds, ["쓴 만큼 C: 여유 감소", "C: 여유 31.0 GB"]);
        let roomy = verdict(&Evidence { landed: landed(120), ..two_disks.clone() });
        assert_eq!((roomy.kind, roomy.patch), (Kind::Diskless, Outlook::Unknown));
        assert_eq!(roomy.patch_grounds[1], "C: 여유 120.0 GB");
        // A probe whose writes stayed where they were written changes nothing.
        let stayed = Some(Landed { keeps: Keeps::Unseen, ..landed(0).unwrap() });
        assert_eq!(verdict(&Evidence { landed: stayed, ..two_disks }).kind, Kind::Ordinary);
    }

    /// A write of `steps` files of 256 MB each, after each of which `memory` was
    /// available and the volumes had `free`: the game's G:, which is written to, then C:
    /// and D:. `after` is what memory was after the removal and some seconds later.
    fn stepped(before: u64, memory: &[u64], free: &[[u64; 3]], after: [u64; 2]) -> Landing {
        use crate::facts::{Moved, Step};
        let now = |available: u64| Some(Memory { available, cache: 0, nonpaged: 0, commit: 0 });
        let began = [305 * GIB, 154 * GIB, 32 * GIB];
        let steps: Vec<Step> = memory
            .iter()
            .zip(free)
            .map(|(available, free)| Step {
                bytes: 256 * MIB,
                seconds: 0.6,
                free: free.map(Some).to_vec(),
                memory: now(*available),
            })
            .collect();
        let ended = free.last().copied().unwrap_or(began);
        let volumes = ["G:", "C:", "D:"]
            .iter()
            .enumerate()
            .map(|(at, letter)| Moved {
                letter: (*letter).to_owned(),
                written_to: at == 0,
                before: Some(began[at]),
                after_write: Some(ended[at]),
                after_delete: Some(began[at]),
                later: Some(began[at]),
            })
            .collect();
        Landing {
            place: Place::Install,
            written: 256 * MIB * steps.len() as u64,
            wanted: 256 * MIB * steps.len() as u64,
            seconds: 0.6 * steps.len() as f64,
            error: None,
            halted: None,
            volumes,
            memory: [now(before), memory.last().copied().and_then(now), now(after[0]), now(after[1])],
            steps,
            waited: 3.0,
            deleted: true,
        }
    }

    /// What G: has free after each of four steps of 256 MB, with C: and D: as they were.
    fn only_the_games_drive() -> [[u64; 3]; 4] {
        [1, 2, 3, 4].map(|step| [305 * GIB - step * 256 * MIB, 154 * GIB, 32 * GIB])
    }

    #[test]
    fn where_a_pc_keeps_its_writes_is_read_from_what_moved_with_them() {
        let before = 20 * GIB + 102 * MIB;
        let down = |megabytes: [u64; 4]| megabytes.map(|gone| before - gone * MIB);
        let on_g = only_the_games_drive();

        // Memory down by about what each step wrote, after every step, and still down
        // after the removal: the writes are kept in memory.
        let in_memory = stepped(before, &down([270, 540, 800, 1070]), &on_g, [before - 1060 * MIB; 2]);
        assert_eq!(keeps(&in_memory), Keeps::Memory { steady: true });
        // The same with the memory back after the removal: it was where they were kept.
        let gave_back = stepped(before, &down([270, 540, 800, 1070]), &on_g, [before - 30 * MIB; 2]);
        assert_eq!(keeps(&gave_back), Keeps::Memory { steady: true });
        // What keeps the writes takes some memory of its own: 1.6 for 1, as on the seat.
        let with_overhead = stepped(before, &down([400, 810, 1210, 1620]), &on_g, [before - 1600 * MIB; 2]);
        assert_eq!(keeps(&with_overhead), Keeps::Memory { steady: true });
        // One step in four that says otherwise (something else gave memory back just
        // then) does not undo the other three.
        let one_off = stepped(before, &down([270, 540, 560, 830]), &on_g, [before - 820 * MIB; 2]);
        assert_eq!(keeps(&one_off), Keeps::Memory { steady: true });

        // Memory that moves about and does not go down with the steps: noise.
        let noisy = stepped(before, &down([300, 120, 420, 260]), &on_g, [before - 200 * MIB; 2]);
        assert_eq!(keeps(&noisy), Keeps::Unseen);
        // One jump of a gigabyte in the middle, for another reason: about as much as was
        // written in all, and in no step with it.
        let jump = stepped(before, &down([10, 1040, 1050, 1060]), &on_g, [before - 1050 * MIB; 2]);
        assert_eq!(keeps(&jump), Keeps::Unseen);
        // Memory that did not move at all: a PC whose writes go to its disk, or a seat
        // whose writes go where it does not show.
        let still = stepped(before, &down([5, 12, 8, 15]), &on_g, [before; 2]);
        assert_eq!(keeps(&still), Keeps::Unseen);

        // D: down by what each step wrote: the writes are kept there, and what D: had
        // free is what the PC takes. Memory that moved besides does not come into it.
        let on_d = [1, 2, 3, 4].map(|step| [305 * GIB - step * 256 * MIB, 154 * GIB, 32 * GIB - step * 256 * MIB]);
        let on_a_volume = stepped(before, &down([270, 540, 800, 1070]), &on_d, [before; 2]);
        assert_eq!(keeps(&on_a_volume), Keeps::Volume { letter: "D:".to_owned(), free: Some(32 * GIB) });
        // D: lost as much in one go while the last step was written (something else wrote
        // to it): it did not go down with the steps, and is not where the writes are kept.
        let at_once = [[304 * GIB, 154 * GIB, 32 * GIB]; 3]
            .into_iter()
            .chain([[304 * GIB, 154 * GIB, 31 * GIB]])
            .collect::<Vec<_>>();
        assert_eq!(keeps(&stepped(before, &down([5, 12, 8, 15]), &at_once, [before; 2])), Keeps::Unseen);
    }

    /// The numbers of the seat this was first run on, whose probe was one file of 512 MB:
    /// memory fell 808 MB and stayed down, C: and D: did not move, G: came back.
    #[test]
    fn the_seats_one_reading_of_memory_is_an_estimate_that_is_not_steady() {
        use crate::facts::Moved;
        let now = |available: u64| Some(Memory { available, cache: 0, nonpaged: 0, commit: 0 });
        let before = 20 * GIB + 102 * MIB;
        let moved = |letter: &str, written_to: bool, free: u64, after_write: u64| Moved {
            letter: letter.to_owned(),
            written_to,
            before: Some(free),
            after_write: Some(after_write),
            after_delete: Some(free),
            later: None,
        };
        let seat = Landing {
            place: Place::Install,
            written: 512 * MIB,
            wanted: 512 * MIB,
            seconds: 1.24,
            error: None,
            halted: None,
            steps: Vec::new(),
            volumes: vec![
                moved("C:", false, 154 * GIB, 154 * GIB),
                moved("D:", false, 32 * GIB, 32 * GIB),
                moved("G:", true, 305 * GIB, 305 * GIB - 530 * MIB),
            ],
            memory: [now(before), now(before - 808 * MIB), now(before - 831 * MIB), None],
            waited: 0.0,
            deleted: true,
        };
        assert_eq!(keeps(&seat), Keeps::Memory { steady: false });
        let landed = Landed::of(&seat, None).unwrap();
        assert_eq!((landed.written, &landed.keeps), (512 * MIB, &Keeps::Memory { steady: false }));
        assert_eq!((landed.memory, landed.dip), (Some((before, 808 * MIB)), Some(808 * MIB)));
        // Written in 1.24 seconds: 413 MB/s, four times as fast as the 102 MB/s the drive
        // reads at. No faster than a drive that reads at 400 MB/s, and nothing to hold it
        // against where reads were not timed.
        assert!(landed.times_faster_than(Some(102.0 * MB_S)).is_some_and(|times| (4.0..4.1).contains(&times)));
        assert_eq!((landed.times_faster_than(Some(400.0 * MB_S)), landed.times_faster_than(None)), (None, None));

        // Less than half of what was written, or several times as much, is not "about one
        // for one": memory moves by that much anyway.
        let lost = |megabytes: u64| Landing {
            memory: [now(before), now(before - megabytes * MIB), now(before - megabytes * MIB), None],
            ..seat.clone()
        };
        assert_eq!((keeps(&lost(200)), keeps(&lost(1400))), (Keeps::Unseen, Keeps::Unseen));
        assert_eq!(
            (keeps(&lost(256)), keeps(&lost(1280))),
            (Keeps::Memory { steady: false }, Keeps::Memory { steady: false })
        );
        // Memory that grew counts as none lost, and without a reading before and after
        // there is nothing to say.
        let grew = Landing { memory: [now(before), now(before + 50 * MIB), now(before), None], ..seat.clone() };
        assert_eq!(keeps(&grew), Keeps::Unseen);
        let unread = Landing { memory: [None, now(before), now(before), None], ..seat.clone() };
        assert_eq!((keeps(&unread), Landed::of(&unread, None).unwrap().memory), (Keeps::Unseen, None));
        // A volume that was not written to and lost what was written took the writes; one
        // that gained room, or whose room could not be read, took nothing.
        let mut elsewhere = seat.clone();
        elsewhere.volumes[1].after_write = Some(32 * GIB - 512 * MIB);
        assert_eq!(keeps(&elsewhere), Keeps::Volume { letter: "D:".to_owned(), free: Some(32 * GIB) });
        let mut odd = lost(0);
        odd.volumes[0].after_write = Some(155 * GIB);
        odd.volumes[1].before = None;
        assert_eq!(keeps(&odd), Keeps::Unseen);
        // A probe that wrote next to nothing says nothing.
        assert_eq!(Landed::of(&Landing { written: 10 * MIB, ..seat }, None), None);
    }

    #[test]
    fn drives_are_sorted_by_how_they_are_attached() {
        let fixed = |bus: u32, model: &str| attached(DriveKind::Fixed, Some(Bus(bus)), model, true);
        assert_eq!(fixed(17, "SAMSUNG MZVL21T0HCLR-00BL2"), Attached::Disk);
        assert_eq!(fixed(11, "WDC WD10EZEX-08WN4A0"), Attached::Disk);
        assert_eq!(fixed(3, "ST3500418AS"), Attached::Disk);
        assert_eq!(fixed(8, "NVMe Samsung SSD 980"), Attached::Disk);
        // A stick or a card is one without being asked about seeking.
        assert_eq!(attached(DriveKind::Removable, Some(Bus::USB), "Samsung PSSD T7", false), Attached::Disk);
        assert_eq!(attached(DriveKind::Removable, Some(Bus(12)), "SD Card", false), Attached::Disk);
        // A storage network, by either of its two buses, whatever else is said.
        assert_eq!(fixed(9, "MSFT Virtual HD"), Attached::Network);
        assert_eq!(fixed(6, "NSM Logical_G 3722GB"), Attached::Network);
        assert_eq!(attached(DriveKind::Fixed, Some(Bus::FIBRE_CHANNEL), "Some Disk", false), Attached::Network);
        assert_eq!(fixed(14, ""), Attached::Virtual);
        assert_eq!(fixed(15, "Msft Virtual Disk"), Attached::Virtual);
        // A bus that says nothing of whose disk it is: SCSI, SAS, none, one not known.
        for bus in [1, 10, 0, 4, 5, 16, 18, 19, 77] {
            assert_eq!(fixed(bus, "Some Disk"), Attached::Unclear, "bus {bus}");
        }
        assert_eq!(attached(DriveKind::Fixed, None, "Some Disk", true), Attached::Unclear);
        // A bus of a PC's own disks, and a disk that does not answer what such a disk answers.
        for bus in [3, 8, 11, 17] {
            assert_eq!(attached(DriveKind::Fixed, Some(Bus(bus)), "Some Disk", false), Attached::Unclear, "bus {bus}");
        }
        // A bus of a PC's own disks under a name that gives the disk away.
        assert_eq!(fixed(11, "GManager VDisk"), Attached::Unclear);
        assert_eq!(fixed(17, "NSM 22H2-H610 232GB"), Attached::Unclear);
        assert_eq!(fixed(10, "Msft Virtual Disk"), Attached::Unclear);
        // What is no disk of this PC at all.
        assert_eq!(attached(DriveKind::Remote, None, "", false), Attached::Share);
        assert_eq!(attached(DriveKind::RamDisk, Some(Bus::SATA), "", true), Attached::Unclear);
    }

    #[test]
    fn name_of_a_disk_gives_a_made_up_one_away_and_leaves_real_ones_alone() {
        // The two disks of the seat, by their maker.
        assert_eq!(mark_in("NSM 22H2-H610 232GB").as_deref(), Some("NSM"));
        assert_eq!(mark_in("nsm Logical_G 3722GB").as_deref(), Some("NSM"));
        // An image named after a release of Windows, and the words made-up disks carry.
        assert_eq!(mark_in("Win10 22H2-H610").as_deref(), Some("22H2"));
        assert_eq!(mark_in("IMG_24H2_B760").as_deref(), Some("24H2"));
        assert_eq!(mark_in("HP LOGICAL VOLUME").as_deref(), Some("logical"));
        assert_eq!(mark_in("Msft Virtual Disk").as_deref(), Some("virtual"));
        assert_eq!(mark_in("GManager VDisk").as_deref(), Some("vdisk"));
        // Disks as PCs have them, a maker that only begins the same, and letters and
        // digits that only look like a release inside a longer word.
        for real in [
            "SAMSUNG MZVL21T0HCLR-00BL2",
            "Micron MTFDKBA512TFH",
            "WDC WD10EZEX-08WN4A0",
            "Samsung SSD 970 EVO Plus 1TB",
            "KINGSTON SA400S37240G",
            "SK hynix BC711 HFM256GD3JX013N",
            "NVMe INTEL SSDPEKNW512G8",
            "ST2000DM008-2FR102",
            "NSMX 500GB",
            "X22H2Y 500GB",
            "TOSHIBA DT01ACA100 22H3",
            "",
        ] {
            assert_eq!(mark_in(real), None, "{real}");
        }
    }

    #[test]
    fn what_a_volume_says_of_its_disk_is_taken_down_with_its_letter_fit_to_show() {
        let scrub = Scrub::new(["MINSU-PC".to_owned()], Vec::new());
        let seat = Drive::of(&served("G:", Bus::FIBRE_CHANNEL, "NSM Logical_G of MINSU-PC", 305 * GIB), &scrub);
        assert_eq!((seat.attached, seat.how.as_str()), (Attached::Network, "Fibre Channel"));
        assert_eq!(seat.marked.as_deref(), Some("NSM"));
        assert!(seat.seek_unanswered);
        // A disk of a PC: no mark, and it answered.
        let seek: Fact<bool> = Ok(false);
        let own = Volume { seek_penalty: seek, ..served("C:", Bus::NVME, "SAMSUNG MZVL21T0HCLR-00BL2", 200 * GIB) };
        let own = Drive::of(&own, &scrub);
        assert_eq!(
            (own.attached, own.how.as_str(), &own.marked, own.seek_unanswered),
            (Attached::Disk, "NVMe", &None, false)
        );
        // A shared folder has no disk to ask.
        let shared = Volume {
            kind: DriveKind::Remote,
            bus: Err(Unknown::Skipped),
            seek_penalty: Err(Unknown::Skipped),
            ..served("", Bus(0), "", GIB)
        };
        let shared = Drive::of(&shared, &scrub);
        assert_eq!(
            (shared.attached, shared.how.as_str(), shared.seek_unanswered),
            (Attached::Share, "네트워크", false)
        );
        // The grounds of a verdict name a volume by its letter, which is a letter and
        // nothing else for a disk. What stands in its place for a folder that another
        // computer shares names that computer, and is not passed on as it is.
        assert_eq!((seat.letter.as_str(), shared.letter.as_str()), ("G:", ""));
        let named = Volume { letter: r"\\MINSU-PC\games".to_owned(), ..served("", Bus(0), "", GIB) };
        assert_eq!(Drive::of(&named, &scrub).letter, r"\\***\games");
    }

    #[test]
    fn same_evidence_gives_the_same_verdict() {
        assert_eq!(verdict(&home()), verdict(&home()));
        assert_eq!(verdict(&pc_room_seat(Bus::FIBRE_CHANNEL)), verdict(&pc_room_seat(Bus::FIBRE_CHANNEL)));
    }
}
