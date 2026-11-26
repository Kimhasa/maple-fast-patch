//! Holds the files two patches on the server start from against each other.
//!
//!     cargo run --release -p mfp-tools --bin originals -- <scratch-folder> <url-a> <url-b>
//!
//! A patch file names, at its very start, the files it was made from and a checksum of each.
//! Two patches that start from the same version name the same files with the same checksums:
//! that is what tells whether a patch that skips versions starts where the single step from
//! its version does. Only the start of each file is fetched, into the scratch folder.

use std::collections::BTreeMap;
use std::path::PathBuf;

use mfp_engine::detect;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(scratch), Some(a), Some(b)) = (args.next(), args.next(), args.next()) else {
        return Err("usage: originals <scratch-folder> <url-a> <url-b>".into());
    };
    let scratch = PathBuf::from(scratch);
    let table = |url: &str| -> Result<BTreeMap<String, u32>, Box<dyn std::error::Error>> {
        Ok(detect::remote_originals(url, &scratch)?.unwrap_or_default())
    };
    let (first, second) = (table(&a)?, table(&b)?);
    let name = |url: &str| url.rsplit('/').next().unwrap_or(url).to_owned();
    println!("{}: {} files", name(&a), first.len());
    println!("{}: {} files", name(&b), second.len());
    let only_first: Vec<_> = first.keys().filter(|file| !second.contains_key(*file)).collect();
    let only_second: Vec<_> = second.keys().filter(|file| !first.contains_key(*file)).collect();
    let differ: Vec<_> =
        first.iter().filter(|(file, crc)| second.get(*file).is_some_and(|other| other != *crc)).collect();
    let same = first.iter().filter(|(file, crc)| second.get(*file) == Some(*crc)).count();
    println!("named by both with the same checksum: {same}");
    println!("named by both with another checksum: {}", differ.len());
    for (file, _) in differ.iter().take(12) {
        println!("  {file}");
    }
    println!("only in the first: {}", only_first.len());
    for file in only_first.iter().take(12) {
        println!("  {file}");
    }
    println!("only in the second: {}", only_second.len());
    for file in only_second.iter().take(12) {
        println!("  {file}");
    }
    Ok(())
}
