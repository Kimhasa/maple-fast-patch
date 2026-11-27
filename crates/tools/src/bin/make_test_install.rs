//! Makes a small test folder that looks like an install one patch behind, for trying the app
//! without touching the real game.
//!
//!     cargo run --release -p mfp-tools --bin make_test_install -- <earlier-patch-file> <install-dir> <test-dir>
//!         [--server live|test]
//!
//! `<earlier-patch-file>` is the patch that comes right before the one to be tested. Its
//! whole-file entries are built from the patch itself, so the test folder gets those files as
//! they were after that earlier patch. Files it rebuilds are copied from the real install,
//! which must still hold them unchanged. The result holds everything the earlier patch wrote,
//! and nothing else: enough for a patch that replaces whole files, not for one that rebuilds.
//!
//! Only reads the real install. `<test-dir>` is deleted first if it exists. `--server` says
//! which client the install is of; without it the live one.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use mfp_engine::apply;
use mfp_engine::plan::{self, Action};
use mfp_engine::preflight::DATA_DIR_NAME;
use mfp_engine::stage::{self, Checked, Dirs, Fingerprint};
use mfp_tools::clients;

fn main() -> std::process::ExitCode {
    mfp_tools::end(run())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let game_program = clients::take_server(&mut args)?.client().game_program;
    let mut args = args.into_iter();
    let patch_path = PathBuf::from(args.next().ok_or("missing patch file")?);
    let install = PathBuf::from(args.next().ok_or("missing install dir")?);
    let test = PathBuf::from(args.next().ok_or("missing test dir")?);
    if test.starts_with(&install) || install.starts_with(&test) {
        return Err("the test folder must be separate from the install folder".into());
    }
    let go = AtomicBool::new(false);

    let (patch, plan) = mfp_tools::read_patch(&patch_path)?;

    if test.exists() {
        fs::remove_dir_all(&test)?;
    }
    fs::create_dir_all(test.join(DATA_DIR_NAME))?;

    // Rebuilt files come from the real install, where they must be this patch's result.
    let mut checked = Checked::default();
    let (mut copied, mut copied_bytes) = (0, 0u64);
    for entry in &plan.entries {
        if !matches!(entry.action, Action::Rebuild { .. }) {
            continue;
        }
        let Some(wanted) = Fingerprint::promised(entry) else { continue };
        checked.up_to_date.insert(entry.name.clone());
        let relative = plan::relative_path(&entry.name)?;
        let source = install.join(&relative);
        if !source.is_file() || stage::fingerprint(&source, &go, &|_| {})? != wanted {
            return Err(format!("{} in the install is not what this patch produces", entry.name).into());
        }
        let dest = test.join(&relative);
        fs::create_dir_all(dest.parent().unwrap())?;
        fs::copy(&source, &dest)?;
        copied += 1;
        copied_bytes += wanted.len;
    }

    // Whole files come out of the patch file itself.
    let dirs = Dirs::new(&test);
    stage::stage(&patch, &plan, &checked, &dirs, &go, &mut |_, _, _| {})?;
    let built = apply::apply(&plan, &checked, &dirs, &mut |_| {})?;

    if !test.join(game_program).is_file() {
        return Err(
            format!("this patch does not include {game_program}, so the folder would not pass as an install").into()
        );
    }
    println!("test folder: {}", mfp_tools::shown(&test.display().to_string()));
    println!(
        "  {built} files built from the patch file, {copied} files ({} MB) copied from the install",
        copied_bytes >> 20
    );
    Ok(())
}
