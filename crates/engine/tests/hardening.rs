//! Tests that push a whole patch run through many made-up installs and through every point
//! at which it can be paused or cut off.

mod support;

use std::sync::atomic::{AtomicBool, Ordering};

use mfp_engine::apply::{self, Leftover};
use mfp_engine::container::PatchFile;
use mfp_engine::plan::{self, Layout};
use mfp_engine::stage::{self, Dirs};
use mfp_engine::{Error, Event};
use support::{GO, LAYOUTS, World, note_placed, run};

#[test]
fn random_patches_produce_exactly_the_expected_install() {
    for layout in LAYOUTS {
        for seed in 1..=25 {
            let world = World::random(seed, layout);
            let what = format!("{layout:?} seed {seed}");

            run(&world.options(), &GO, &mut |_| {}).unwrap_or_else(|e| panic!("{what}: {e}"));
            world.assert_patched(&what);

            // A second run finds nothing left to do and changes nothing.
            let again = run(&world.options(), &GO, &mut |_| {}).unwrap_or_else(|e| panic!("{what}, again: {e}"));
            assert_eq!((again.files_changed, again.immediate), (0, false), "{what}: second run changed files");
            world.assert_patched(&format!("{what}, again"));
        }
    }
}

#[test]
fn pausing_at_any_moment_and_continuing_ends_in_the_expected_install() {
    for layout in LAYOUTS {
        for seed in [3, 11] {
            // Pause at the first event, then at the second, and so on until a run gets
            // through without reaching the pause.
            for pause_at in 1.. {
                let world = World::random(seed, layout);
                let what = format!("{layout:?} seed {seed}, paused at event {pause_at}");
                let stop = AtomicBool::new(false);
                let mut events = 0;
                // The files named as placed, over all the runs it takes.
                let mut placed = Vec::new();
                let first = run(&world.options(), &stop, &mut |event| {
                    events += 1;
                    if events == pause_at {
                        stop.store(true, Ordering::Relaxed);
                    }
                    note_placed(&mut placed, event);
                });
                match first {
                    Ok(_) => {
                        world.assert_patched(&what);
                        world.assert_named_once(&placed, &what);
                        if events < pause_at {
                            break;
                        }
                        continue;
                    }
                    Err(Error::Stopped) => world.assert_untouched(&what),
                    Err(err) => panic!("{what}: {err}"),
                }
                // The safe way puts nothing in place before its apply, and a paused run
                // never got that far.
                assert!(placed.is_empty(), "{what}: {placed:?}");
                run(&world.options(), &GO, &mut |event| note_placed(&mut placed, event))
                    .unwrap_or_else(|e| panic!("{what}, continued: {e}"));
                world.assert_patched(&format!("{what}, continued"));
                world.assert_named_once(&placed, &format!("{what}, continued"));
            }
        }
    }
}

#[test]
fn apply_cut_off_at_any_moment_is_undone_and_the_next_run_finishes() {
    for layout in LAYOUTS {
        for seed in [5, 17] {
            for fuse in 0.. {
                let world = World::random(seed, layout);
                let what = format!("{layout:?} seed {seed}, cut off at moment {fuse}");
                let dirs = Dirs::new(&world.install);
                let patch = PatchFile::open(&world.patch).unwrap();
                let plan = plan::scan(&patch, &GO).unwrap();
                let checked = stage::check_originals(&plan, &world.install, &GO, &|_, _| {}).unwrap();
                stage::stage(&patch, &plan, &checked, &dirs, &GO, &mut |_, _, _| {}).unwrap();

                let ran_to_the_end = apply::apply_then_crash(&plan, &checked, &dirs, fuse).unwrap();

                assert_eq!(apply::recover(&dirs).unwrap(), Leftover::RolledBack, "{what}");
                world.assert_untouched(&what);
                run(&world.options(), &GO, &mut |_| {}).unwrap_or_else(|e| panic!("{what}, next run: {e}"));
                world.assert_patched(&format!("{what}, next run"));

                if ran_to_the_end {
                    break;
                }
            }
        }
    }
}

#[test]
fn every_step_of_a_run_is_announced_in_order() {
    let world = World::random(7, Layout::Table);
    let mut stages = Vec::new();
    let mut last_progress = None;
    // Steps during which a game file was named, which is what the screen shows as its one line.
    let mut naming = std::collections::BTreeSet::new();
    run(&world.options(), &GO, &mut |event| match event {
        Event::Stage(stage) => {
            stages.push(format!("{stage:?}"));
            last_progress = None;
        }
        Event::Progress { done, total } => {
            assert!(done <= total, "progress {done} exceeds its total {total}");
            assert!(last_progress <= Some(done), "progress went backwards within a step");
            last_progress = Some(done);
        }
        Event::File(name) => {
            assert!(!name.is_empty(), "a file was named without a name");
            naming.insert(stages.last().cloned().unwrap_or_default());
        }
        _ => {}
    })
    .unwrap();
    assert_eq!(stages, ["VerifyPatch", "VerifyInstall", "Rebuild", "Apply"]);
    let naming: Vec<_> = naming.into_iter().collect();
    assert_eq!(naming, ["Apply", "Rebuild", "VerifyInstall"], "every step on game files names the file it is on");
}
