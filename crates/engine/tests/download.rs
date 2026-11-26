//! Tests the downloader against a small web server that can misbehave on request.

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mfp_engine::container::PatchFile;
use mfp_engine::plan::Layout;
use mfp_engine::run::{Disk, half_patched, run_on_disk};
use mfp_engine::{Error, Event, Options, Outcome, Source, Stage, download};
use support::{Drive, GO, LIVE, PatchBuilder, Rng, World, files_under, run};

/// What the server holds and how it should misbehave.
#[derive(Default)]
struct Site {
    body: Vec<u8>,
    etag: String,
    /// Answer every request with this status instead of serving the file.
    status: Option<u16>,
    /// Send only this many bytes of the next download, then drop the connection.
    cut_next_after: Option<usize>,
    /// Drop every connection when it reaches this position in the file.
    down_from: Option<usize>,
    /// Answer this many of the next downloads with "service unavailable".
    unavailable_next: usize,
    /// Flip a byte in the next download.
    damage_next: bool,
    /// Send the whole file even when asked for a part.
    ignore_range: bool,
    /// One line per request: the method and the range asked for.
    log: Vec<String>,
    /// Who each request said it came from. Empty for a request that did not say.
    senders: Vec<String>,
}

struct Server {
    url: String,
    site: Arc<Mutex<Site>>,
}

impl Server {
    fn start(body: Vec<u8>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.patch", listener.local_addr().unwrap());
        let site = Arc::new(Mutex::new(Site { body, etag: "\"v1\"".into(), ..Site::default() }));
        let shared = site.clone();
        std::thread::spawn(move || {
            for connection in listener.incoming().flatten() {
                let _ = answer(connection, &shared);
            }
        });
        Server { url, site }
    }

    fn set(&self, change: impl FnOnce(&mut Site)) {
        change(&mut self.site.lock().unwrap());
    }

    fn log(&self) -> Vec<String> {
        self.site.lock().unwrap().log.clone()
    }
}

fn answer(mut connection: TcpStream, site: &Mutex<Site>) -> std::io::Result<()> {
    let mut reader = BufReader::new(connection.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let method = line.split(' ').next().unwrap_or("").to_owned();
    let (mut range, mut if_range) = (None, None);
    let mut sender = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else { continue };
        match name.to_ascii_lowercase().as_str() {
            "range" => {
                range = value.trim().strip_prefix("bytes=").and_then(|range| {
                    let (start, end) = range.split_once('-')?;
                    Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()))
                })
            }
            "if-range" => if_range = Some(value.trim().to_owned()),
            "user-agent" => sender = value.trim().to_owned(),
            _ => {}
        }
    }

    let mut site = site.lock().unwrap();
    site.log.push(match range {
        Some((start, _)) => format!("{method} from {start}"),
        None => method.clone(),
    });
    site.senders.push(sender);
    let mut status = site.status;
    if method == "GET" && site.unavailable_next > 0 {
        site.unavailable_next -= 1;
        status = Some(503);
    }
    if let Some(status) = status {
        return connection
            .write_all(format!("HTTP/1.1 {status} Nope\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
    }

    let total = site.body.len();
    // The part of the file to send: all of it unless a part was asked for and is honoured.
    let part = match range {
        Some((start, end)) if !site.ignore_range && if_range.as_ref().is_none_or(|tag| *tag == site.etag) => {
            Some((start, end.map_or(total, |end| end + 1).min(total)))
        }
        _ => None,
    };
    let head = match part {
        Some((start, _)) if start >= total => {
            return connection
                .write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
        Some((start, end)) => format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{total}\r\nContent-Length: {}\r\n",
            end - 1,
            end - start
        ),
        None => format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\n"),
    };
    connection.write_all(
        format!("{head}ETag: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n", site.etag).as_bytes(),
    )?;
    if method != "GET" {
        return Ok(());
    }

    let (start, end) = part.unwrap_or((0, total));
    let mut payload = site.body[start..end].to_vec();
    // Damage lands on the last byte of the file, wherever the request that carries it starts.
    if end == total
        && std::mem::take(&mut site.damage_next)
        && let Some(last) = payload.last_mut()
    {
        *last ^= 0xff;
    }
    if let Some(limit) = site.cut_next_after.take() {
        payload.truncate(limit);
    }
    if let Some(position) = site.down_from {
        payload.truncate(position.saturating_sub(start));
    }
    drop(site);
    connection.write_all(&payload)
}

fn temp_dest() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("cache").join("file.patch");
    (dir, dest)
}

fn fetch(server: &Server, dest: &Path) -> Result<download::Remote, Error> {
    download::fetch(&server.url, dest, &GO, &mut |_, _| {})
}

/// A run on `world` that downloads its patch file from `server` to `dest`.
fn options(world: &World, server: &Server, dest: &Path) -> Options {
    let source = Source::Url { url: server.url.clone(), dest: dest.to_path_buf() };
    Options { source, ..world.options() }
}

/// How many of the requests the server has seen are downloads, and how many are questions.
fn gets_and_heads(server: &Server) -> (usize, usize) {
    let log = server.log();
    (log.iter().filter(|line| line.starts_with("GET")).count(), log.iter().filter(|line| *line == "HEAD").count())
}

#[test]
fn downloads_a_whole_file_and_does_not_fetch_it_twice() {
    let body = Rng::new(1).bytes(300_000);
    let server = Server::start(body.clone());
    let (_dir, dest) = temp_dest();

    fetch(&server, &dest).unwrap();
    assert!(fs::read(&dest).unwrap() == body);
    assert!(!download::part_path(&dest).exists());

    fetch(&server, &dest).unwrap();
    assert_eq!(server.log(), ["HEAD", "GET from 0", "HEAD"], "the finished file was fetched again");
}

#[test]
fn large_file_arrives_in_pieces() {
    let body = Rng::new(12).bytes(20 << 20);
    let server = Server::start(body.clone());
    let (_dir, dest) = temp_dest();

    let mut reports = Vec::new();
    download::fetch(&server.url, &dest, &GO, &mut |done, total| reports.push((done, total))).unwrap();
    assert!(fs::read(&dest).unwrap() == body);
    assert_eq!(server.log(), ["HEAD", "GET from 0", "GET from 8388608", "GET from 16777216"]);
    assert!(reports.windows(2).all(|pair| pair[0].0 <= pair[1].0), "progress went backwards");
    assert!(reports.iter().all(|&(_, total)| total == 20 << 20));
}

#[test]
fn dropped_connection_is_picked_up_where_it_left_off() {
    let body = Rng::new(2).bytes(300_000);
    let server = Server::start(body.clone());
    let (_dir, dest) = temp_dest();

    server.set(|site| site.cut_next_after = Some(120_000));
    fetch(&server, &dest).unwrap();
    assert!(fs::read(&dest).unwrap() == body);
    assert_eq!(server.log(), ["HEAD", "GET from 0", "GET from 120000"]);
}

#[test]
fn connection_that_keeps_dropping_is_survived_as_long_as_bytes_arrive() {
    let body = Rng::new(13).bytes(300_000);
    let server = Server::start(body.clone());
    let (_dir, dest) = temp_dest();

    // Every request gets 20,000 bytes further and is then cut off: far more failures than
    // the downloader has patience for, but none of them without progress.
    let site = server.site.clone();
    server.set(|site| site.down_from = Some(20_000));
    let impatient = download::Patience { attempts: 2, first_wait: Duration::ZERO };
    download::fetch_with(&server.url, &dest, impatient, &GO, &mut |done, _| {
        site.lock().unwrap().down_from = Some(done as usize / 20_000 * 20_000 + 20_000);
    })
    .unwrap();
    assert!(fs::read(&dest).unwrap() == body);
    assert!(server.log().len() > 10, "the connection was not cut often enough to test anything");
}

#[test]
fn server_having_a_bad_moment_is_asked_again() {
    let body = Rng::new(18).bytes(300_000);
    let server = Server::start(body.clone());
    let (_dir, dest) = temp_dest();

    server.set(|site| site.unavailable_next = 2);
    let patience = download::Patience { attempts: 5, first_wait: Duration::from_millis(1) };
    download::fetch_with(&server.url, &dest, patience, &GO, &mut |_, _| {}).unwrap();
    assert!(fs::read(&dest).unwrap() == body);
    assert_eq!(server.log(), ["HEAD", "GET from 0", "GET from 0", "GET from 0"]);

    // One that stays bad is given up on, with the server's answer as the reason.
    let (_dir, dest) = temp_dest();
    server.set(|site| site.unavailable_next = usize::MAX);
    let err = download::fetch_with(&server.url, &dest, patience, &GO, &mut |_, _| {}).unwrap_err();
    assert!(matches!(err, Error::Download { status: Some(503), .. }), "unexpected error: {err}");
}

#[test]
fn empty_file_is_downloaded_as_an_empty_file() {
    let server = Server::start(Vec::new());
    let (_dir, dest) = temp_dest();
    fetch(&server, &dest).unwrap();
    assert_eq!(fs::metadata(&dest).unwrap().len(), 0);
}

#[test]
fn continuing_near_the_end_from_a_server_that_cannot_send_pieces_gets_the_whole_file() {
    // Bigger than one piece, and all but the last part of it already downloaded.
    let body = Rng::new(19).bytes(9 << 20);
    let server = Server::start(body.clone());
    let (_dir, dest) = temp_dest();
    server.set(|site| site.down_from = Some(8_500_000));
    let patience = download::Patience { attempts: 1, first_wait: Duration::ZERO };
    download::fetch_with(&server.url, &dest, patience, &GO, &mut |_, _| {}).unwrap_err();
    assert_eq!(fs::metadata(download::part_path(&dest)).unwrap().len(), 8_500_000);

    server.set(|site| {
        site.down_from = None;
        site.ignore_range = true;
    });
    fetch(&server, &dest).unwrap();
    assert!(fs::read(&dest).unwrap() == body);
    // The rest was asked for, the whole file came back, so all of it was asked for plainly.
    assert_eq!(server.log()[server.log().len() - 2..], ["GET from 8500000", "GET"]);
}

/// Downloads the first 120,000 bytes and then fails, as on a line that went dead.
fn fetch_the_start(server: &Server, dest: &Path) -> Error {
    server.set(|site| site.down_from = Some(120_000));
    let patience = download::Patience { attempts: 3, first_wait: Duration::from_millis(1) };
    let err = download::fetch_with(&server.url, dest, patience, &GO, &mut |_, _| {}).unwrap_err();
    server.set(|site| site.down_from = None);
    err
}

#[test]
fn gives_up_on_a_dead_line_and_continues_on_the_next_call() {
    let body = Rng::new(14).bytes(300_000);
    let server = Server::start(body.clone());
    let (_dir, dest) = temp_dest();

    let err = fetch_the_start(&server, &dest);
    assert!(matches!(err, Error::Download { status: None, .. }), "unexpected error: {err}");
    assert!(!dest.exists(), "a cut-off download must not look finished");
    assert_eq!(fs::metadata(download::part_path(&dest)).unwrap().len(), 120_000);
    assert_eq!(server.log(), ["HEAD", "GET from 0", "GET from 120000", "GET from 120000", "GET from 120000"]);

    fetch(&server, &dest).unwrap();
    assert!(fs::read(&dest).unwrap() == body);
    assert_eq!(server.log()[5..], ["HEAD", "GET from 120000"]);
}

#[test]
fn starts_over_when_the_file_on_the_server_was_replaced() {
    let server = Server::start(Rng::new(3).bytes(300_000));
    let (_dir, dest) = temp_dest();
    fetch_the_start(&server, &dest);

    // Same size, different contents and tag: the bytes already on disk belong to the old file.
    let replacement = Rng::new(4).bytes(300_000);
    server.set(|site| {
        site.body = replacement.clone();
        site.etag = "\"v2\"".into();
    });
    fetch(&server, &dest).unwrap();
    assert!(fs::read(&dest).unwrap() == replacement, "old and new file were mixed");
    assert_eq!(server.log().last().unwrap(), "GET from 0", "the replaced file was not downloaded from the start");
}

#[test]
fn file_replaced_in_the_middle_of_a_download_is_not_mixed_with_the_old_one() {
    let server = Server::start(Rng::new(15).bytes(20 << 20));
    let (_dir, dest) = temp_dest();

    // The file changes on the server once the first piece has arrived.
    let replacement = Rng::new(16).bytes(20 << 20);
    let (site, swap) = (server.site.clone(), replacement.clone());
    let err = download::fetch(&server.url, &dest, &GO, &mut |done, _| {
        if done == 8 << 20 {
            let mut site = site.lock().unwrap();
            site.body = swap.clone();
            site.etag = "\"v2\"".into();
        }
    })
    .unwrap_err();
    assert!(matches!(err, Error::Download { status: Some(_), .. }), "unexpected error: {err}");
    assert!(!dest.exists(), "a file made of two versions was accepted");

    fetch(&server, &dest).unwrap();
    assert!(fs::read(&dest).unwrap() == replacement, "old and new file were mixed");
}

#[test]
fn replaced_file_is_noticed_even_after_a_finished_download() {
    let server = Server::start(Rng::new(5).bytes(100_000));
    let (_dir, dest) = temp_dest();
    fetch(&server, &dest).unwrap();

    let replacement = Rng::new(6).bytes(100_000);
    server.set(|site| {
        site.body = replacement.clone();
        site.etag = "\"v2\"".into();
    });
    fetch(&server, &dest).unwrap();
    assert!(fs::read(&dest).unwrap() == replacement);
}

#[test]
fn server_that_sends_the_whole_file_instead_of_the_rest_still_works() {
    let body = Rng::new(7).bytes(300_000);
    let server = Server::start(body.clone());
    let (_dir, dest) = temp_dest();
    fetch_the_start(&server, &dest);

    server.set(|site| site.ignore_range = true);
    fetch(&server, &dest).unwrap();
    assert!(fs::read(&dest).unwrap() == body, "the whole file was appended to the partial one");
}

#[test]
fn server_that_cannot_send_pieces_still_delivers_a_large_file() {
    let body = Rng::new(17).bytes(20 << 20);
    let server = Server::start(body.clone());
    let (_dir, dest) = temp_dest();

    server.set(|site| site.ignore_range = true);
    fetch(&server, &dest).unwrap();
    assert!(fs::read(&dest).unwrap() == body);
    // Asked for a piece, got the whole file, asked once more for all of it.
    assert_eq!(server.log(), ["HEAD", "GET from 0", "GET"]);
}

#[test]
fn pausing_keeps_what_was_downloaded_and_continuing_finishes() {
    let body = Rng::new(8).bytes(3_000_000);
    let server = Server::start(body.clone());
    let (_dir, dest) = temp_dest();

    let stop = AtomicBool::new(false);
    let err = download::fetch(&server.url, &dest, &stop, &mut |done, _| {
        if done >= 500_000 {
            stop.store(true, Ordering::Relaxed);
        }
    })
    .unwrap_err();
    assert!(matches!(err, Error::Stopped), "unexpected error: {err}");
    let kept = fs::metadata(download::part_path(&dest)).unwrap().len();
    assert!((500_000..3_000_000).contains(&kept), "kept {kept} bytes");

    let mut first_report = None;
    download::fetch(&server.url, &dest, &GO, &mut |done, _| {
        first_report.get_or_insert(done);
    })
    .unwrap();
    assert_eq!(first_report, Some(kept), "the download did not continue from the kept part");
    assert!(fs::read(&dest).unwrap() == body);
}

#[test]
fn file_that_is_not_on_the_server_is_reported_as_not_published() {
    let server = Server::start(Vec::new());
    let (_dir, dest) = temp_dest();
    for status in [403, 404] {
        server.set(|site| site.status = Some(status));
        assert!(matches!(fetch(&server, &dest), Err(Error::NotPublished)), "status {status}");
        assert!(matches!(download::probe(&server.url), Err(Error::NotPublished)), "status {status}");
    }
    // A server error is asked about again, and reported when it does not pass.
    server.set(|site| site.status = Some(500));
    let asked_before = server.log().len();
    let patience = download::Patience { attempts: 3, first_wait: Duration::from_millis(1) };
    let err = download::fetch_with(&server.url, &dest, patience, &GO, &mut |_, _| {}).unwrap_err();
    assert!(matches!(err, Error::Download { status: Some(500), .. }), "unexpected error: {err}");
    assert_eq!(server.log().len() - asked_before, 3, "the question was not asked three times");
    assert!(!dest.exists());
}

#[test]
fn whole_run_downloads_the_patch_and_applies_it() {
    let world = World::random(21, Layout::Table);
    let server = Server::start(fs::read(&world.patch).unwrap());
    let (_dir, dest) = temp_dest();
    let options = options(&world, &server, &dest);

    let outcome = run(&options, &GO, &mut |_| {}).unwrap();
    assert!(outcome.files_changed > 0 && !outcome.immediate, "{outcome:?}");
    world.assert_patched("downloaded patch");
    // A run asks about its patch file once, and fetches what it was told about.
    assert_eq!(server.log()[..2], ["HEAD", "GET from 0"]);
    assert_eq!(gets_and_heads(&server).1, 1, "the run asked more than once: {:?}", server.log());
}

#[test]
fn session_asks_as_often_as_it_is_asked_to_and_fetches_what_it_was_told_about() {
    let body = Rng::new(26).bytes(300_000);
    let server = Server::start(body.clone());
    let (_dir, dest) = temp_dest();
    let patience = download::Patience::default();

    let session = download::Session::new();
    let remote = session.probe(&server.url).unwrap();
    assert_eq!(remote, download::Remote { len: 300_000, etag: Some("\"v1\"".into()) });
    assert_eq!(session.probe_patiently(&server.url, patience, &GO).unwrap(), remote);
    let fetched = session.fetch(&server.url, &dest, remote.clone(), patience, &GO, &mut |_, _| {}).unwrap();
    assert_eq!(fetched, remote);
    assert!(fs::read(&dest).unwrap() == body);
    // The download did not ask once more: it was told.
    assert_eq!(server.log(), ["HEAD", "HEAD", "GET from 0"]);

    // A file that is finished is not fetched again, and a session says "not published"
    // as a single question does.
    session.fetch(&server.url, &dest, remote, patience, &GO, &mut |_, _| {}).unwrap();
    assert_eq!(server.log().len(), 3);
    server.set(|site| site.status = Some(404));
    assert!(matches!(session.probe(&server.url), Err(Error::NotPublished)));
    assert!(matches!(session.probe_patiently(&server.url, patience, &GO), Err(Error::NotPublished)));
}

#[test]
fn every_request_says_who_is_asking() {
    // The server here does not care who asks. The servers this is for are run by people
    // who may want to know, so every kind of request names this tool and its version.
    let server = Server::start(Rng::new(30).bytes(50_000));
    let (_dir, dest) = temp_dest();
    fetch(&server, &dest).unwrap();
    download::probe(&server.url).unwrap();
    download::page(&server.url, 100).unwrap();
    download::fetch_prefix(&server.url, 100).unwrap();

    assert_eq!(server.log(), ["HEAD", "GET from 0", "HEAD", "GET", "GET from 0"]);
    let senders = server.site.lock().unwrap().senders.clone();
    let expected = format!("MapleFastPatch/{}", env!("CARGO_PKG_VERSION"));
    assert_eq!(senders, vec![expected; 5]);
}

#[test]
fn file_replaced_after_it_was_asked_about_is_not_taken_for_the_one_asked_about() {
    let server = Server::start(Rng::new(28).bytes(300_000));
    let (_dir, dest) = temp_dest();
    let session = download::Session::new();
    let told = session.probe(&server.url).unwrap();

    // Same size, other contents: what the server now sends is not what was asked about.
    let replacement = Rng::new(29).bytes(300_000);
    server.set(|site| {
        site.body = replacement.clone();
        site.etag = "\"v2\"".into();
    });
    let patience = download::Patience { attempts: 2, first_wait: Duration::from_millis(1) };
    let err = session.fetch(&server.url, &dest, told, patience, &GO, &mut |_, _| {}).unwrap_err();
    assert!(matches!(err, Error::Download { status: Some(_), .. }), "unexpected error: {err}");
    assert!(!dest.exists(), "a file that was not asked about was accepted");

    // Asked about again, it is fetched as the file it is.
    fetch(&server, &dest).unwrap();
    assert!(fs::read(&dest).unwrap() == replacement);
}

#[test]
fn damaged_download_is_thrown_away_and_fetched_again() {
    let world = World::random(22, Layout::Table);
    let server = Server::start(fs::read(&world.patch).unwrap());
    let (_dir, dest) = temp_dest();
    let options = options(&world, &server, &dest);

    server.set(|site| site.damage_next = true);
    run(&options, &GO, &mut |_| {}).unwrap();
    world.assert_patched("after a damaged download");
    assert_eq!(server.log().iter().filter(|line| line.starts_with("GET")).count(), 2);
}

#[test]
fn whole_download_in_a_form_the_engine_cannot_read_is_kept_and_not_fetched_again() {
    let world = World::random(27, Layout::Table);
    // A patch file whose checksum is right, with a rebuild entry that holds an instruction
    // no patch has had so far. Fetching it again would bring the same bytes.
    let mut builder = PatchBuilder::new(Layout::Table);
    builder.create(r"Data\Fine.wz", b"an entry the engine reads");
    builder.raw(b"Data\\Strange.wz\x01");
    builder.raw(&0u32.to_le_bytes());
    builder.raw(&0xF000_0000u32.to_le_bytes());
    let server = Server::start(builder.bytes());
    let (_dir, dest) = temp_dest();
    let options = options(&world, &server, &dest);

    for attempt in 1..=2 {
        let err = run(&options, &GO, &mut |_| {}).unwrap_err();
        assert!(
            matches!(&err, Error::UnknownForm(detail) if detail.contains("unknown instruction")),
            "unexpected error: {err}"
        );
        world.assert_untouched("after a patch in an unknown form");
        assert!(dest.exists(), "a download that was whole was thrown away");
        // Downloaded once, in the first run. The second run only asks whether it still is
        // the same file.
        assert_eq!(gets_and_heads(&server), (1, attempt), "{:?}", server.log());
    }
    assert!(!world.install.join(".maple-fast-patch").exists(), "work folder left behind");
}

#[test]
fn download_that_stays_damaged_fails_without_touching_the_install() {
    let world = World::random(23, Layout::Table);
    let mut damaged = fs::read(&world.patch).unwrap();
    let last = damaged.len() - 1;
    damaged[last] ^= 0xff;
    let server = Server::start(damaged);
    let (_dir, dest) = temp_dest();
    let options = options(&world, &server, &dest);

    let err = run(&options, &GO, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::CorruptPatch(_)), "unexpected error: {err}");
    world.assert_untouched("after a download that stayed damaged");
    assert!(!dest.exists(), "the damaged file was kept for the next run");
}

#[test]
fn paused_download_inside_a_run_continues_on_the_next_run() {
    let world = World::random(24, Layout::Table);
    let patch = fs::read(&world.patch).unwrap();
    let server = Server::start(patch.clone());
    let (_dir, dest) = temp_dest();
    let options = options(&world, &server, &dest);

    // The pause comes with the first bytes of the download.
    let stop = AtomicBool::new(false);
    let mut downloading = false;
    let err = run(&options, &stop, &mut |event| match event {
        Event::Stage(stage) => downloading = stage == Stage::Download,
        Event::Progress { done, .. } if downloading && done > 0 => stop.store(true, Ordering::Relaxed),
        _ => {}
    })
    .unwrap_err();
    assert!(matches!(err, Error::Stopped), "unexpected error: {err}");
    world.assert_untouched("after pausing the download");
    let kept = fs::metadata(download::part_path(&dest)).unwrap().len();
    assert!(kept > 0, "nothing was kept of the paused download");

    run(&options, &GO, &mut |_| {}).unwrap();
    world.assert_patched("after continuing the download");
    let from_the_start = server.log().iter().filter(|line| *line == "GET from 0").count();
    assert_eq!(from_the_start, 1, "the kept part was downloaded again: {:?}", server.log());
}

#[test]
fn dropped_connection_inside_a_run_does_not_fail_the_run() {
    let world = World::random(25, Layout::Table);
    let patch = fs::read(&world.patch).unwrap();
    let server = Server::start(patch.clone());
    let (_dir, dest) = temp_dest();
    let options = options(&world, &server, &dest);

    server.set(|site| site.cut_next_after = Some(patch.len() / 2));
    run(&options, &GO, &mut |_| {}).unwrap();
    world.assert_patched("after the connection dropped once");
    assert!(server.log().iter().any(|line| line == &format!("GET from {}", patch.len() / 2)));
}

/// An address at which nothing answers.
fn dead_address() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}/file.patch", listener.local_addr().unwrap())
}

/// Runs a patch on `drive`, a made-up one that holds the install folder and nothing else.
/// The folder a patch file is downloaded to is on another drive, with all the room it wants.
fn run_on_drive(
    options: &Options,
    drive: &Drive,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<Outcome, Error> {
    let free = |dir: &Path| -> std::io::Result<u64> {
        Ok(if dir.starts_with(&drive.install) { drive.free() } else { u64::MAX / 2 })
    };
    run_on_disk(&LIVE, options, &Disk { free: &free, slack: 0, file_system: None }, stop, on_event)
}

/// Starts `options`, a run in the immediate way that downloads its patch file, on a drive
/// that is as tight as that way can bear, and pauses it right after its first file went
/// into place. Returns the drive.
fn pause_half_patched(world: &World, options: &Options, what: &str) -> Drive {
    let drive = Drive::tight(&world.install, &world.patch);
    let stop = AtomicBool::new(false);
    let paused = run_on_drive(options, &drive, &stop, &mut |_| {
        stop.store(half_patched(&world.install), Ordering::Relaxed);
    });
    assert!(matches!(paused, Err(Error::Stopped)), "{what}: {paused:?}");
    assert!(half_patched(&world.install), "{what}");
    drive
}

#[test]
fn half_patched_install_is_finished_from_the_patch_file_on_this_pc_whatever_the_server_does() {
    for what in ["the server has another file", "the server no longer lists the file", "the server is out of reach"] {
        let world = World::random(11, Layout::PerEntry);
        let server = Server::start(fs::read(&world.patch).unwrap());
        let (_dir, dest) = temp_dest();
        let mut options = Options { immediate: true, ..options(&world, &server, &dest) };
        let drive = pause_half_patched(&world, &options, what);
        let on_this_pc = fs::read(&dest).unwrap();
        let asked = server.log().len();

        match what {
            "the server has another file" => {
                let other = fs::read(World::random(12, Layout::PerEntry).patch).unwrap();
                server.set(|site| {
                    site.body = other;
                    site.etag = "\"v2\"".into();
                });
            }
            "the server no longer lists the file" => server.set(|site| site.status = Some(404)),
            _ => options.source = Source::Url { url: dead_address(), dest: dest.clone() },
        }
        // The file that began is here, so nobody is asked for it, and nothing is fetched
        // over it.
        let outcome = run_on_drive(&options, &drive, &GO, &mut |event| {
            assert!(!matches!(event, Event::Stage(Stage::Download)), "{what}: the patch file was fetched again");
        })
        .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert!(outcome.immediate && outcome.files_changed > 0, "{what}: {outcome:?}");
        world.assert_patched(what);
        assert_eq!(server.log().len(), asked, "{what}: the server was asked: {:?}", server.log());
        assert!(fs::read(&dest).unwrap() == on_this_pc, "{what}: the patch file on this PC was changed");
    }
}

#[test]
fn half_patched_install_that_lost_its_patch_file_takes_only_that_file_from_the_server() {
    let world = World::random(11, Layout::PerEntry);
    let patch = fs::read(&world.patch).unwrap();
    let server = Server::start(patch.clone());
    let (_dir, dest) = temp_dest();
    let options = Options { immediate: true, ..options(&world, &server, &dest) };
    let drive = pause_half_patched(&world, &options, "paused");
    let pinned = PatchFile::open(&dest).unwrap().id();
    download::discard(&dest);
    let half_patched_install = files_under(&world.install);
    let untouched = |what: &str| {
        assert!(files_under(&world.install) == half_patched_install, "{what}: the install folder was changed");
        assert!(!dest.exists() && !download::part_path(&dest).exists(), "{what}: something was fetched");
    };

    // A server that cannot give the file says so, and that is all that happens.
    server.set(|site| site.status = Some(404));
    let err = run_on_drive(&options, &drive, &GO, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::NotPublished), "unexpected error: {err}");
    untouched("not listed");
    let nowhere = Options { source: Source::Url { url: dead_address(), dest: dest.clone() }, ..world.options() };
    let err = run_on_drive(&nowhere, &drive, &GO, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::Download { status: None, .. }), "unexpected error: {err}");
    untouched("out of reach");

    // Another file under the same address is told from its first bytes and refused by
    // name, before any more of it is fetched.
    let other = World::random(12, Layout::PerEntry);
    let offered = PatchFile::open(&other.patch).unwrap().id();
    let other_patch = fs::read(&other.patch).unwrap();
    server.set(|site| {
        site.status = None;
        site.body = other_patch;
        site.etag = "\"v2\"".into();
    });
    let asked = server.log().len();
    let err = run_on_drive(&options, &drive, &GO, &mut |_| {}).unwrap_err();
    assert!(
        matches!(&err, Error::PinnedToOtherPatch { pinned: p, offered: o } if *p == pinned && *o == offered),
        "unexpected error: {err}"
    );
    assert_eq!(server.log()[asked..], ["HEAD", "GET from 0"]);
    untouched("another file on the server");

    // When the server has the file that began again, it is fetched and the folder finished.
    server.set(|site| {
        site.body = patch.clone();
        site.etag = "\"v3\"".into();
    });
    let outcome = run_on_drive(&options, &drive, &GO, &mut |_| {}).unwrap();
    assert!(outcome.immediate && outcome.files_changed > 0, "{outcome:?}");
    world.assert_patched("after the patch file was fetched again");
    assert!(fs::read(&dest).unwrap() == patch);
}

#[test]
fn damaged_patch_file_of_a_half_patched_install_is_replaced_only_by_the_same_file() {
    let world = World::random(11, Layout::PerEntry);
    let patch = fs::read(&world.patch).unwrap();
    let server = Server::start(patch.clone());
    let (_dir, dest) = temp_dest();
    let options = Options { immediate: true, ..options(&world, &server, &dest) };
    let drive = pause_half_patched(&world, &options, "paused");
    // The last byte of the file on this PC goes bad. By its header it is still the file
    // that began; by its checksum it is of no use.
    let mut damaged = patch.clone();
    *damaged.last_mut().unwrap() ^= 0xFF;
    fs::write(&dest, &damaged).unwrap();
    let half_patched_install = files_under(&world.install);

    // The server has another file by now: the damaged one is left where it is.
    let other = fs::read(World::random(12, Layout::PerEntry).patch).unwrap();
    server.set(|site| {
        site.body = other;
        site.etag = "\"v2\"".into();
    });
    let err = run_on_drive(&options, &drive, &GO, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::PinnedToOtherPatch { .. }), "unexpected error: {err}");
    assert!(fs::read(&dest).unwrap() == damaged, "the patch file on this PC was changed");
    assert!(files_under(&world.install) == half_patched_install, "the install folder was changed");

    // The server has the file that began: it takes the place of the damaged one.
    server.set(|site| {
        site.body = patch.clone();
        site.etag = "\"v1\"".into();
    });
    let outcome = run_on_drive(&options, &drive, &GO, &mut |_| {}).unwrap();
    assert!(outcome.immediate && outcome.files_changed > 0, "{outcome:?}");
    world.assert_patched("after the damaged patch file was fetched again");
    assert!(fs::read(&dest).unwrap() == patch);
}
