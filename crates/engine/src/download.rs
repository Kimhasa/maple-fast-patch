//! Downloads a patch file, continuing a partial download when the server still has the same
//! file.
//!
//! Next to the file being downloaded sits a small tag file that says which version of the
//! remote file the bytes on disk belong to. Bytes are only ever reused under a matching tag.

use std::cell::{Cell, RefCell};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// What the server says about a file without sending it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub len: u64,
    /// Identifies this exact version of the file. A download is only continued when the tag
    /// is unchanged; if the file was replaced, the download starts over.
    pub etag: Option<String>,
}

impl Remote {
    /// What the tag file holds for bytes downloaded from this version of `url`.
    fn tag(&self, url: &str) -> Option<String> {
        // A weak tag does not promise identical bytes, so it cannot vouch for a partial file.
        let etag = self.etag.as_deref().filter(|etag| !etag.starts_with("W/"))?;
        Some(format!("{url}\n{etag}\n{}", self.len))
    }

    /// The tag as it was saved for `dest`, minus the address line, for callers that only
    /// know the destination. Matches when the saved tag ends with this version's tag and size.
    fn tag_for_dest(&self, dest: &Path) -> Option<String> {
        let saved = saved_tag(dest)?;
        let url = saved.lines().next()?;
        self.tag(url)
    }
}

/// Whether `url` is to be fetched over HTTPS and nothing else.
fn secure(url: &str) -> bool {
    url.starts_with("https://")
}

/// What every request gives as its sender: this tool by name, with its version. The people
/// who run the servers can then tell what is asking, and nothing here looks like a browser
/// or like something that would rather not be recognised.
const USER_AGENT: &str = concat!("MapleFastPatch/", env!("CARGO_PKG_VERSION"));

/// Makes an agent. Every agent the engine sends requests with is made here.
///
/// With `https_only`, nothing is fetched over plain HTTP, redirect or not. `body_time`
/// limits how long one response body may take to arrive.
fn agent_with(tls: ureq::tls::TlsConfig, https_only: bool, body_time: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(body_time)
        .https_only(https_only)
        .tls_config(tls)
        .build()
        .into()
}

/// An agent for downloads and pages. See [`agent_with`] for what the arguments mean.
fn agent(https_only: bool, body_time: Option<Duration>) -> ureq::Agent {
    // Certificates are judged the way the rest of Windows judges them.
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    agent_with(tls, https_only, body_time)
}

fn transport(err: ureq::Error) -> Error {
    lost(err.to_string())
}

/// A request that failed without an answer from the server.
fn lost(detail: impl Into<String>) -> Error {
    Error::Download { status: None, detail: detail.into() }
}

/// How often a download that waits for the server looks whether the run was paused.
const LOOK: Duration = Duration::from_millis(50);

/// Does `work`, which waits for the server, on a thread of its own, and waits for it here,
/// where a pause is seen within a moment.
///
/// Waiting for a server cannot be broken off: a request ends when the server answers or
/// when one of its time limits runs out, and until then the thread that made it is held.
/// After a pause that thread is left to end by itself, and what it brings is dropped.
fn aside<T: Send + 'static>(stop: &AtomicBool, work: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    let (done, result) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("mfp-request".to_owned())
        .spawn(move || drop(done.send(work())))
        .map_err(|e| Error::io("start a thread for a request", e))?;
    loop {
        match result.recv_timeout(LOOK) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) if stop.load(Ordering::Relaxed) => return Err(Error::Stopped),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(lost("the request was given up")),
        }
    }
}

/// Reads the body of an answer on a thread of its own and hands on what arrives, piece by
/// piece. An empty piece is the end of the body. See [`aside`] for why: a server that stops
/// sending in the middle of a body holds whoever reads it.
fn read_aside(mut body: impl Read + Send + 'static) -> Result<mpsc::Receiver<std::io::Result<Vec<u8>>>> {
    let (arrived, arrivals) = mpsc::sync_channel(2);
    let read = move || {
        let mut buffer = vec![0u8; 1 << 18];
        loop {
            let piece = body.read(&mut buffer).map(|count| buffer[..count].to_vec());
            let last = !matches!(&piece, Ok(bytes) if !bytes.is_empty());
            // After a pause nobody waits for the rest.
            if arrived.send(piece).is_err() || last {
                break;
            }
        }
    };
    std::thread::Builder::new()
        .name("mfp-body".to_owned())
        .spawn(read)
        .map_err(|e| Error::io("start a thread for a download", e))?;
    Ok(arrivals)
}

fn header(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    Some(response.headers().get(name)?.to_str().ok()?.to_owned())
}

/// Reads the body of an answer, at most `limit` bytes of it.
fn body(response: ureq::http::Response<ureq::Body>, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::Download { status: None, detail: format!("connection lost: {e}") })?;
    Ok(bytes)
}

/// Requests that share their connections.
///
/// A request made on its own opens a connection for itself, and over HTTPS that takes
/// several exchanges before the question is even asked. A session keeps the connection of
/// one request for the next request to the same server: a run asks about its patch file and
/// then fetches it, and the app asks about several patch files in a row.
#[derive(Clone)]
pub struct Session {
    agent: ureq::Agent,
}

impl Default for Session {
    fn default() -> Session {
        Session::new()
    }
}

impl Session {
    pub fn new() -> Session {
        // Whether plain HTTP will do is said with each request, since a session is not tied
        // to one address. The time limit is that of one piece of a download.
        Session { agent: agent(false, Some(PIECE_TIME)) }
    }

    /// A request for what the server says about `url`. A file asked for over HTTPS is never
    /// fetched over plain HTTP, redirect or not.
    fn head(&self, url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        self.agent.head(url).config().https_only(secure(url)).build()
    }

    /// A request for `url` itself, under the same rule as [`Session::head`].
    fn get(&self, url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        self.agent.get(url).config().https_only(secure(url)).build()
    }

    /// Asks whether the file exists and how big it is.
    pub fn probe(&self, url: &str) -> Result<Remote> {
        let response = self.head(url).call().map_err(transport)?;
        match response.status().as_u16() {
            200 => {
                let len =
                    header(&response, "content-length").and_then(|value| value.parse().ok()).ok_or_else(|| {
                        Error::Download { status: Some(200), detail: "server did not state the file size".into() }
                    })?;
                Ok(Remote { len, etag: header(&response, "etag") })
            }
            // The server answers 403 or 404 for a patch file that has not been uploaded yet.
            403 | 404 => Err(Error::NotPublished),
            status => Err(Error::Download { status: Some(status), detail: "unexpected answer".into() }),
        }
    }

    /// [`Session::probe`], asked again with the same patience as a download when the line
    /// or the server fails. A patch run starts with this question; one lost answer should
    /// not end the run. A pause ends the asking at once, also while a server that was
    /// reached is taking its time to answer.
    pub fn probe_patiently(&self, url: &str, patience: Patience, stop: &AtomicBool) -> Result<Remote> {
        let mut failures = 0;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Error::Stopped);
            }
            let (session, address) = (self.clone(), url.to_owned());
            match aside(stop, move || session.probe(&address))? {
                Err(err @ Error::Download { status, .. }) if passing(status) => {
                    failures += 1;
                    if failures >= patience.attempts {
                        return Err(err);
                    }
                    wait(patience.pause(failures), stop)?;
                }
                other => return other,
            }
        }
    }

    /// Downloads `url` to `dest`. `remote` is what a probe said about the file just now;
    /// `on_progress` receives bytes on disk and the total.
    ///
    /// The file arrives in pieces. A piece that fails (dropped connection, silent server) is
    /// asked for again from the last byte on disk, so a flaky line slows the download down
    /// instead of ending it. If the download does end early, by pausing or after repeated
    /// failures, `dest.part` stays behind and the next call continues from there.
    pub fn fetch(
        &self,
        url: &str,
        dest: &Path,
        remote: Remote,
        patience: Patience,
        stop: &AtomicBool,
        on_progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Remote> {
        let part = part_path(dest);
        let tag = remote.tag(url);
        let same_file = tag.is_some() && saved_tag(dest) == tag;

        if same_file && fs::metadata(dest).is_ok_and(|meta| meta.len() == remote.len) {
            on_progress(remote.len, remote.len);
            return Ok(remote);
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::file("create", parent, e))?;
        }
        // Whatever is on disk under another tag belongs to another file.
        if !same_file || fs::metadata(&part).is_ok_and(|meta| meta.len() > remote.len) {
            discard(dest);
        }
        if remote.len == 0 {
            fs::write(&part, []).map_err(|e| Error::file("create", &part, e))?;
        }

        {
            let piece = Piece {
                session: self,
                url,
                remote: &remote,
                part: &part,
                dest,
                tag: tag.as_deref(),
                silence: patience.silence,
                in_one_go: Cell::new(false),
                sink: RefCell::new(Sink::new()),
            };
            let received = piece.receive_all(patience, stop, on_progress);
            // However the download ended, what arrived is made safe on disk and the file
            // is closed: the next call continues from it, or it is about to be renamed.
            let closed = piece.sink.borrow_mut().close(&part);
            received?;
            closed?;
        }

        crate::busy::when_free(|| fs::rename(&part, dest)).map_err(|e| Error::file("rename", &part, e))?;
        on_progress(remote.len, remote.len);
        Ok(remote)
    }
}

/// Asks whether the file exists and how big it is. Opens a connection for this one
/// question; several questions in a row are better asked through one [`Session`].
pub fn probe(url: &str) -> Result<Remote> {
    Session::new().probe(url)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Where an unfinished download of `dest` is kept.
pub fn part_path(dest: &Path) -> PathBuf {
    with_suffix(dest, ".part")
}

fn tag_path(dest: &Path) -> PathBuf {
    with_suffix(dest, ".tag")
}

/// Removes a downloaded file together with its partial download and tag.
pub fn discard(dest: &Path) {
    for path in [dest.to_path_buf(), part_path(dest), tag_path(dest)] {
        let _ = fs::remove_file(path);
    }
}

/// Bytes of `remote` that are already on disk and will be reused by [`fetch`].
pub fn bytes_on_disk(dest: &Path, remote: &Remote) -> u64 {
    let size = |path: &Path| fs::metadata(path).map_or(0, |meta| meta.len());
    match saved_tag(dest) {
        Some(saved) if Some(&saved) == remote.tag_for_dest(dest).as_ref() => {
            if size(dest) == remote.len {
                remote.len
            } else {
                size(&part_path(dest)).min(remote.len)
            }
        }
        _ => 0,
    }
}

fn saved_tag(dest: &Path) -> Option<String> {
    fs::read_to_string(tag_path(dest)).ok()
}

/// The file is fetched in pieces of this size, each with its own request.
const PIECE: u64 = 8 << 20;
/// A piece that has not fully arrived within this time is cut short and the rest of it asked
/// for again, so a connection that went silent cannot hang the download.
const PIECE_TIME: Duration = Duration::from_secs(60);
/// The longest a file that the server only sends whole may take to arrive. Nobody waits that
/// long for a server that went silent, see [`Patience::silence`]; this is what ends the
/// thread that is still listening to one.
const WHOLE_FILE_TIME: Duration = Duration::from_secs(24 * 60 * 60);
/// The longest wait between two attempts.
const LONGEST_WAIT: Duration = Duration::from_secs(8);

/// How long [`fetch`] keeps trying when requests fail.
#[derive(Debug, Clone, Copy)]
pub struct Patience {
    /// Failed requests in a row, with no byte gained in between, before giving up.
    pub attempts: u32,
    /// The wait before the second attempt. Each later wait is twice as long, up to 8 seconds.
    pub first_wait: Duration,
    /// How long a server may send nothing in the middle of an answer before the request
    /// counts as failed.
    pub silence: Duration,
}

impl Default for Patience {
    /// Rides out about half a minute without a connection, and a minute of a server that
    /// has stopped in the middle of an answer.
    fn default() -> Patience {
        Patience { attempts: 8, first_wait: Duration::from_millis(500), silence: PIECE_TIME }
    }
}

/// Asks the server about `url` and downloads it to `dest`, on one connection where the
/// server allows it. `on_progress` receives bytes on disk and the total. See
/// [`Session::fetch`] for how a download survives a bad line and continues after a pause.
pub fn fetch(url: &str, dest: &Path, stop: &AtomicBool, on_progress: &mut dyn FnMut(u64, u64)) -> Result<Remote> {
    fetch_with(url, dest, Patience::default(), stop, on_progress)
}

/// [`fetch`] with a chosen [`Patience`]. Public for tests, which cannot wait as long as a run does.
#[doc(hidden)]
pub fn fetch_with(
    url: &str,
    dest: &Path,
    patience: Patience,
    stop: &AtomicBool,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Remote> {
    let session = Session::new();
    let remote = session.probe_patiently(url, patience, stop)?;
    session.fetch(url, dest, remote, patience, stop, on_progress)
}

/// Whether a failed request is worth making again: the line failed (no status), or the
/// server answered that it is having a bad moment (5xx, "too many requests").
fn passing(status: Option<u16>) -> bool {
    status.is_none_or(|code| code >= 500 || code == 429)
}

impl Patience {
    /// The wait after `failures` failed requests in a row.
    fn pause(&self, failures: u32) -> Duration {
        self.first_wait.saturating_mul(1 << failures.saturating_sub(1).min(8)).min(LONGEST_WAIT)
    }
}

/// [`probe`], asked again with the same patience as a download when the line or the server
/// fails.
pub fn probe_patiently(url: &str, patience: Patience, stop: &AtomicBool) -> Result<Remote> {
    Session::new().probe_patiently(url, patience, stop)
}

/// Sleeps, waking early when the run is paused.
fn wait(time: Duration, stop: &AtomicBool) -> Result<()> {
    let step = Duration::from_millis(50);
    let mut left = time;
    while !left.is_zero() {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        std::thread::sleep(step.min(left));
        left = left.saturating_sub(step);
    }
    Ok(())
}

/// How long bytes of a download may wait in the file cache before they are made safe on
/// disk. A power cut can cost this much of a download, and what it costs is noticed: a
/// partial file that lost bytes makes a finished file whose checksum is wrong.
const SYNC_EVERY: Duration = Duration::from_secs(5);

/// The partial file as a download has it open. It stays open from one piece to the next and
/// is synced every few seconds, not after every piece: on a fast line that was several
/// syncs a second, each with the file closed and opened again around it.
struct Sink {
    file: Option<File>,
    /// When what was written was last made safe on disk.
    synced: Instant,
}

impl Sink {
    fn new() -> Sink {
        Sink { file: None, synced: Instant::now() }
    }

    /// The partial file, placed to take the bytes that follow its first `start`. Starting
    /// at zero empties it: the download begins again.
    fn at(&mut self, part: &Path, start: u64) -> Result<&mut File> {
        let failed = |e| Error::file("create", part, e);
        let mut file = match self.file.take() {
            Some(file) => file,
            None => OpenOptions::new().create(true).write(true).truncate(false).open(part).map_err(failed)?,
        };
        if start == 0 {
            file.set_len(0).map_err(failed)?;
        }
        file.seek(SeekFrom::End(0)).map_err(failed)?;
        Ok(self.file.insert(file))
    }

    /// Makes what was written safe on disk, unless that was done a moment ago.
    fn sync_if_due(&mut self, part: &Path) -> Result<()> {
        if self.synced.elapsed() < SYNC_EVERY { Ok(()) } else { self.sync(part) }
    }

    fn sync(&mut self, part: &Path) -> Result<()> {
        if let Some(file) = &self.file {
            file.sync_all().map_err(|e| Error::file("write", part, e))?;
        }
        self.synced = Instant::now();
        Ok(())
    }

    /// Makes what was written safe on disk and closes the file, which lets it be renamed
    /// or removed.
    fn close(&mut self, part: &Path) -> Result<()> {
        let synced = self.sync(part);
        self.file = None;
        synced
    }
}

/// What every request of one download has in common.
struct Piece<'a> {
    session: &'a Session,
    url: &'a str,
    remote: &'a Remote,
    part: &'a Path,
    dest: &'a Path,
    tag: Option<&'a str>,
    /// See [`Patience::silence`].
    silence: Duration,
    /// Set once the server turns out to ignore requests for a piece of the file.
    in_one_go: Cell<bool>,
    sink: RefCell<Sink>,
}

impl Piece<'_> {
    /// Requests piece after piece until the partial file holds the whole file.
    fn receive_all(&self, patience: Patience, stop: &AtomicBool, on_progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
        let on_disk = || fs::metadata(self.part).map_or(0, |meta| meta.len());
        let mut done = on_disk();
        let mut failures = 0;
        while done < self.remote.len {
            if stop.load(Ordering::Relaxed) {
                return Err(Error::Stopped);
            }
            let outcome = self.receive(done, stop, on_progress);
            let gained = on_disk() > done;
            match outcome {
                Ok(()) => failures = 0,
                // The server would not continue from where the partial file ends; start
                // over. The file has to be closed before it can be removed.
                Err(Error::Download { status: Some(416), .. }) if done > 0 => {
                    self.sink.borrow_mut().close(self.part)?;
                    discard(self.dest);
                }
                // The line failed, or the server is having a bad moment (5xx, "too many
                // requests"): neither says anything against the file. Bytes that did arrive
                // are kept, and an attempt that gained some is not held against the line.
                Err(err @ Error::Download { status, .. }) if passing(status) => {
                    failures = if gained { 0 } else { failures + 1 };
                    if failures >= patience.attempts {
                        return Err(err);
                    }
                    // Even a line that keeps delivering a little is given a breath between
                    // requests, so a broken one is not hammered.
                    wait(patience.pause(failures), stop)?;
                }
                Err(err) => return Err(err),
            }
            done = on_disk();
        }
        Ok(())
    }

    /// Requests the piece of the file that starts at `start` and appends what arrives to the
    /// partial file.
    fn receive(&self, start: u64, stop: &AtomicBool, on_progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
        let (remote, part) = (self.remote, self.part);
        let end = (start + PIECE).min(remote.len) - 1;
        let mut request = self.session.get(self.url);
        if self.in_one_go.get() {
            // The whole file takes as long as it takes, for as long as bytes keep coming.
            request = request.config().timeout_recv_body(Some(WHOLE_FILE_TIME)).build();
        } else {
            request = request.header("Range", format!("bytes={start}-{end}"));
            if let Some(etag) = &remote.etag {
                // Makes the server send the whole file instead if it was replaced meanwhile.
                request = request.header("If-Range", etag);
            }
        }
        let response = aside(stop, move || request.call())?.map_err(transport)?;
        let status = response.status().as_u16();
        let unexpected = |detail: &str| Error::Download { status: Some(status), detail: detail.to_owned() };

        let asked_for_all = (start, end + 1) == (0, remote.len);
        let (start, expected) = match status {
            206 if !self.in_one_go.get() => {
                let range = format!("bytes {start}-{end}/{}", remote.len);
                if header(&response, "content-range").as_deref() != Some(range.as_str()) {
                    return Err(unexpected("the server sent a different part of the file than asked for"));
                }
                (start, end + 1)
            }
            200 => (0, remote.len),
            _ => return Err(unexpected("unexpected answer")),
        };
        // What the server is about to send has to be the file that was probed.
        if header(&response, "etag") != remote.etag {
            return Err(unexpected("the file changed on the server"));
        }
        // The server sent the whole file where a piece was asked for, so it does not do
        // pieces. The whole file may need longer than a piece is allowed; ask again for all
        // of it, without the time limit.
        if status == 200 && !self.in_one_go.get() && !asked_for_all {
            self.in_one_go.set(true);
            drop(response);
            return self.receive(0, stop, on_progress);
        }

        let mut sink = self.sink.borrow_mut();
        let file = sink.at(part, start)?;
        // The tag is written once the partial file really holds bytes of this version.
        if start == 0 {
            let tag_path = tag_path(self.dest);
            match self.tag {
                Some(tag) => fs::write(&tag_path, tag).map_err(|e| Error::file("write", &tag_path, e))?,
                None => drop(fs::remove_file(&tag_path)),
            }
        }

        let arrivals = read_aside(response.into_body().into_reader())?;
        let mut done = start;
        let mut heard = Instant::now();
        on_progress(done, remote.len);
        let outcome = loop {
            if stop.load(Ordering::Relaxed) {
                break Err(Error::Stopped);
            }
            let bytes = match arrivals.recv_timeout(LOOK) {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(e)) => break Err(lost(format!("connection lost: {e}"))),
                Err(mpsc::RecvTimeoutError::Timeout) if heard.elapsed() < self.silence => continue,
                // The server keeps the connection and says nothing more. Waiting on is
                // waiting for nothing; the next request starts from what did arrive.
                Err(mpsc::RecvTimeoutError::Timeout) => break Err(lost("the server went silent")),
                Err(mpsc::RecvTimeoutError::Disconnected) => break Err(lost("connection lost")),
            };
            heard = Instant::now();
            // The answer is read to its very end, not just for the bytes asked for: only a
            // connection whose answer was read out is kept for the next piece.
            if bytes.is_empty() {
                break Ok(());
            }
            if done + bytes.len() as u64 > expected {
                break Err(unexpected("the server sent more than it announced"));
            }
            file.write_all(&bytes).map_err(|e| Error::file("write", part, e))?;
            done += bytes.len() as u64;
            on_progress(done, remote.len);
        };
        sink.sync_if_due(part)?;
        outcome?;
        if done != expected {
            return Err(Error::Download {
                status: None,
                detail: format!("connection closed after {done} of {} bytes", remote.len),
            });
        }
        Ok(())
    }
}

/// How long the answer to a small read may take to arrive.
const SMALL_READ_TIME: Duration = Duration::from_secs(30);

/// Asks for a small page. The answer comes back whatever its status, with its body unread.
fn ask_for_page(url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    agent(secure(url), Some(SMALL_READ_TIME)).get(url).call().map_err(transport)
}

/// Turns down an answer to a small read that is anything but "200 OK".
fn require_ok(response: &ureq::http::Response<ureq::Body>) -> Result<()> {
    match response.status().as_u16() {
        200 => Ok(()),
        status => Err(Error::Download { status: Some(status), detail: "unexpected answer".into() }),
    }
}

/// The body of an answer as text, at most `limit` bytes of it.
fn text(response: ureq::http::Response<ureq::Body>, limit: u64) -> Result<String> {
    Ok(String::from_utf8_lossy(&body(response, limit)?).into_owned())
}

/// Fetches a small page as text, at most `limit` bytes of it.
pub fn page(url: &str, limit: u64) -> Result<String> {
    let response = ask_for_page(url)?;
    require_ok(&response)?;
    text(response, limit)
}

/// What a site answered a small read with, whatever the answer was: its status, the path
/// of the address it came from once redirects were followed (without what follows a `?`),
/// at most `limit` bytes of its text, and the time the server stated. For whoever has to
/// tell an answer that is not the page it asked for from no answer at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub path: String,
    pub text: String,
    pub date: Option<String>,
}

/// Reads a small page whatever status it is answered with. Fails only where no answer came.
pub fn page_answer(url: &str, limit: u64) -> Result<Answer> {
    use ureq::ResponseExt;
    let response = ask_for_page(url)?;
    let (status, path) = (response.status().as_u16(), response.get_uri().path().to_owned());
    let date = header(&response, "date");
    Ok(Answer { status, path, text: text(response, limit)?, date })
}

/// An agent that leaves TLS to Windows itself. Some of Nexon's older servers only offer
/// ciphers that the agent for downloads does not speak; Windows still does.
fn system_agent(url: &str) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    agent_with(tls, secure(url), Some(SMALL_READ_TIME))
}

/// What a small read brought: its text, and what the server said the time was as it
/// answered, which is its `Date` header as it was sent. Whoever goes by the clock can hold
/// its own against that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dated {
    pub text: String,
    pub date: Option<String>,
}

/// Asks a SOAP service one question and returns its answer as text, at most `limit` bytes,
/// with the time the server said it was.
pub fn soap(url: &str, action: &str, envelope: &str, limit: u64) -> Result<Dated> {
    let response = system_agent(url)
        .post(url)
        .header("Content-Type", "text/xml; charset=utf-8")
        .header("SOAPAction", format!("\"{action}\""))
        .send(envelope)
        .map_err(transport)?;
    require_ok(&response)?;
    let date = header(&response, "date");
    Ok(Dated { text: text(response, limit)?, date })
}

/// Downloads only the first `len` bytes of a file.
pub fn fetch_prefix(url: &str, len: u64) -> Result<Vec<u8>> {
    let response = agent(secure(url), Some(PIECE_TIME))
        .get(url)
        .header("Range", format!("bytes=0-{}", len.saturating_sub(1)))
        .call()
        .map_err(transport)?;
    match response.status().as_u16() {
        200 | 206 => {}
        403 | 404 => return Err(Error::NotPublished),
        status => return Err(Error::Download { status: Some(status), detail: "unexpected answer".into() }),
    }
    body(response, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_stays_open_from_piece_to_piece_and_its_size_shows_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("file.patch.part");
        let on_disk = || fs::metadata(&part).unwrap().len();
        let mut sink = Sink::new();

        // Two pieces through one open file. What was written counts at once, without a
        // sync or a close: the download tells from the size how far it has come.
        sink.at(&part, 0).unwrap().write_all(b"first ").unwrap();
        assert_eq!(on_disk(), 6);
        sink.at(&part, 6).unwrap().write_all(b"second").unwrap();
        assert_eq!(on_disk(), 12);
        // Synced a moment ago, which here is when the sink was made: nothing is due.
        let synced = sink.synced;
        sink.sync_if_due(&part).unwrap();
        assert_eq!(sink.synced, synced);
        sink.close(&part).unwrap();
        assert!(sink.file.is_none());
        assert_eq!(fs::read(&part).unwrap(), b"first second");

        // A later call goes on behind what is there, and a download that starts over
        // empties the file, also while it is open.
        sink.at(&part, 12).unwrap().write_all(b", third").unwrap();
        assert_eq!(on_disk(), 19);
        sink.at(&part, 0).unwrap().write_all(b"again").unwrap();
        sink.close(&part).unwrap();
        assert_eq!(fs::read(&part).unwrap(), b"again");
        // Closed, the file can be renamed, as a finished download is.
        fs::rename(&part, dir.path().join("file.patch")).unwrap();
        sink.close(&part).unwrap();
    }

    #[test]
    fn every_agent_names_this_tool() {
        // The agent for downloads and pages, the one a session keeps, and the one for the
        // service that only Windows' own TLS can talk to.
        assert_eq!(USER_AGENT, format!("MapleFastPatch/{}", env!("CARGO_PKG_VERSION")));
        for agent in [agent(true, None), Session::new().agent, system_agent("https://example.invalid/service")] {
            let sender = format!("{:?}", agent.config().user_agent());
            assert!(sender.contains(USER_AGENT), "{sender}");
        }
    }
}
