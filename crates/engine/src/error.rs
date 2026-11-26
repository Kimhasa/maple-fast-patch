use std::fmt;
use std::io;
use std::path::Path;

use crate::preflight::Problem;

/// Why a patch run stopped. Each kind calls for a different next step, so they are kept apart
/// instead of being folded into one "checksum error".
#[derive(Debug)]
pub enum Error {
    /// The operating system refused a file operation. `what` names the step and file.
    Io { what: String, source: io::Error },
    /// The file is not a MapleStory patch file.
    NotAPatchFile,
    /// The patch file is damaged or in a layout this engine does not know.
    CorruptPatch(String),
    /// A run found the patch file whole, by the checksum it carries, and still could not
    /// read what is in it: it is in a form this version does not know. Fetching the file
    /// again would bring the same contents.
    UnknownForm(String),
    /// The patch file names a path outside the install folder.
    UnsafePath(String),
    /// Files in the install folder differ from what the patch was made for.
    OriginalMismatch(Vec<String>),
    /// A rebuilt file does not match the checksum the patch file promises.
    NewFileMismatch(String),
    /// A check that must pass before the install folder may be changed did not pass.
    Blocked(Vec<Problem>),
    /// The install folder is half patched and can only be finished with the patch file that
    /// began there (see [`crate::immediate`]). That file is no longer on this PC, and what
    /// the server now has under its address is another file. `pinned` and `offered` are the
    /// ids of the two, as [`crate::container::PatchFile::id`] gives them.
    PinnedToOtherPatch { pinned: String, offered: String },
    /// The download failed. `status` is the HTTP status when the server answered.
    Download { status: Option<u16>, detail: String },
    /// The server has no such patch file (yet).
    NotPublished,
    /// The run was paused or cancelled. Work done so far is kept.
    Stopped,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn io(what: impl Into<String>, source: io::Error) -> Self {
        Error::Io { what: what.into(), source }
    }

    /// A file operation the operating system refused: `action` names the step.
    ///
    /// Public for tools that read a patch file entry by entry and report what goes wrong the
    /// way the engine does.
    pub fn file(action: &str, path: &Path, source: io::Error) -> Self {
        Error::Io { what: format!("{action} {}", path.display()), source }
    }

    /// A patch file that does not hold what it should.
    ///
    /// Public for tools that read a patch file entry by entry and report what goes wrong the
    /// way the engine does.
    pub fn corrupt(detail: impl Into<String>) -> Self {
        Error::CorruptPatch(detail.into())
    }
}

/// Turns what checks found into whether a run may go on: nothing found lets it, anything
/// found stops it with [`Error::Blocked`].
pub(crate) fn blocked_unless_empty(problems: Vec<Problem>) -> Result<()> {
    if problems.is_empty() { Ok(()) } else { Err(Error::Blocked(problems)) }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { what, source } => match source.raw_os_error() {
                Some(code) => write!(f, "{what}: {source} (os error {code})"),
                None => write!(f, "{what}: {source}"),
            },
            Error::NotAPatchFile => write!(f, "not a patch file"),
            Error::CorruptPatch(detail) => write!(f, "patch file is damaged: {detail}"),
            Error::UnknownForm(detail) => write!(f, "patch file is in a form this version cannot read: {detail}"),
            Error::UnsafePath(name) => write!(f, "patch file names a path outside the install folder: {name}"),
            Error::OriginalMismatch(files) => {
                write!(f, "{} file(s) differ from the original: {}", files.len(), files.join(", "))
            }
            Error::NewFileMismatch(name) => write!(f, "rebuilt file has the wrong checksum: {name}"),
            Error::Blocked(problems) => write!(f, "blocked by {} check(s): {problems:?}", problems.len()),
            Error::PinnedToOtherPatch { pinned, offered } => write!(
                f,
                "the install folder is half patched and needs the patch file it was started with ({pinned}); \n                 the server now has another ({offered})"
            ),
            Error::Download { status: Some(status), detail } => write!(f, "download failed (HTTP {status}): {detail}"),
            Error::Download { status: None, detail } => write!(f, "download failed: {detail}"),
            Error::NotPublished => write!(f, "patch file is not published"),
            Error::Stopped => write!(f, "stopped"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
