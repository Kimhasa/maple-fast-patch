//! Tests of the forms a patch file comes in: the patch block as the whole file, or behind a
//! program with a trailer that says where it is, and with or without a zlib header in front
//! of the compressed contents. Every form has to be read as the same patch, and a file
//! that is cut short or has one wrong byte must never be taken for a good one.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use mfp_engine::container::PatchFile;
use mfp_engine::plan::{self, Layout, Plan};
use mfp_engine::{Error, Outcome, crc};
use support::{GO, LAYOUTS, Op, PatchBuilder, Rng, World, options, run};

/// The number that ends a trailer.
const MARK: u32 = 0xf2f7_fbf3;
/// Bytes of the block before the compressed contents: signature, version, checksum.
const HEADER: usize = 16;

/// The forms a patch file comes in.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Form {
    /// The block is the whole file, as in a `.patch` from the server.
    Plain,
    /// The same, with no zlib header in front of the compressed contents.
    NoZlibHeader,
    /// A program, the block, a notice, and a trailer of three 32-bit numbers.
    Exe32,
    /// A program, the block, a notice, and a trailer of three 64-bit numbers.
    Exe64,
    /// No program: the block, a notice, and a trailer of three 64-bit numbers.
    Patch64,
}

const FORMS: [Form; 5] = [Form::Plain, Form::NoZlibHeader, Form::Exe32, Form::Exe64, Form::Patch64];

/// A patch file in one of its forms, and where its parts are.
struct Wrapped {
    bytes: Vec<u8>,
    /// The program in front of the block. Empty where the form has none.
    program: Range<usize>,
    block: Range<usize>,
    notice: Range<usize>,
    trailer: Range<usize>,
}

impl Wrapped {
    /// The part of the block that its checksum covers.
    fn payload(&self) -> Range<usize> {
        self.block.start + HEADER..self.block.end
    }

    /// The signature, without which the file is no patch file.
    fn signature(&self) -> Range<usize> {
        self.block.start..self.block.start + 8
    }

    /// The checksum stored in the block.
    fn checksum(&self) -> Range<usize> {
        self.block.start + 12..self.block.start + HEADER
    }
}

/// Puts a patch file that is in the plain form into another form.
fn wrap(plain: &[u8], form: Form) -> Wrapped {
    let program = match form {
        // A program starts with these two letters. What follows them here is anything.
        Form::Exe32 | Form::Exe64 => [b"MZ".to_vec(), Rng::new(99).bytes(3000)].concat(),
        _ => Vec::new(),
    };
    let block = match form {
        Form::NoZlibHeader => without_zlib_header(plain),
        _ => plain.to_vec(),
    };
    let notice: &[u8] = match form {
        Form::Plain | Form::NoZlibHeader => b"",
        _ => b"What a self-patching program shows before it starts.",
    };
    let trailer: Vec<u8> = match form {
        Form::Plain | Form::NoZlibHeader => Vec::new(),
        Form::Exe32 => {
            [block.len() as u32, notice.len() as u32, MARK].iter().flat_map(|word| word.to_le_bytes()).collect()
        }
        Form::Exe64 | Form::Patch64 => [block.len() as u64, notice.len() as u64, u64::from(MARK)]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect(),
    };

    let block_at = program.len();
    let notice_at = block_at + block.len();
    let trailer_at = notice_at + notice.len();
    let bytes = [program.as_slice(), block.as_slice(), notice, trailer.as_slice()].concat();
    Wrapped {
        program: 0..block_at,
        block: block_at..notice_at,
        notice: notice_at..trailer_at,
        trailer: trailer_at..bytes.len(),
        bytes,
    }
}

/// The same patch with its contents compressed again, this time without the two bytes a
/// zlib stream starts with.
fn without_zlib_header(plain: &[u8]) -> Vec<u8> {
    let mut contents = Vec::new();
    flate2::read::ZlibDecoder::new(&plain[HEADER..]).read_to_end(&mut contents).unwrap();
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&contents).unwrap();
    let payload = encoder.finish().unwrap();
    // The engine tells by these two bytes whether there is a header. This has to be a
    // payload it takes for one without.
    assert!(
        !(payload[0] == 0x78 && (u16::from(payload[0]) << 8 | u16::from(payload[1])) % 31 == 0),
        "the compressed contents happen to start like a zlib header"
    );

    let mut block = plain[..12].to_vec();
    block.extend(crc::update(0, &payload).to_le_bytes());
    block.extend(payload);
    block
}

/// Where a file in `form` is written, next to the patch file of a made-up install.
fn path_for(world: &World, form: Form) -> PathBuf {
    world.patch.with_file_name(match form {
        Form::Exe32 | Form::Exe64 => "wrapped.exe",
        _ => "wrapped.patch",
    })
}

/// Opens a patch file, checks it and reads what it will do, as a run does.
fn read(path: &Path) -> Result<(PatchFile, Plan), Error> {
    let patch = PatchFile::open(path)?;
    patch.verify(&GO, |_| {})?;
    let plan = plan::scan(&patch, &GO)?;
    Ok((patch, plan))
}

/// Fails unless two readings of a patch say the same thing.
fn assert_same_plan(found: &Plan, expected: &Plan, what: &str) {
    assert_eq!(found.layout, expected.layout, "{what}");
    assert_eq!(found.originals, expected.originals, "{what}");
    assert_eq!(found.contents_len, expected.contents_len, "{what}");
    assert!(found.entries == expected.entries, "{what}: the entries differ");
}

/// A small patch in the plain form: enough to have a table, entries of every kind and a
/// compressed payload of a few thousand bytes.
fn small_patch() -> Vec<u8> {
    let mut rng = Rng::new(7);
    let (old, fresh, literal) = (rng.bytes(4000), rng.bytes(2500), rng.bytes(700));
    let new = [&old[100..1000], &literal[..], &[3u8; 50][..]].concat();
    let ops = [Op::Copy(r"Data\A.wz".into(), 100, 900), Op::Literal(literal), Op::Fill(3, 50)];
    let mut builder = PatchBuilder::new(Layout::Table);
    builder.original(r"Data\A.wz", &old);
    builder.create_dir(r"Data\Fresh");
    builder.create(r"Data\Fresh\New.wz", &fresh);
    builder.rebuild(r"Data\A.wz", &old, &new, &ops);
    builder.delete(r"Data\Gone.wz");
    builder.bytes()
}

#[test]
fn every_form_is_read_and_applied_as_the_same_patch() {
    for layout in LAYOUTS {
        for seed in [31, 32] {
            let plain_world = World::random(seed, layout);
            let plain = fs::read(&plain_world.patch).unwrap();
            let (plain_patch, plain_plan) = read(&plain_world.patch).unwrap();
            assert_eq!(plain_plan.layout, layout);

            for form in FORMS {
                let what = format!("{layout:?} seed {seed}, {form:?}");
                // The same seed makes the same install and the same patch.
                let world = World::random(seed, layout);
                assert!(fs::read(&world.patch).unwrap() == plain, "{what}: the made-up patch is not the same twice");
                let path = path_for(&world, form);
                fs::write(&path, wrap(&plain, form).bytes).unwrap();

                let (patch, plan) = read(&path).unwrap_or_else(|e| panic!("{what}: {e}"));
                assert_same_plan(&plan, &plain_plan, &what);
                // Wrapped, it is the same block. Compressed again, it is another.
                assert_eq!(patch.id() == plain_patch.id(), form != Form::NoZlibHeader, "{what}");

                let outcome =
                    run(&options(&world.install, &path), &GO, &mut |_| {}).unwrap_or_else(|e| panic!("{what}: {e}"));
                assert!(outcome.files_changed > 0 && !outcome.immediate, "{what}: {outcome:?}");
                world.assert_patched(&what);
                let again = run(&options(&world.install, &path_for(&world, form)), &GO, &mut |_| {}).unwrap();
                assert_eq!(again, Outcome { files_changed: 0, immediate: false }, "{what}, again");
            }
        }
    }
}

#[test]
fn short_trailer_behind_a_file_that_is_no_program_is_refused_before_anything_is_touched() {
    // Only a program carries the trailer of 32-bit numbers. Behind a block that stands at
    // the start of the file it is not looked for: the block is taken to be the whole file,
    // and its checksum does not hold for the notice and the trailer that follow it.
    let world = World::random(33, Layout::Table);
    let plain = fs::read(&world.patch).unwrap();
    let as_program = wrap(&plain, Form::Exe32);
    let path = path_for(&world, Form::Plain);
    fs::write(&path, &as_program.bytes[as_program.block.start..]).unwrap();

    let err = read(&path).map(drop).unwrap_err();
    assert!(matches!(err, Error::CorruptPatch(_)), "unexpected error: {err}");
    let err = run(&options(&world.install, &path), &GO, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::CorruptPatch(_)), "unexpected error: {err}");
    world.assert_untouched("a short trailer behind a plain block");
    assert!(!world.install.join(".maple-fast-patch").exists());
}

#[test]
fn file_that_is_cut_short_is_never_taken_for_a_patch() {
    let dir = tempfile::tempdir().unwrap();
    let plain = small_patch();
    for form in FORMS {
        let whole = wrap(&plain, form).bytes;
        let path = dir.path().join(format!("{form:?}.bin"));
        fs::write(&path, &whole).unwrap();
        read(&path).unwrap_or_else(|e| panic!("{form:?}, whole: {e}"));

        // Cut inside the trailer, right in front of it, in the notice, in the contents,
        // in the header, and down to nothing.
        let len = whole.len();
        let mut kept: Vec<usize> =
            [1, 2, 4, 8, 11, 12, 13, 16, 23, 24, 25, 60, 100].iter().map(|cut| len - cut).collect();
        kept.extend([len / 2, len / 3, 2000, 17, 16, 15, 8, 2, 1, 0]);
        for keep in kept {
            fs::write(&path, &whole[..keep]).unwrap();
            let refused = read(&path).map(drop);
            assert!(
                matches!(refused, Err(Error::NotAPatchFile | Error::CorruptPatch(_))),
                "{form:?} cut to {keep} of {len} bytes: {refused:?}"
            );
        }
    }
}

#[test]
fn run_on_a_file_that_is_cut_short_changes_nothing() {
    for form in FORMS {
        let world = World::random(34, Layout::Table);
        let whole = wrap(&fs::read(&world.patch).unwrap(), form).bytes;
        let path = path_for(&world, form);
        fs::write(&path, &whole[..whole.len() - 1]).unwrap();

        let err = run(&options(&world.install, &path), &GO, &mut |_| {}).unwrap_err();
        assert!(matches!(err, Error::NotAPatchFile | Error::CorruptPatch(_)), "{form:?}: unexpected error: {err}");
        world.assert_untouched(&format!("{form:?}, one byte short"));
        assert!(!world.install.join(".maple-fast-patch").exists(), "{form:?}: work folder left behind");
    }
}

#[test]
fn one_wrong_byte_is_refused_or_changes_nothing_about_the_patch() {
    let dir = tempfile::tempdir().unwrap();
    let plain = small_patch();
    let (_, expected) = {
        let path = dir.path().join("plain.patch");
        fs::write(&path, &plain).unwrap();
        read(&path).unwrap()
    };
    let (mut refused, mut harmless) = (0, 0);
    for form in FORMS {
        let wrapped = wrap(&plain, form);
        let path = dir.path().join(format!("{form:?}.bin"));
        // Every byte of what tells where the patch is and whether it is whole, and bytes
        // all over the rest.
        let mut places: Vec<usize> = (wrapped.block.start..wrapped.block.start + HEADER).collect();
        places.extend(wrapped.trailer.clone());
        places.extend(wrapped.payload().step_by(37));
        places.extend([wrapped.payload().start + 1, wrapped.payload().end - 1]);
        places.extend(wrapped.program.clone().take(4));
        places.extend(wrapped.program.clone().step_by(501));
        places.extend(wrapped.notice.clone().step_by(7));

        for at in places {
            for bit in [0x01, 0x80] {
                let what = format!("{form:?}, byte {at} of {} with {bit:#04x} flipped", wrapped.bytes.len());
                let mut bytes = wrapped.bytes.clone();
                bytes[at] ^= bit;
                fs::write(&path, &bytes).unwrap();
                // What the checksum covers, the checksum itself and the signature can
                // never be wrong and pass.
                let guarded = wrapped.payload().contains(&at)
                    || wrapped.checksum().contains(&at)
                    || wrapped.signature().contains(&at);
                match read(&path) {
                    Err(Error::NotAPatchFile | Error::CorruptPatch(_)) => refused += 1,
                    // Read all the same: then the byte was one that carries nothing of the
                    // patch, and what was read is the patch, exactly.
                    Ok((_, plan)) => {
                        assert!(!guarded, "{what}: the file was accepted");
                        assert_same_plan(&plan, &expected, &what);
                        harmless += 1;
                    }
                    Err(other) => panic!("{what}: unexpected error: {other}"),
                }
            }
        }
    }
    // Both happen, or the test is not looking where it thinks it is: the version number,
    // the program and the notice may be anything, the rest may not.
    assert!(refused > 500 && harmless > 20, "{refused} refused, {harmless} harmless");
}
