//! Writes the files that patches bring whole into a folder: part of an install, made of
//! real files, without an install to take them from.
//!
//!     cargo run --release -p mfp-tools --bin whole_files -- <out-dir> [--most-mb <megabytes>] <patch-file>...
//!
//! A patch brings some files whole (new ones, and small ones it replaces) and rebuilds the
//! others out of the files of an install. The whole ones are in the patch file itself. Given
//! a main patch and the minor patches after it, in order, this leaves a folder that holds
//! those files as they are at the last of the patches, and nothing of the files that are
//! rebuilt. It is what the diagnostic program (`mfp-check`) is tried on where the game itself
//! must not be touched: its files are the real ones, only most of them are missing.
//!
//! `--most-mb` keeps the folder below that size: a file that would take it over is left out.
//! Reads only the patch files, and refuses to write under a folder called `Nexon`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use mfp_engine::crc;
use mfp_engine::plan::{self, Header};
use mfp_tools::clients;

fn main() -> std::process::ExitCode {
    mfp_tools::end(run())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().ok_or("missing the folder to write to")?);
    if clients::under_nexon(&out) {
        return Err("the folder to write to must not be under a folder called Nexon".into());
    }
    let (mut most, mut patches) = (u64::MAX, Vec::new());
    while let Some(arg) = args.next() {
        if arg == "--most-mb" {
            let megabytes: u64 = args.next().and_then(|text| text.parse().ok()).ok_or("--most-mb needs megabytes")?;
            most = megabytes << 20;
        } else {
            patches.push(PathBuf::from(arg));
        }
    }
    if patches.is_empty() {
        return Err("missing patch files".into());
    }

    // What the folder holds by now, by the form names are compared in, with each length.
    let mut written: BTreeMap<String, u64> = BTreeMap::new();
    let mut left_out = 0;
    let mut buffer = vec![0u8; 1 << 20];
    for path in &patches {
        let (patch, plan) = mfp_tools::read_patch(path)?;
        let (mut stream, layout, _) = plan::open_contents(&patch)?;
        for entry in &plan.entries {
            let (name, header) = plan::next_entry(&mut stream, layout, &entry.name)?;
            match header {
                Header::CreateDir => {}
                Header::Rebuild { .. } => plan::read_ops(&mut stream, layout, &name, |op, stream| match op {
                    plan::Op::Literal { len } => stream.skip(len),
                    plan::Op::Fill { .. } | plan::Op::Copy { .. } => Ok(()),
                })?,
                Header::Delete => {
                    let key = plan::name_key(&name);
                    let below = format!("{key}\\");
                    let gone: Vec<String> =
                        written.keys().filter(|file| **file == key || file.starts_with(&below)).cloned().collect();
                    for file in gone {
                        written.remove(&file);
                        let _ = fs::remove_file(out.join(plan::relative_path(&file)?));
                    }
                }
                Header::Create { len, crc: promised } => {
                    let key = plan::name_key(&name);
                    let used: u64 = written.iter().filter(|(file, _)| **file != key).map(|(_, len)| len).sum();
                    if used.saturating_add(len) > most {
                        stream.skip(len)?;
                        left_out += 1;
                        continue;
                    }
                    let dest = out.join(plan::relative_path(&name)?);
                    fs::create_dir_all(dest.parent().ok_or("a file without a folder")?)?;
                    let mut file = fs::File::create(&dest)?;
                    let (mut todo, mut checksum) = (len, 0u32);
                    while todo > 0 {
                        let piece = &mut buffer[..todo.min(1 << 20) as usize];
                        stream.read_exact(piece)?;
                        checksum = crc::update(checksum, piece);
                        file.write_all(piece)?;
                        todo -= piece.len() as u64;
                    }
                    if checksum != promised {
                        return Err(format!("{name} is not what the patch file says it is").into());
                    }
                    written.insert(key, len);
                }
            }
        }
    }
    let bytes: u64 = written.values().sum();
    println!("folder: {}", mfp_tools::shown(&out.display().to_string()));
    println!("  {} files, {} MB; {left_out} left out to stay below the size asked for", written.len(), bytes >> 20);
    Ok(())
}
