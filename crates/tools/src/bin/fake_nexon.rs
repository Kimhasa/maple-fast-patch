//! Makes and runs a made-up world to try the app against, without the real game and without
//! Nexon's servers (see `mfp_tools::fake_world`).
//!
//!     fake_nexon make <dir> [--server live|test] [--versions N] [--minors M] [--size <MB>]
//!         [--from <number>] [--seed <number>]
//!     fake_nexon serve <dir> [--port P] [--kbps <KB a second>]
//!     fake_nexon publish <dir> <patch-file|all>
//!     fake_nexon maintenance <dir> announce|begin|extend|end [--server live|test] [--in <minutes>]
//!     fake_nexon state <dir>
//!
//! `make` makes the world in `<dir>`, which must not exist yet or be empty: a small install
//! of the client at an old version, and patch files for N main versions after it and M minor
//! patches on the newest. `--from` is the number of the installed version as the patch files
//! count it, and `--size` the megabytes of game data, 256 without it and 1024 at most (making
//! a world takes about four times that in memory). No patch file is published at first.
//!
//! `serve` hands the world out on this PC and on nothing else, and prints every request.
//! With `--kbps` a patch file is sent no faster than that, so that a download lasts long
//! enough to be watched, paused or cut off.
//! `publish` makes a patch file appear on the server, and `maintenance` changes what the
//! inspection service and the notice boards say: a maintenance announced for later (`--in`,
//! 30 minutes without it), begun now for an hour, extended by an hour, or over. Both work
//! while the server runs. `state` says where the world stands.
//!
//! Nothing here reads or writes outside `<dir>`, and `make` refuses a folder under `Nexon`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mfp_kms::Server;
use mfp_kms::notice::{self, clock};
use mfp_tools::fake_notice::{self, BOARD_FILE, Change, INSPECTION_FILE, Maintenance, TEST_BOARD_FILE};
use mfp_tools::fake_world::{self, HELD_DIR, INSTALL_DIR, Options, SERVED_DIR, World};
use mfp_tools::{clients, fake_server};

/// The port the server listens on unless another is asked for.
const PORT: u16 = 18421;

/// Takes an option and its value out of the arguments.
fn take(args: &mut Vec<String>, name: &str) -> Result<Option<String>, String> {
    let Some(at) = args.iter().position(|arg| arg == name) else { return Ok(None) };
    let value = args.get(at + 1).cloned().ok_or(format!("{name} needs a value"))?;
    args.drain(at..=at + 1);
    Ok(Some(value))
}

/// Takes an option whose value is a number out of the arguments.
fn take_number<T: std::str::FromStr>(args: &mut Vec<String>, name: &str) -> Result<Option<T>, String> {
    take(args, name)?.map(|value| value.parse().map_err(|_| format!("{name} needs a number, not {value}"))).transpose()
}

fn main() -> std::process::ExitCode {
    mfp_tools::end(run())
}

fn run() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let server = clients::take_server(&mut args)?;
    if args.len() < 2 {
        return Err("say what to do and where: make, serve, publish, maintenance or state, then the folder".into());
    }
    let (what, dir) = (args.remove(0), PathBuf::from(args.remove(0)));
    match what.as_str() {
        "make" => make(&dir, server, args),
        "serve" => serve(dir, args),
        "publish" => publish(&dir, args),
        "maintenance" => maintenance(&dir, server, args),
        "state" => state(&dir),
        other => Err(format!("unknown command {other}")),
    }
}

fn make(dir: &Path, server: Server, mut args: Vec<String>) -> Result<(), String> {
    let mut options = Options::new(server);
    options.versions = take_number(&mut args, "--versions")?.unwrap_or(options.versions);
    options.minors = take_number(&mut args, "--minors")?.unwrap_or(options.minors);
    options.megabytes = take_number(&mut args, "--size")?.unwrap_or(options.megabytes);
    options.from = take_number(&mut args, "--from")?;
    options.seed = take_number(&mut args, "--seed")?;
    if let Some(left) = args.first() {
        return Err(format!("unknown option {left}"));
    }
    // The stand-in for the game's programs is built together with this program.
    let launcher = std::env::current_exe()
        .map_err(|e| format!("find this program: {e}"))?
        .with_file_name(format!("fake_launcher{}", std::env::consts::EXE_SUFFIX));
    if !launcher.is_file() {
        return Err("fake_launcher is not beside this program; build it with `cargo build -p mfp-tools`".into());
    }
    let world = fake_world::make(dir, &options, &launcher, notice::now())?;
    print!("{}", world.table());
    if server == Server::Test {
        println!("  the test world brings out a version with its first minor patch: publishing the last main patch");
        println!("  publishes Minor/01to02.patch with it");
    }
    println!("nothing is published yet. Next, with the same folder:");
    println!("  fake_nexon serve <dir>                   hands the world out on this PC");
    println!("  fake_nexon publish <dir> <file|all>      makes a patch file appear");
    println!("  fake_nexon maintenance <dir> begin       announces a maintenance as underway");
    Ok(())
}

fn serve(dir: PathBuf, mut args: Vec<String>) -> Result<(), String> {
    let port = take_number(&mut args, "--port")?.unwrap_or(PORT);
    let pace = take_number::<u64>(&mut args, "--kbps")?.map(|kilobytes| kilobytes.max(1) * 1024);
    let world = World::read(&dir)?;
    let log: fake_server::Log = Arc::new(|line| println!("{line}"));
    let address =
        fake_server::serve(dir.clone(), port, pace, log).map_err(|e| format!("listen on port {port}: {e}"))?;
    let root = world.patches.first().and_then(|patch| patch.path.split('/').next()).unwrap_or("Patch");
    println!("serving the {} client's world on this PC only. Ctrl+C ends it.", world.server.key());
    if let Some(pace) = pace {
        println!("  patch files are sent at {} KB a second at most", pace / 1024);
    }
    println!("  patch files:                    http://{address}/{root}");
    println!("  inspection service:             http://{address}/inspection   ({INSPECTION_FILE})");
    println!("  notice board of the live game:  http://{address}/board        ({BOARD_FILE})");
    println!("  notice board of the test world: http://{address}/board-test   ({TEST_BOARD_FILE})");
    let published = fake_world::published(&dir);
    println!("  published now: {}", if published.is_empty() { "nothing".to_owned() } else { published.join(", ") });
    loop {
        std::thread::park();
    }
}

fn publish(dir: &Path, args: Vec<String>) -> Result<(), String> {
    let which = args.first().ok_or("say which patch file, or all")?;
    for path in fake_world::publish(dir, which)? {
        println!("published: {path}");
    }
    Ok(())
}

fn maintenance(dir: &Path, server: Server, mut args: Vec<String>) -> Result<(), String> {
    let minutes = take_number(&mut args, "--in")?.unwrap_or(30);
    let change = match args.first().map(String::as_str) {
        Some("announce") => Change::Announce { minutes },
        Some("begin") => Change::Begin,
        Some("extend") => Change::Extend,
        Some("end") => Change::End,
        _ => return Err("say what happens to the maintenance: announce, begin, extend or end".into()),
    };
    // A patch of the live game is announced for the version it brings.
    let world = World::read(dir)?;
    let label = world.client().version_label(world.newest().number);
    let told = fake_notice::change(dir, server, change, &label, notice::now())?;
    print_maintenance(server, &told);
    println!("  {INSPECTION_FILE}, {BOARD_FILE} and {TEST_BOARD_FILE} are written anew");
    Ok(())
}

fn print_maintenance(server: Server, told: &Maintenance) {
    println!("{}: {}", server.key(), told.title(server));
    let state = if told.over { ", marked as over" } else { "" };
    println!("  from {} to {} Korean time{state}", clock(told.begins), clock(told.ends));
}

fn state(dir: &Path) -> Result<(), String> {
    let world = World::read(dir)?;
    let client = world.client();
    match world.version_of(&fake_world::install_dir(dir)) {
        Some(version) => println!("{INSTALL_DIR}\\ is at {} (number {})", version.label(client), version.number),
        None => println!("{INSTALL_DIR}\\ holds the files of none of this world's versions"),
    }
    for patch in &world.patches {
        let place = if dir.join(SERVED_DIR).join(&patch.path).is_file() { "published" } else { HELD_DIR };
        println!("  {place:<10}{}", patch.path);
    }
    for server in Server::ALL {
        match Maintenance::read(dir, server) {
            Some(told) => print_maintenance(server, &told),
            None => println!("{}: no maintenance has been announced", server.key()),
        }
    }
    Ok(())
}
