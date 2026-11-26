//! Tests the tool that reads off a patch file what a way back would have to keep and what
//! room the immediate way needs.

/// The made-up installs and patch files of the engine's own tests.
#[path = "../../engine/tests/support/mod.rs"]
mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use mfp_engine::container::PatchFile;
use mfp_engine::immediate;
use mfp_engine::plan::{self, Layout, Plan};
use mfp_engine::stage::Checked;
use mfp_tools::undo_size::{self, Counts, Fate, Floor, Old, Rule, Sequence};
use support::{GO, LAYOUTS, Op, PatchBuilder, World, copy, on_disk};

/// Writes `builder`'s patch into `dir` and reads it the way a run does.
fn written(builder: &PatchBuilder, dir: &Path) -> (PatchFile, Plan) {
    let path = dir.join("made-up.patch");
    builder.write(&path);
    let patch = PatchFile::open(&path).unwrap();
    let plan = plan::scan(&patch, &GO).unwrap();
    (patch, plan)
}

/// The length of the file a patch calls `name` in `install`, as the immediate way counts it.
fn len_in(install: &Path, name: &str) -> u64 {
    fs::metadata(on_disk(install, name)).ok().filter(|meta| meta.is_file()).map_or(0, |meta| meta.len())
}

fn old(name: &str, fate: Fate, extent: u64, covered: u64, len: Option<u64>) -> Old {
    Old { name: name.to_owned(), fate, extent, covered, len }
}

#[test]
fn what_copies_read_and_what_becomes_of_each_old_file_is_told_from_the_patch() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b, c, d, e) = (r"Data\A.wz", r"Data\B.wz", r"Data\C.wz", r"Data\D.wz", r"Data\E.wz");
    let (x, y) = (r"Data\Old\x.wz", r"Data\Old\y.wz");
    let mut builder = PatchBuilder::new(Layout::Table);
    builder.original(a, &[1; 100]).original(b, &[2; 60]).original(x, &[3; 20]);
    // A reads 0..10 and 20..50 of itself, the stretch 30..40 twice, and 5..15 of B.
    let a_ops = [
        copy(a, 0, 10),
        Op::Literal(vec![9; 5]),
        copy(a, 20, 20),
        Op::Fill(0, 7),
        copy("data/b.WZ", 5, 10),
        copy(a, 30, 20),
    ];
    builder.rebuild(a, &[1; 100], &[0; 72], &a_ops);
    builder.create(c, &[4; 33]);
    // E is new, built from B and from a file in a folder that is deleted further down.
    builder.rebuild(e, &[], &[0; 14], &[copy(b, 0, 8), copy(x, 2, 6)]);
    builder.delete(d).create_dir(r"Data\New").create(r"Data\Empty.wz", &[]).delete("Data\\Old\\");
    let (patch, plan) = written(&builder, dir.path());

    let measured = undo_size::measure(&patch, &plan, None, &GO).unwrap();
    assert_eq!(
        measured.counts,
        Counts {
            folders: 1,
            deletes: 2,
            whole: 1,
            whole_bytes: 33,
            empty: 1,
            rebuilds: 2,
            rebuilt_bytes: 72 + 14,
            copies: 6,
            copy_bytes: 10 + 20 + 10 + 20 + 8 + 6,
            literals: 1,
            literal_bytes: 5,
            fills: 1,
            fill_bytes: 7,
        }
    );
    assert_eq!(
        measured.olds,
        [
            old(a, Fate::Rebuilt, 50, 40, None),
            old(c, Fate::ReplacedWhole, 0, 0, None),
            old(e, Fate::Rebuilt, 0, 0, None),
            old(d, Fate::Deleted, 0, 0, None),
            old("Data\\Old\\", Fate::Deleted, 0, 0, None),
            // Read under two spellings, one file. It stays, so nothing of it is kept.
            old("data/b.WZ", Fate::Kept, 15, 15, None),
            // Not named by an entry, and gone with its folder all the same.
            old(x, Fate::Deleted, 8, 6, None),
        ]
    );
    // From the patch alone: what lies below the highest byte read and is read by no copy.
    assert_eq!(measured.least_kept(), (50 - 40) + (8 - 6));
    assert_eq!((measured.kept(), measured.too_short()), (0, 0));

    // With the old install, the lengths say the rest. C and D are kept whole, the folder
    // by what is in it and has no line of its own, and E was not there before.
    let install = dir.path().join("install");
    for (name, len) in [(a, 100), (b, 60), (c, 40), (d, 30), (x, 20), (y, 25)] {
        fs::create_dir_all(on_disk(&install, name).parent().unwrap()).unwrap();
        fs::write(on_disk(&install, name), vec![0u8; len]).unwrap();
    }
    let measured = undo_size::measure(&patch, &plan, Some(&install), &GO).unwrap();
    let lens: Vec<Option<u64>> = measured.olds.iter().map(|old| old.len).collect();
    assert_eq!(lens, [Some(100), Some(40), None, Some(30), Some(25), Some(60), Some(20)]);
    assert_eq!(measured.kept(), (100 - 40) + 40 + 30 + 25 + (20 - 6));
    assert_eq!(measured.too_short(), 0);

    // A file shorter than the patch reads it gives the folder away as another version.
    fs::write(on_disk(&install, a), vec![0u8; 49]).unwrap();
    assert_eq!(undo_size::measure(&patch, &plan, Some(&install), &GO).unwrap().too_short(), 1);
}

#[test]
fn room_under_the_engines_rule_is_the_engines_own_figure() {
    for layout in LAYOUTS {
        for seed in 1..=40 {
            let world = World::random(seed, layout);
            let plan = plan::scan(&PatchFile::open(&world.patch).unwrap(), &GO).unwrap();
            let floor = undo_size::floor(
                &plan,
                &|name| len_in(&world.install, name),
                Rule::ConsumersBuilt,
                Sequence::AsInPatch,
            );
            let engine = immediate::space_needed(&plan, &Checked::default(), &BTreeSet::new(), &world.install);
            assert_eq!(floor.peak, engine, "{layout:?} seed {seed}");
        }
    }
}

#[test]
fn stricter_rule_never_needs_less_room_and_no_rule_needs_more_than_the_safe_way() {
    for layout in LAYOUTS {
        for seed in 1..=40 {
            let what = format!("{layout:?} seed {seed}");
            let world = World::random(seed, layout);
            let plan = plan::scan(&PatchFile::open(&world.patch).unwrap(), &GO).unwrap();
            let old_len = |name: &str| len_in(&world.install, name);
            let floor = |rule, sequence| undo_size::floor(&plan, &old_len, rule, sequence);

            let built = floor(Rule::ConsumersBuilt, Sequence::AsInPatch);
            let along = floor(Rule::ConsumersGoAlong, Sequence::AsInPatch);
            let in_place = floor(Rule::ConsumersInPlace, Sequence::AsInPatch);
            assert!(
                built.peak <= along.peak && along.peak <= in_place.peak,
                "{what}: {built:?} {along:?} {in_place:?}"
            );
            assert!(
                built.left <= along.left && along.left <= in_place.left,
                "{what}: {built:?} {along:?} {in_place:?}"
            );
            for rule in [Rule::ConsumersBuilt, Rule::ConsumersGoAlong, Rule::ConsumersInPlace] {
                for sequence in [Sequence::AsInPatch, Sequence::ConsumersFirst] {
                    let floor = floor(rule, sequence);
                    assert!(floor.peak <= plan.new_bytes(), "{what}: {rule:?} {sequence:?} {floor:?}");
                }
            }
        }
    }
}

#[test]
fn files_built_from_one_another_in_a_circle_go_in_together_or_wait_for_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let (p, q) = (r"Data\P.wz", r"Data\Q.wz");
    let (s, x, z) = (r"Data\S.wz", r"Data\X.wz", r"Data\Z.wz");
    let sizes = [(p, 50), (q, 30), (s, 20), (x, 10), (z, 5)];
    let old_len = |name: &str| sizes.iter().find(|(known, _)| *known == name).map_or(0, |(_, len)| *len);
    let room = |builder: &PatchBuilder, rule, sequence| {
        let (_, plan) = written(builder, dir.path());
        undo_size::floor(&plan, &old_len, rule, sequence)
    };
    let floor = |peak, left, left_bytes, largest_group| Floor { peak, left, left_bytes, largest_group };

    // P is built from the old Q and Q from the old P: 60 and 40 are written before either
    // may go, and then 80 come back.
    let mut circle = PatchBuilder::new(Layout::Table);
    circle.original(p, &[1; 50]).original(q, &[2; 30]);
    circle.rebuild(p, &[1; 50], &[0; 60], &[copy(q, 0, 30), copy(p, 0, 30)]);
    circle.rebuild(q, &[2; 30], &[0; 40], &[copy(p, 10, 40)]);
    for sequence in [Sequence::AsInPatch, Sequence::ConsumersFirst] {
        assert_eq!(room(&circle, Rule::ConsumersBuilt, sequence), floor(100, 0, 0, 2));
        assert_eq!(room(&circle, Rule::ConsumersGoAlong, sequence), floor(100, 0, 0, 2));
        // Neither is ever in place before the other, so both wait for the apply.
        assert_eq!(room(&circle, Rule::ConsumersInPlace, sequence), floor(100, 2, 100, 0));
    }

    // A chain: X is built from the old S, and Z from the old X. In the order of the patch,
    // S (20 new) is built first and waits for X (40), which waits for Z (15).
    let mut chain = PatchBuilder::new(Layout::Table);
    chain.original(s, &[3; 20]).original(x, &[4; 10]).original(z, &[5; 5]);
    chain.create(s, &[6; 20]);
    chain.rebuild(x, &[4; 10], &[0; 40], &[copy(s, 0, 20), copy(x, 0, 10), Op::Fill(0, 10)]);
    chain.rebuild(z, &[5; 5], &[0; 15], &[copy(x, 0, 10), copy(z, 0, 5)]);
    // Once X is built the new S may go under today's rule: 60 written, 20 back, 15 more.
    assert_eq!(room(&chain, Rule::ConsumersBuilt, Sequence::AsInPatch), floor(60, 0, 0, 2));
    // Under the stricter rules S waits until X is in place, which is when Z is: 75 in all.
    assert_eq!(room(&chain, Rule::ConsumersInPlace, Sequence::AsInPatch), floor(75, 0, 0, 1));
    assert_eq!(room(&chain, Rule::ConsumersGoAlong, Sequence::AsInPatch), floor(75, 0, 0, 3));
    // Built the other way round, each file goes into place as it is built: 15 and 5 back,
    // 40 and 10 back, 20 and 20 back. The most is 60, with the last of them.
    for rule in [Rule::ConsumersBuilt, Rule::ConsumersInPlace, Rule::ConsumersGoAlong] {
        assert_eq!(room(&chain, rule, Sequence::ConsumersFirst), floor(60, 0, 0, 1), "{rule:?}");
    }
}
