//! The made-up server. It hands out the patch files of a made-up world the way Nexon's patch
//! server does, and what that world says about maintenance. It listens on this PC only.
//!
//! What it knows of HTTP is what the engine's downloader asks: whether a file is there and
//! how long it is (`HEAD`), a piece of it (`Range`), and whether it is still the file that
//! was asked about (`ETag`, `If-Range`). A file that is not published is answered with 403,
//! as Nexon's server answers. Every request is answered on a connection of its own.

use std::fs::{self, File, Metadata};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use crate::calendar::log_stamp;
use crate::fake_notice::{BOARD_FILE, INSPECTION_FILE, TEST_BOARD_FILE};
use crate::fake_world::SERVED_DIR;

/// The addresses that answer from a file in the world's folder, which a tester may edit
/// while the server runs: the inspection service and the two notice boards.
pub const PAGES: [(&str, &str, &str); 3] = [
    ("/inspection", INSPECTION_FILE, "text/xml; charset=utf-8"),
    ("/board", BOARD_FILE, "text/html; charset=utf-8"),
    ("/board-test", TEST_BOARD_FILE, "text/html; charset=utf-8"),
];

/// Hears one line for every request: when it came, what was asked and what was answered.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// Begins to serve the world at `dir` on `port` of this PC, where 0 takes any port that is
/// free, and returns the address. It goes on for as long as the program runs.
///
/// With a `pace`, patch files are sent no faster than that many bytes a second, on each
/// connection. A made-up patch is small and this PC is its own server, so without one a
/// download is over before anybody can watch it, pause it or cut it off.
pub fn serve(dir: PathBuf, port: u16, pace: Option<u64>, log: Log) -> io::Result<SocketAddr> {
    // Bound to this PC by number: nothing outside it can reach the server.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    let address = listener.local_addr()?;
    std::thread::spawn(move || {
        for connection in listener.incoming().flatten() {
            let (dir, log) = (dir.clone(), log.clone());
            std::thread::spawn(move || {
                // A caller that goes away in the middle is its own business.
                let _ = answer(connection, &dir, pace, &log);
            });
        }
    });
    Ok(address)
}

/// What a request asks for.
struct Request {
    method: String,
    target: String,
    /// The piece of the file that is asked for: where it starts, and its last byte if stated.
    range: Option<(u64, Option<u64>)>,
    /// The piece is only wanted of the file that carries this tag.
    if_range: Option<String>,
}

fn read_request(connection: &TcpStream) -> io::Result<Request> {
    connection.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(connection.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut words = line.split_whitespace();
    let (method, target) = (words.next().unwrap_or("").to_owned(), words.next().unwrap_or("").to_owned());
    let (mut range, mut if_range, mut sent_along) = (None, None, 0u64);
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
                    Some((start.parse().ok()?, end.parse().ok()))
                })
            }
            "if-range" => if_range = Some(value.trim().to_owned()),
            "content-length" => sent_along = value.trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    // What comes with a question, such as the envelope the inspection service is asked
    // with, is taken off the line and not looked at: the answer is the same.
    io::copy(&mut reader.take(sent_along.min(1 << 20)), &mut io::sink())?;
    Ok(Request { method, target, range, if_range })
}

fn answer(mut connection: TcpStream, dir: &Path, pace: Option<u64>, log: &Log) -> io::Result<()> {
    let request = read_request(&connection)?;
    let path = request.target.split('?').next().unwrap_or("");
    let (status, bytes) = match PAGES.iter().find(|(address, ..)| *address == path) {
        Some((_, file, kind)) => send_page(&mut connection, &request, &dir.join(file), kind)?,
        None => match published(dir, path) {
            Some(file) => send_file(&mut connection, &request, &file, pace)?,
            None => send_nothing(&mut connection, "403 Forbidden", "")?,
        },
    };
    let piece = match request.range {
        Some((start, Some(end))) => format!(" bytes {start}-{end}"),
        Some((start, None)) => format!(" bytes {start}-"),
        None => String::new(),
    };
    log(&format!("[{}] {} {}{piece} -> {status}, {bytes} bytes", log_stamp(), request.method, request.target));
    Ok(())
}

/// The file of the served tree that an address names, if it is published.
fn published(dir: &Path, path: &str) -> Option<PathBuf> {
    let relative = Path::new(path.trim_start_matches('/'));
    // Only names that lead down into the served tree, never out of it.
    let inside = relative.components().all(|part| matches!(part, Component::Normal(_)));
    let file = dir.join(SERVED_DIR).join(relative);
    (inside && file.is_file()).then_some(file)
}

/// Identifies one version of a file: its length and when it was last written.
fn tag(meta: &Metadata) -> String {
    let written = meta.modified().ok().and_then(|at| at.duration_since(UNIX_EPOCH).ok()).map_or(0, |at| at.as_nanos());
    format!("\"{:x}-{written:x}\"", meta.len())
}

/// Answers with a status and no body. Returns the status and the bytes sent.
fn send_nothing(connection: &mut TcpStream, status: &str, more: &str) -> io::Result<(u16, u64)> {
    connection
        .write_all(format!("HTTP/1.1 {status}\r\n{more}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes())?;
    Ok((status[..3].parse().unwrap_or(0), 0))
}

/// Sends a patch file or the piece of it that was asked for, at `pace` where there is one.
fn send_file(connection: &mut TcpStream, request: &Request, file: &Path, pace: Option<u64>) -> io::Result<(u16, u64)> {
    let mut opened = File::open(file)?;
    let meta = opened.metadata()?;
    let (total, tag) = (meta.len(), tag(&meta));
    let about = format!("ETag: {tag}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n");
    if request.method == "HEAD" {
        connection.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\n{about}\r\n").as_bytes())?;
        return Ok((200, 0));
    }
    // A piece is only sent of the file that was asked about. Of another one the whole
    // is sent, which is how the caller learns that the file was replaced.
    let range = request.range.filter(|_| request.if_range.as_ref().is_none_or(|wanted| *wanted == tag));
    let (status, start, end) = match range {
        Some((start, _)) if start >= total => {
            return send_nothing(
                connection,
                "416 Range Not Satisfiable",
                &format!("Content-Range: bytes */{total}\r\n"),
            );
        }
        Some((start, end)) => (206, start, end.map_or(total, |end| end + 1).min(total)),
        None => (200, 0, total),
    };
    let head = match status {
        206 => format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{total}\r\n", end - 1),
        _ => "HTTP/1.1 200 OK\r\n".to_owned(),
    };
    connection.write_all(format!("{head}Content-Length: {}\r\n{about}\r\n", end - start).as_bytes())?;
    opened.seek(SeekFrom::Start(start))?;
    let mut piece = opened.take(end - start);
    let Some(pace) = pace else { return Ok((status, io::copy(&mut piece, connection)?)) };
    // A tenth of a second's worth at a time, and then a tenth of a second's rest.
    let mut buffer = vec![0u8; (pace / 10).clamp(1, 1 << 20) as usize];
    let mut sent = 0;
    loop {
        let count = piece.read(&mut buffer)?;
        if count == 0 {
            return Ok((status, sent));
        }
        connection.write_all(&buffer[..count])?;
        sent += count as u64;
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Sends one of the files that tell of maintenance, as it is at this moment.
fn send_page(connection: &mut TcpStream, request: &Request, file: &Path, kind: &str) -> io::Result<(u16, u64)> {
    let Ok(body) = fs::read(file) else { return send_nothing(connection, "404 Not Found", "") };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    connection.write_all(head.as_bytes())?;
    if request.method == "HEAD" {
        return Ok((200, 0));
    }
    connection.write_all(&body)?;
    Ok((200, body.len() as u64))
}
