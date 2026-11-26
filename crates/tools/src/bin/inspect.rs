//! Reads a real patch file and compares it with an install folder, changing nothing.
//!
//!     cargo run --release -p mfp-tools --bin inspect -- <url-or-file> <install-dir> [cache-dir]
//!
//! Prints what the patch would do and, for every file it writes, whether the install folder
//! already holds the new version, still holds the original, or holds something else.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use mfp_engine::container::PatchFile;
use mfp_engine::download;
use mfp_engine::plan::{self, Action};
use mfp_engine::stage::{self, Fingerprint};

fn main() -> std::process::ExitCode {
    mfp_tools::end(run())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let source = args.next().ok_or("missing patch url or file")?;
    let install = PathBuf::from(args.next().ok_or("missing install dir")?);
    let cache = args.next().map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let go = AtomicBool::new(false);

    let patch_path = if source.starts_with("http") {
        let dest = cache.join(source.rsplit('/').next().unwrap());
        let started = Instant::now();
        let mut last = 0;
        let remote = download::fetch(&source, &dest, &go, &mut |done, total| {
            if done - last >= 100 << 20 || done == total {
                last = done;
                println!("  downloaded {} / {} MB", done >> 20, total >> 20);
            }
        })?;
        println!("download: {} bytes, tag {:?}, {:.1}s", remote.len, remote.etag, started.elapsed().as_secs_f32());
        dest
    } else {
        PathBuf::from(source)
    };

    let started = Instant::now();
    let patch = PatchFile::open(&patch_path)?;
    patch.verify(&go, |_| {})?;
    println!("payload checksum ok: {} bytes in {:.1}s", patch.payload_len(), started.elapsed().as_secs_f32());

    let started = Instant::now();
    let plan = plan::scan(&patch, &go)?;
    println!(
        "scan: {:?} layout, {} originals listed, {} entries, {} MB of contents, {:.1}s",
        plan.layout,
        plan.originals.len(),
        plan.entries.len(),
        plan.contents_len >> 20,
        started.elapsed().as_secs_f32()
    );

    let (mut dirs, mut creates, mut rebuilds, mut deletes) = (0, 0, 0, 0);
    for entry in &plan.entries {
        match entry.action {
            Action::CreateDir => dirs += 1,
            Action::Create { .. } => creates += 1,
            Action::Rebuild { .. } => rebuilds += 1,
            Action::Delete => deletes += 1,
        }
    }
    println!(
        "  {creates} create, {rebuilds} rebuild, {deletes} delete, {dirs} folder; {} MB of new files",
        plan.new_bytes() >> 20
    );

    let started = Instant::now();
    let (mut is_new, mut is_old, mut other, mut missing) = (0, 0, 0, 0);
    let mut examples = Vec::new();
    for entry in &plan.entries {
        let Some(new) = Fingerprint::promised(entry) else { continue };
        let old_crc = match &entry.action {
            Action::Rebuild { old_crc, .. } => *old_crc,
            _ => None,
        };
        let path = install.join(plan::relative_path(&entry.name)?);
        if !Path::new(&path).is_file() {
            missing += 1;
            continue;
        }
        let found = stage::fingerprint(&path, &go, &|_| {})?;
        if found == new {
            is_new += 1;
        } else if Some(found.crc) == old_crc {
            is_old += 1;
        } else {
            other += 1;
            if examples.len() < 8 {
                examples.push(format!("{} ({} bytes, patch says {})", entry.name, found.len, new.len));
            }
        }
    }
    println!(
        "install: {is_new} already the new version, {is_old} still the original, {other} something else, {missing} missing ({:.1}s)",
        started.elapsed().as_secs_f32()
    );
    for example in examples {
        println!("  differs: {example}");
    }
    Ok(())
}
