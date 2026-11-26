//! Says how much of the files a patch rebuilds stays where it was.
//!
//!     cargo run --release -p mfp-tools --bin inplace_size -- <patch-file>...
//!
//! A patch rebuilds a file from pieces of old files, bytes it brings itself and fills. Built
//! the usual way, the whole new file is written anew. But a piece copied from the same file
//! at the same place does not need writing at all if the file is changed where it stands.
//! This counts those pieces, to tell whether changing files in place would spare a drive
//! that can take few writes (a PC room's seat without a disk of its own keeps what is
//! written in a small space, and loses it at the next start). Reads only the patch files.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use mfp_engine::container::PatchFile;
use mfp_engine::plan::{self, Header, Op};

/// Drives hand out room in pieces of this size; a piece that is touched at all is written
/// whole.
const BLOCK: u64 = 4096;

#[derive(Default)]
struct Tally {
    rebuilt_files: u64,
    rebuilt_bytes: u64,
    whole_bytes: u64,
    /// Copied from the same file at the same place.
    stays: u64,
    /// Copied from the same file, from another place in it.
    moves: u64,
    /// Copied from another file.
    comes_from_elsewhere: u64,
    brought: u64,
    /// What has to be written, counted in whole blocks.
    written_blocks: u64,
    /// Files of which nothing stays in place, and files of which everything does but for
    /// less than a hundredth.
    files_all_moved: u64,
    files_nearly_still: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let go = AtomicBool::new(false);
    for path in std::env::args().skip(1).map(PathBuf::from) {
        let patch = PatchFile::open(&path)?;
        patch.verify(&go, |_| {})?;
        let plan = plan::scan(&patch, &go)?;
        let mut tally = Tally::default();
        let (mut stream, layout, _) = plan::open_contents(&patch)?;
        for entry in &plan.entries {
            let (name, header) = plan::next_entry(&mut stream, layout, &entry.name)?;
            match header {
                Header::CreateDir | Header::Delete => {}
                Header::Create { len, .. } => {
                    stream.skip(len)?;
                    tally.whole_bytes += len;
                }
                Header::Rebuild { .. } => {
                    let own = plan::name_key(&name);
                    let (mut at, mut stays, mut dirty) = (0u64, 0u64, Vec::<(u64, u64)>::new());
                    plan::read_ops(&mut stream, layout, &name, |op, stream| {
                        let len = match op {
                            Op::Literal { len } => {
                                stream.skip(len)?;
                                tally.brought += len;
                                dirty.push((at, at + len));
                                len
                            }
                            Op::Fill { len, .. } => {
                                tally.brought += len;
                                dirty.push((at, at + len));
                                len
                            }
                            Op::Copy { from, start, len } => {
                                if plan::name_key(&from) != own {
                                    tally.comes_from_elsewhere += len;
                                    dirty.push((at, at + len));
                                } else if start == at {
                                    stays += len;
                                } else {
                                    tally.moves += len;
                                    dirty.push((at, at + len));
                                }
                                len
                            }
                        };
                        at += len;
                        Ok(())
                    })?;
                    tally.rebuilt_files += 1;
                    tally.rebuilt_bytes += at;
                    tally.stays += stays;
                    tally.files_all_moved += u64::from(stays == 0);
                    tally.files_nearly_still += u64::from(at > 0 && (at - stays) * 100 < at);
                    // Whole blocks touched by what does not stay; neighbours share a block.
                    let mut last_block_end = 0u64;
                    for (from, to) in dirty.into_iter().filter(|(from, to)| to > from) {
                        let first = (from / BLOCK).max(last_block_end);
                        let end = to.div_ceil(BLOCK);
                        if end > first {
                            tally.written_blocks += end - first;
                            last_block_end = end;
                        }
                    }
                }
            }
        }
        let gib = |bytes: u64| bytes as f64 / (1u64 << 30) as f64;
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        println!("{name}");
        println!(
            "  files rebuilt: {} ({:.2} GiB), whole new files: {:.2} GiB",
            tally.rebuilt_files,
            gib(tally.rebuilt_bytes),
            gib(tally.whole_bytes)
        );
        println!(
            "  of the rebuilt files: {:.2} GiB stays where it was, {:.2} GiB moves within its file, {:.2} GiB comes from another file, {:.2} GiB is brought by the patch",
            gib(tally.stays),
            gib(tally.moves),
            gib(tally.comes_from_elsewhere),
            gib(tally.brought)
        );
        println!(
            "  files of which nothing stays in place: {}; files that change by less than a hundredth: {}",
            tally.files_all_moved, tally.files_nearly_still
        );
        let in_place = tally.written_blocks * BLOCK + tally.whole_bytes;
        println!(
            "  to write, building every file anew: {:.2} GiB; changing files where they stand (whole 4 KiB blocks): {:.2} GiB",
            gib(tally.rebuilt_bytes + tally.whole_bytes),
            gib(in_place)
        );
    }
    Ok(())
}
