//! Whose a program is, by Windows' own check of its signature: asked of the launcher before
//! an app that runs with administrator rights starts it.
//!
//! An app with administrator rights hands them on to whatever it starts. Which launcher
//! that is, a folder says that is named in the settings, and anything that runs as the user
//! can write those: without a check, a file that is merely called like the launcher would
//! be given the rights. So with those rights the app starts a launcher only when Windows
//! vouches that the file is signed, that the signature is this file's, and that the signer
//! is Nexon ([`vouched`]). Without them nothing is asked: what is started then runs with
//! what the user's own rights and Windows' prompt give it, as it always did.
//!
//! The file that is checked is the file that is started. It is held open from before the
//! check until Windows has started it ([`Held`]), in a way that lets nobody write to it,
//! remove it or give it another name meanwhile, and it is started by the path it really has.

use std::path::{Path, PathBuf};

/// Nexon, as the certificate its programs are signed with names it: as the organisation,
/// and as the signer itself.
pub(crate) const NEXON: &str = "NEXON Korea Corporation";

/// What Windows' check of a file's signature came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Signature {
    /// The file is signed, the signature is this file's, and its certificates lead to an
    /// authority Windows trusts. Who signed it, as the signer's certificate says.
    Valid { organisation: String, name: String },
    /// The file carries no signature.
    Unsigned,
    /// Windows does not vouch for the file, or could not be asked: by the error it gave.
    Failed(u32),
}

/// Why a launcher is not started by an app that runs with administrator rights.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unvouched {
    NotSigned,
    /// Signed, and validly so, by somebody else.
    OtherSigner {
        organisation: String,
        name: String,
    },
    /// The signature does not hold, or the check could not be made: Windows' error.
    CheckFailed(u32),
    /// The file is not where its folder says it is: a link leads to it, or it is named in a
    /// way that Windows reads as another path.
    Elsewhere,
}

impl Unvouched {
    /// The reason, for the log.
    pub(crate) fn line(&self) -> String {
        // A name out of a certificate is anybody's to choose: it is kept short.
        let short = |name: &str| name.chars().take(64).collect::<String>();
        match self {
            Unvouched::NotSigned => "it is not signed".to_owned(),
            Unvouched::OtherSigner { organisation, name } => {
                format!("it is signed by another than Nexon ({} / {})", short(organisation), short(name))
            }
            Unvouched::CheckFailed(error) => format!("its signature could not be checked (error 0x{error:08X})"),
            Unvouched::Elsewhere => "it is not the file its folder holds under that name".to_owned(),
        }
    }
}

/// Whether a launcher may be started: by an app without administrator rights (`elevated`)
/// always, and what `signature` would say is not even asked. By an app with them only when
/// the file is validly signed by Nexon, named exactly so as the organisation and as the
/// signer. Everything else is a no, whatever kept the check from saying yes.
pub(crate) fn vouched(elevated: bool, signature: impl FnOnce() -> Signature) -> Result<(), Unvouched> {
    if !elevated {
        return Ok(());
    }
    match signature() {
        Signature::Valid { organisation, name } if organisation == NEXON && name == NEXON => Ok(()),
        Signature::Valid { organisation, name } => Err(Unvouched::OtherSigner { organisation, name }),
        Signature::Unsigned => Err(Unvouched::NotSigned),
        Signature::Failed(error) => Err(Unvouched::CheckFailed(error)),
    }
}

/// Whether the file that really is at `real` is the one `folder` holds under `name`, where
/// `folder` is where the folder really is: the file lies in that very folder, under that
/// name. A file that a link under that name leads to lies somewhere else.
pub(crate) fn in_its_folder(real: &Path, folder: &Path, name: &str) -> bool {
    let named = real.file_name().is_some_and(|file| file.eq_ignore_ascii_case(name));
    named && real.parent() == Some(folder)
}

/// `real`, a path as Windows gives it for where a file really is (`\\?\C:\Nexon\Maple`), as
/// paths are usually written (`C:\Nexon\Maple`). A program is told the path it was started
/// by as its own, and not every program takes the long form.
///
/// `None` where the two would not be the same path. Windows reads the usual form more
/// freely than the long one: it drops a dot or a space at the end of a name, and takes
/// some names for devices. A file that is named so is started by neither form.
pub(crate) fn plain(real: &Path) -> Option<PathBuf> {
    use std::path::{Component, Prefix};
    let mut parts = real.components();
    let Some(Component::Prefix(prefix)) = parts.next() else { return None };
    let Prefix::VerbatimDisk(letter) = prefix.kind() else { return None };
    let mut plain = PathBuf::from(format!("{}:\\", char::from(letter)));
    plain.extend(parts.filter(|part| matches!(part, Component::Normal(_))));
    // As Windows reads it, it is what was written, and it leads to the same place.
    let read_as_written = std::path::absolute(&plain).is_ok_and(|read| read == plain);
    (read_as_written && plain.canonicalize().is_ok_and(|leads_to| leads_to == real)).then_some(plain)
}

/// A file held open so that it stays the file it is: until this is let go, nobody writes to
/// it, removes it or renames it, and no folder above it is renamed or made to lead
/// elsewhere, since Windows renames no folder that has an open file under it.
pub(crate) struct Held {
    file: std::fs::File,
    real: PathBuf,
}

impl Held {
    /// Opens the file at `path` and holds it. Fails where the file cannot be read, and
    /// where somebody has it open for writing: that one could still change it.
    pub(crate) fn open(path: &Path) -> std::io::Result<Held> {
        let mut asked = std::fs::OpenOptions::new();
        asked.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Others may read it meanwhile, which starting it takes, and no more.
            asked.share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ);
        }
        let file = asked.open(path)?;
        let real = real_path(&file, path)?;
        Ok(Held { file, real })
    }

    /// Where the file that is held really is, as Windows says of the open file itself:
    /// whatever links the path it was opened by led through, this names none.
    pub(crate) fn real(&self) -> &Path {
        &self.real
    }

    /// What Windows says of the signature of the file that is held; see [`Signature`].
    ///
    /// Windows checks the whole of it: that the signature is this file's, that every
    /// certificate up to an authority it trusts holds, and that none of them was withdrawn.
    /// It shows nothing while it does.
    pub(crate) fn signature(&self) -> Signature {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, TRUST_E_NO_SIGNER_CERT, TRUST_E_NOSIGNATURE};
            use windows_sys::Win32::Security::WinTrust::{
                WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO, WTD_CHOICE_FILE,
                WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT, WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE,
                WTD_STATEACTION_VERIFY, WTD_UI_NONE, WinVerifyTrust,
            };

            use crate::wide::to_wide;
            let path = to_wide(&self.real);
            let mut file = WINTRUST_FILE_INFO {
                cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
                pcwszFilePath: path.as_ptr(),
                // The file that is held is the one that is read.
                hFile: self.file.as_raw_handle().cast(),
                pgKnownSubject: std::ptr::null_mut(),
            };
            let mut asked = WINTRUST_DATA {
                cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
                dwUIChoice: WTD_UI_NONE,
                fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
                dwUnionChoice: WTD_CHOICE_FILE,
                Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
                dwStateAction: WTD_STATEACTION_VERIFY,
                dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT,
                ..Default::default()
            };
            let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
            // No window is given for Windows to ask anybody in.
            let nobody = INVALID_HANDLE_VALUE;
            // SAFETY: the structures are filled in as the call reads them, and they, the
            // path and the open file outlive both calls. What the first call keeps for the
            // signer to be read from is let go by the second.
            unsafe {
                let said = WinVerifyTrust(nobody, &mut action, (&raw mut asked).cast());
                let signature = match said {
                    0 => signer(asked.hWVTStateData).unwrap_or(Signature::Failed(TRUST_E_NO_SIGNER_CERT as u32)),
                    TRUST_E_NOSIGNATURE => Signature::Unsigned,
                    error => Signature::Failed(error as u32),
                };
                asked.dwStateAction = WTD_STATEACTION_CLOSE;
                WinVerifyTrust(nobody, &mut action, (&raw mut asked).cast());
                signature
            }
        }
        #[cfg(not(windows))]
        {
            Signature::Failed(0)
        }
    }
}

/// Who signed, out of what Windows kept of a signature it has just vouched for (`state`):
/// the organisation and the name in the certificate of the one signer.
///
/// # Safety
///
/// `state` is what a call of `WinVerifyTrust` that verified has left, and is not let go yet.
#[cfg(windows)]
unsafe fn signer(state: windows_sys::Win32::Foundation::HANDLE) -> Option<Signature> {
    use windows_sys::Win32::Security::Cryptography::{
        CERT_CONTEXT, CERT_NAME_ATTR_TYPE, CertGetNameStringW, szOID_COMMON_NAME, szOID_ORGANIZATION_NAME,
    };
    use windows_sys::Win32::Security::WinTrust::{
        WTHelperGetProvCertFromChain, WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData,
    };
    // SAFETY: each call is given what the one before handed back, checked for being
    // there, and all of it lives as long as the state does.
    let certificate = unsafe {
        let provider = WTHelperProvDataFromStateData(state);
        if provider.is_null() {
            return None;
        }
        let signer = WTHelperGetProvSignerFromChain(provider, 0, 0, 0);
        if signer.is_null() {
            return None;
        }
        let certificate = WTHelperGetProvCertFromChain(signer, 0);
        if certificate.is_null() || (*certificate).pCert.is_null() {
            return None;
        }
        (*certificate).pCert
    };
    let named = |attribute: windows_sys::core::PCSTR| {
        let mut name = [0u16; 256];
        // SAFETY: the certificate is the one found above, the attribute is a constant
        // text, and the buffer is as long as the call is told.
        let length = unsafe {
            let certificate: *const CERT_CONTEXT = certificate;
            CertGetNameStringW(certificate, CERT_NAME_ATTR_TYPE, 0, attribute.cast(), name.as_mut_ptr(), 256)
        };
        // The length counts the end of the text with it.
        String::from_utf16_lossy(&name[..(length as usize).saturating_sub(1).min(name.len())])
    };
    Some(Signature::Valid { organisation: named(szOID_ORGANIZATION_NAME), name: named(szOID_COMMON_NAME) })
}

/// Where the open `file` really is, in the long form Windows gives. `opened_by` is the path
/// it was opened by, which is all that is known of it where Windows is not there to ask.
fn real_path(file: &std::fs::File, opened_by: &Path) -> std::io::Result<PathBuf> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
        let _ = opened_by;
        let mut path = vec![0u16; 512];
        loop {
            // SAFETY: the file is open, and the buffer is as long as the call is told.
            let length = unsafe {
                GetFinalPathNameByHandleW(file.as_raw_handle().cast(), path.as_mut_ptr(), path.len() as u32, 0)
            } as usize;
            if length == 0 {
                return Err(std::io::Error::last_os_error());
            }
            // A buffer that was too short is told how long one has to be.
            if length < path.len() {
                return Ok(PathBuf::from(std::ffi::OsString::from_wide(&path[..length])));
            }
            path.resize(length + 1, 0);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        opened_by.canonicalize()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::scratch::Scratch;

    fn valid(organisation: &str, name: &str) -> Signature {
        Signature::Valid { organisation: organisation.to_owned(), name: name.to_owned() }
    }

    fn other(organisation: &str, name: &str) -> Unvouched {
        Unvouched::OtherSigner { organisation: organisation.to_owned(), name: name.to_owned() }
    }

    /// With administrator rights a launcher is started only when Windows vouches that it is
    /// Nexon's. Without them nothing is asked, and nothing changes.
    #[test]
    fn with_administrator_rights_only_a_launcher_that_nexon_signed_is_started() {
        let nexon = "NEXON Korea Corporation";
        let table = [
            ("signed by Nexon", valid(nexon, nexon), Ok(())),
            // Either of the two names alone is not Nexon's certificate.
            ("the organisation alone", valid(nexon, "Somebody Else"), Err(other(nexon, "Somebody Else"))),
            ("the name alone", valid("Somebody Else", nexon), Err(other("Somebody Else", nexon))),
            ("nearly the name", valid("Nexon Korea Corporation", nexon), Err(other("Nexon Korea Corporation", nexon))),
            (
                "the name and more",
                valid("NEXON Korea Corporation Ltd.", nexon),
                Err(other("NEXON Korea Corporation Ltd.", nexon)),
            ),
            ("the name with a space after it", valid(nexon, "NEXON Korea Corporation "), {
                Err(other(nexon, "NEXON Korea Corporation "))
            }),
            ("no names in the certificate", valid("", ""), Err(other("", ""))),
            (
                "signed by Microsoft",
                valid("Microsoft Corporation", "Microsoft Windows"),
                Err(other("Microsoft Corporation", "Microsoft Windows")),
            ),
            ("not signed", Signature::Unsigned, Err(Unvouched::NotSigned)),
            // The file was changed after it was signed, its authority is not trusted, or
            // the file could not be opened: whatever it is, it is a no.
            ("a signature that is not the file's", Signature::Failed(0x8009_6010), {
                Err(Unvouched::CheckFailed(0x8009_6010))
            }),
            ("an authority Windows does not trust", Signature::Failed(0x800B_0109), {
                Err(Unvouched::CheckFailed(0x800B_0109))
            }),
            ("a check that could not be made", Signature::Failed(32), Err(Unvouched::CheckFailed(32))),
        ];
        for (what, signature, with_rights) in table {
            let asked = Cell::new(0);
            let says = || {
                asked.set(asked.get() + 1);
                signature.clone()
            };
            assert_eq!(vouched(true, says), with_rights, "{what}");
            assert_eq!(asked.get(), 1, "{what}");
            // Without the rights the file is started as it always was, and is not looked at.
            assert_eq!(vouched(false, says), Ok(()), "{what}, without the rights");
            assert_eq!(asked.get(), 1, "{what}, without the rights");
        }
    }

    #[test]
    fn the_log_says_why_a_launcher_was_not_started() {
        assert_eq!(Unvouched::NotSigned.line(), "it is not signed");
        assert_eq!(
            other("Somebody Else", "Somebody").line(),
            "it is signed by another than Nexon (Somebody Else / Somebody)"
        );
        // A name out of a certificate is not written out at any length.
        assert_eq!(
            other(&"a".repeat(300), "b").line().chars().count(),
            "it is signed by another than Nexon ( / b)".len() + 64
        );
        assert_eq!(Unvouched::CheckFailed(0x800B_0109).line(), "its signature could not be checked (error 0x800B0109)");
        assert_eq!(Unvouched::CheckFailed(32).line(), "its signature could not be checked (error 0x00000020)");
        assert_eq!(Unvouched::Elsewhere.line(), "it is not the file its folder holds under that name");
    }

    #[test]
    fn a_launcher_is_the_file_its_folder_holds_under_that_name_and_no_other() {
        let folder = Path::new(r"\\?\C:\Nexon\Maple");
        assert!(in_its_folder(Path::new(r"\\?\C:\Nexon\Maple\GameLauncher.exe"), folder, "GameLauncher.exe"));
        // Windows does not tell names apart by their case.
        assert!(in_its_folder(Path::new(r"\\?\C:\Nexon\Maple\gamelauncher.EXE"), folder, "GameLauncher.exe"));
        // What a link under that name leads to: another file, in this folder or elsewhere.
        for elsewhere in [
            r"\\?\C:\Nexon\Maple\Other.exe",
            r"\\?\C:\Nexon\Maple\Data\GameLauncher.exe",
            r"\\?\C:\Temp\GameLauncher.exe",
            r"\\?\D:\Nexon\Maple\GameLauncher.exe",
            r"\\?\UNC\nas\games\Maple\GameLauncher.exe",
            r"\\?\C:\Nexon\GameLauncher.exe",
        ] {
            assert!(!in_its_folder(Path::new(elsewhere), folder, "GameLauncher.exe"), "{elsewhere}");
        }
    }

    /// The path a launcher is started by is the one it really has, written the usual way,
    /// and only where Windows reads the usual way as that very path.
    #[cfg(windows)]
    #[test]
    fn a_program_is_started_by_the_path_it_really_has_or_not_at_all() {
        let scratch = Scratch::new("signed-plain");
        let folder = scratch.0.join("Maple Story");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("GameLauncher.exe"), b"a launcher").unwrap();
        let real = folder.join("GameLauncher.exe").canonicalize().unwrap();
        assert!(real.as_os_str().to_string_lossy().starts_with(r"\\?\"));
        let usual = plain(&real).unwrap();
        assert!(!usual.as_os_str().to_string_lossy().starts_with(r"\\"));
        assert_eq!(usual.canonicalize().unwrap(), real);
        assert_eq!(format!(r"\\?\{}", usual.display()), real.display().to_string());

        // A name that ends in a dot is another name once it is written the usual way: the
        // file under it is not this one, whatever lies there.
        let dotted = Path::new(&format!(r"{}.", real.parent().unwrap().display())).join("GameLauncher.exe");
        std::fs::create_dir_all(dotted.parent().unwrap()).unwrap();
        std::fs::write(&dotted, b"another launcher").unwrap();
        assert_eq!(plain(&dotted), None);
        std::fs::remove_dir_all(dotted.parent().unwrap()).unwrap();
        // A path that was never the long form of a drive's is none to start by, and neither
        // is one of a folder another computer shares or of a file that is not there. These
        // are turned down by how they are written: nothing is looked for under them.
        for odd in [r"Q:\Made Up\GameLauncher.exe", r"\\?\UNC\nas\games\GameLauncher.exe", r"Maple\GameLauncher.exe"] {
            assert_eq!(plain(Path::new(odd)), None, "{odd}");
        }
        assert_eq!(plain(&real.with_file_name("Missing.exe")), None);
    }

    /// While a file is held, it stays the file it is: it cannot be written to, removed or
    /// renamed, and neither can a folder above it. Reading it, which starting it takes, is
    /// left to everybody.
    #[cfg(windows)]
    #[test]
    fn a_file_that_is_held_cannot_be_changed_or_moved_until_it_is_let_go() {
        let scratch = Scratch::new("signed-held");
        let folder = scratch.0.join("game").join("inner");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("GameLauncher.exe");
        std::fs::write(&file, b"a launcher").unwrap();

        let held = Held::open(&file).unwrap();
        assert_eq!(held.real(), file.canonicalize().unwrap());
        assert!(in_its_folder(held.real(), &folder.canonicalize().unwrap(), "GameLauncher.exe"));
        assert_eq!(std::fs::read(&file).unwrap(), b"a launcher");
        assert!(std::fs::write(&file, b"another").is_err());
        assert!(std::fs::OpenOptions::new().append(true).open(&file).is_err());
        assert!(std::fs::remove_file(&file).is_err());
        assert!(std::fs::rename(&file, folder.join("Moved.exe")).is_err());
        // The folder it lies in, and the one above that.
        assert!(std::fs::rename(&folder, scratch.0.join("game").join("moved")).is_err());
        assert!(std::fs::rename(scratch.0.join("game"), scratch.0.join("moved")).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"a launcher");

        drop(held);
        std::fs::write(&file, b"another").unwrap();
        std::fs::rename(scratch.0.join("game"), scratch.0.join("moved")).unwrap();

        // A file somebody has open for writing is not held: that one could still change it.
        let file = scratch.0.join("moved").join("inner").join("GameLauncher.exe");
        let writer = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        assert!(Held::open(&file).is_err());
        drop(writer);
        assert!(Held::open(&file).is_ok());
        assert!(Held::open(&scratch.0.join("moved").join("Missing.exe")).is_err());
    }

    /// Windows' own check, asked of files that are on every PC: this test program, which
    /// nobody signed, a file that is no program, and a program of Windows itself, which is
    /// signed and not by Nexon. None of them is started by an app with administrator rights.
    #[cfg(windows)]
    #[test]
    fn windows_is_asked_and_vouches_for_no_program_that_is_not_nexons() {
        let own = Held::open(&std::env::current_exe().unwrap()).unwrap();
        assert_eq!(own.signature(), Signature::Unsigned);
        assert_eq!(vouched(true, || own.signature()), Err(Unvouched::NotSigned));

        let scratch = Scratch::new("signed-windows");
        let text = scratch.0.join("GameLauncher.exe");
        std::fs::write(&text, b"a launcher").unwrap();
        let no_program = Held::open(&text).unwrap().signature();
        assert!(matches!(no_program, Signature::Unsigned | Signature::Failed(_)), "{no_program:?}");
        assert!(vouched(true, || no_program.clone()).is_err());

        // Programs of Windows that carry their signature in the file itself.
        let windows = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let signed = ["explorer.exe", r"System32\ntoskrnl.exe", r"System32\winload.exe"]
            .iter()
            .filter_map(|program| Held::open(&windows.join(program)).ok())
            .map(|held| held.signature())
            .find(|signature| matches!(signature, Signature::Valid { .. }));
        let Some(Signature::Valid { organisation, name }) = signed.clone() else {
            panic!("no program of Windows was found to be signed");
        };
        assert_eq!(organisation, "Microsoft Corporation");
        assert!(name.starts_with("Microsoft"), "{name}");
        assert_eq!(vouched(true, || signed.unwrap()), Err(Unvouched::OtherSigner { organisation, name }));
    }
}
