//! Tests the tool that puts a pre-patch folder back together from a patched one: patch a
//! made-up install, rebuild the old folder from the result, patch that, and compare.

/// The made-up installs and patch files of the engine's own tests.
#[path = "../../engine/tests/support/mod.rs"]
mod support;

use std::fs;
use std::path::Path;

use mfp_engine::container::PatchFile;
use mfp_engine::plan::{self, Action, Layout};
use mfp_tools::reconstruct;
use support::{GO, LIVE, World, run};

fn patch_folder(dir: &Path, patch: &Path) -> Result<usize, mfp_engine::Error> {
    run(&support::options(dir, patch), &GO, &mut |_| {}).map(|outcome| outcome.files_changed)
}

#[test]
fn folder_rebuilt_from_a_patched_install_takes_the_patch_and_gives_the_same_files() {
    for seed in 1..=30 {
        let world = World::random(seed, Layout::Table);
        let what = format!("seed {seed}");
        patch_folder(&world.install, &world.patch).unwrap_or_else(|e| panic!("{what}: {e}"));
        world.assert_patched(&what);

        let patch = PatchFile::open(&world.patch).unwrap();
        let plan = plan::scan(&patch, &GO).unwrap();
        let old = world.install.parent().unwrap().join("Rebuilt");
        let report = reconstruct::reconstruct(&patch, &plan, &world.install, &old, false, &GO, &mut |_| {})
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        reconstruct::pass_as_install(&LIVE, &world.install, &old).unwrap();

        // Two instructions that read the same bytes of an original must have agreed, and
        // every rebuilt file was on disk, so nothing needed a stand-in.
        assert!(report.conflicts.is_empty(), "{what}: {:?}", report.conflicts);
        assert!(report.survey.stand_ins.is_empty(), "{what}: {:?}", report.survey.stand_ins);

        // The rebuilt folder is not up to date: the patch has real work to do on it.
        let changed = patch_folder(&old, &world.patch).unwrap_or_else(|e| panic!("{what}, on the rebuilt folder: {e}"));
        assert!(changed > 0, "{what}: the patch found nothing to do");
        for entry in &plan.entries {
            let relative = plan::relative_path(&entry.name).unwrap();
            match entry.action {
                Action::Create { .. } | Action::Rebuild { .. } => {
                    let expected = world.after.get(&entry.name).map(Vec::as_slice).unwrap_or_default();
                    let found = fs::read(old.join(&relative)).unwrap_or_default();
                    assert!(found == expected, "{what}: {} differs after patching the rebuilt folder", entry.name);
                }
                Action::Delete => assert!(!old.join(&relative).exists(), "{what}: {} was not deleted", entry.name),
                Action::CreateDir => assert!(old.join(&relative).is_dir(), "{what}: {} was not created", entry.name),
            }
        }
        assert!(!old.join(".maple-fast-patch").exists(), "{what}: work folder left behind");
    }
}

#[test]
fn file_changed_after_the_patch_gets_a_stand_in_and_the_rest_still_matches() {
    let mut solved = 0;
    for seed in 1..=30 {
        let world = World::random(seed, Layout::Table);
        let what = format!("seed {seed}");
        patch_folder(&world.install, &world.patch).unwrap();
        let patch = PatchFile::open(&world.patch).unwrap();
        let plan = plan::scan(&patch, &GO).unwrap();

        // A later patch changed one of the rebuilt files: what it was made from is lost.
        let Some(changed) = plan.entries.iter().find(|entry| matches!(entry.action, Action::Rebuild { .. })) else {
            continue;
        };
        let changed_path = world.install.join(plan::relative_path(&changed.name).unwrap());
        let mut later = fs::read(&changed_path).unwrap();
        later.extend(b"changed again by a later patch");
        fs::write(&changed_path, later).unwrap();

        let old = world.install.parent().unwrap().join("Rebuilt");
        let report = reconstruct::reconstruct(&patch, &plan, &world.install, &old, false, &GO, &mut |_| {}).unwrap();
        reconstruct::pass_as_install(&LIVE, &world.install, &old).unwrap();
        assert!(report.conflicts.is_empty(), "{what}: {:?}", report.conflicts);
        assert_eq!(report.survey.stand_ins, std::slice::from_ref(&changed.name), "{what}");
        // Without four bytes that only this file reads there is nothing to adjust; such a
        // patch cannot be run to the end, and the report says so.
        if !report.unsolved.is_empty() {
            continue;
        }
        solved += 1;

        patch_folder(&old, &world.patch).unwrap_or_else(|e| panic!("{what}, on the rebuilt folder: {e}"));
        for entry in &plan.entries {
            if entry.name == changed.name || !matches!(entry.action, Action::Create { .. } | Action::Rebuild { .. }) {
                continue;
            }
            let expected = world.after.get(&entry.name).map(Vec::as_slice).unwrap_or_default();
            let found = fs::read(old.join(plan::relative_path(&entry.name).unwrap())).unwrap_or_default();
            assert!(found == expected, "{what}: {} differs", entry.name);
        }
        // The stand-in passed the engine's own check of the new file, which is all it can do.
        assert!(old.join(plan::relative_path(&changed.name).unwrap()).is_file(), "{what}: no stand-in was built");
    }
    assert!(solved >= 5, "only {solved} of 30 made-up patches had a file that could get a stand-in");
}
