//! What kind of PC and Windows this is, who runs the program, and how the PC is connected.

use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

use super::registry::{self, Root};
use super::{last_error, volume};
use crate::facts::{Adapter, DriveKind, Fact, Rights, Unknown, WebView};
use crate::privacy::Scrub;
use crate::wide::{from_wide, from_wide_ptr};

/// What must not be shown on this PC: the names of the user and of the computer, and the
/// user's own folder. They are read only to be taken out of what is shown.
pub fn hidden() -> Scrub {
    use windows_sys::Win32::System::SystemInformation::{
        ComputerNameDnsHostname, ComputerNamePhysicalDnsHostname, GetComputerNameExW,
    };
    use windows_sys::Win32::System::WindowsProgramming::GetUserNameW;

    let mut names = Vec::new();
    let mut units = [0u16; 512];
    let mut len = units.len() as u32;
    // SAFETY: `units` has room for `len` units, and the call writes how many it filled in.
    if unsafe { GetUserNameW(units.as_mut_ptr(), &mut len) } != 0 {
        names.push(from_wide(&units));
    }
    for kind in [ComputerNameDnsHostname, ComputerNamePhysicalDnsHostname] {
        let mut len = units.len() as u32;
        // SAFETY: as above.
        if unsafe { GetComputerNameExW(kind, units.as_mut_ptr(), &mut len) } != 0 {
            names.push(from_wide(&units));
        }
    }
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    names.extend(["USERNAME", "COMPUTERNAME", "USERDOMAIN"].into_iter().filter_map(env));
    // `\\NAME`, the computer that let the user in.
    names.extend(env("LOGONSERVER").map(|server| server.trim_start_matches('\\').to_owned()));

    let mut homes: Vec<String> = env("USERPROFILE").into_iter().collect();
    if let (Some(drive), Some(path)) = (env("HOMEDRIVE"), env("HOMEPATH")) {
        homes.push(format!("{drive}{path}"));
    }
    Scrub::new(names, homes)
}

/// Seconds since 1970.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// The kind of drive this program was started from.
pub fn started_from() -> Fact<DriveKind> {
    let exe = std::env::current_exe().map_err(|err| super::io_error(&err))?;
    Ok(volume::kind_of(&exe))
}

/// The Windows edition and build with the kind of processor it runs on, as the report
/// shows it: `Windows 11 Pro (26200) · 64비트`.
pub fn windows(edition_and_build: &str) -> String {
    use windows_sys::Win32::System::SystemInformation::{
        GetNativeSystemInfo, PROCESSOR_ARCHITECTURE_AMD64, PROCESSOR_ARCHITECTURE_ARM64, SYSTEM_INFO,
    };

    let mut info = SYSTEM_INFO::default();
    // SAFETY: the call fills in the structure it is given, and nothing else.
    unsafe { GetNativeSystemInfo(&mut info) };
    // SAFETY: the union's first two bytes are the processor architecture in either reading.
    let bits = match unsafe { info.Anonymous.Anonymous.wProcessorArchitecture } {
        PROCESSOR_ARCHITECTURE_AMD64 => "64비트",
        PROCESSOR_ARCHITECTURE_ARM64 => "64비트(ARM)",
        _ => "32비트",
    };
    if edition_and_build.is_empty() {
        format!("확인 불가 · {bits}")
    } else {
        format!("{edition_and_build} · {bits}")
    }
}

/// A handle that is closed when it goes out of use.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle is open, was opened by this program, and is not used again.
        unsafe { CloseHandle(self.0) };
    }
}

/// Who runs this program: an administrator or not, and with an administrator's rights or
/// without.
pub fn rights() -> Fact<Rights> {
    use windows_sys::Win32::Security::{
        CheckTokenMembership, CreateWellKnownSid, GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
        TokenElevationType, WinBuiltinAdministratorsSid,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the handle of this process needs no closing, and the token's handle is
    // written to a variable that outlives the call.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error());
    }
    let token = Owned(token);

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut kind = 0i32;
    let mut len = 0u32;
    // SAFETY: each answer is written to a variable of the size that is passed with it.
    let read = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        ) != 0
            && GetTokenInformation(
                token.0,
                TokenElevationType,
                (&raw mut kind).cast(),
                size_of::<i32>() as u32,
                &mut len,
            ) != 0
    };
    if !read {
        return Err(last_error());
    }
    let elevated = elevation.TokenIsElevated != 0;

    // The most a security identifier takes is 68 bytes.
    let mut administrators = [0u8; 68];
    let mut sid_len = administrators.len() as u32;
    let mut member = 0i32;
    // SAFETY: the identifier is written into a buffer of the length that is passed, and
    // then only read. A null token means the one this thread runs with.
    let is_member = unsafe {
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            std::ptr::null_mut(),
            administrators.as_mut_ptr().cast(),
            &mut sid_len,
        ) != 0
            && CheckTokenMembership(std::ptr::null_mut(), administrators.as_mut_ptr().cast(), &mut member) != 0
            && member != 0
    };
    // An administrator who has not been given the rights yet runs with a token Windows has
    // cut down: that is elevation type 3. The membership check then says no.
    const LIMITED: i32 = 3;
    let asks_first =
        registry::number(Root::Machine, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System", "EnableLUA")
            .map(|enabled| enabled != 0);
    Ok(Rights { administrator: elevated || is_member || kind == LIMITED, elevated, asks_first })
}

/// The WebView2 runtime, if one is installed: for the whole PC, or else for this user.
pub fn webview() -> Option<WebView> {
    // The key Microsoft documents for finding the runtime.
    const CLIENT: &str = r"Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
    let installed = |root: Root, software: &str| {
        registry::text(root, &format!(r"{software}\{CLIENT}"), "pv").filter(|version| {
            // Nothing but a version is passed on, and `0.0.0.0` stands for a runtime that was removed.
            !version.is_empty()
                && version.chars().all(|c| c.is_ascii_digit() || c == '.')
                && version.chars().any(|c| c.is_ascii_digit() && c != '0')
        })
    };
    let for_all = installed(Root::Machine32, "SOFTWARE").or_else(|| installed(Root::Machine, "SOFTWARE"));
    match for_all {
        Some(version) => Some(WebView { version, for_all: true }),
        None => installed(Root::User, "Software").map(|version| WebView { version, for_all: false }),
    }
}

/// Words in the name of a network card that mark it as no card at all.
const NOT_A_CARD: [&str; 10] =
    ["virtual", "hyper-v", "vpn", "tap-", "loopback", "bluetooth", "wintun", "wireguard", "host-only", "tunnel"];

/// The network cards that are connected: their models and speeds. Addresses are not asked
/// for, and what Windows hands over of them all the same is not looked at.
pub fn adapters() -> Fact<Vec<Adapter>> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_FRIENDLY_NAME, GAA_FLAG_SKIP_MULTICAST,
        GAA_FLAG_SKIP_UNICAST, GetAdaptersAddresses, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211,
        IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;

    const BUFFER_OVERFLOW: u32 = 111;
    let flags = GAA_FLAG_SKIP_UNICAST
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER
        | GAA_FLAG_SKIP_FRIENDLY_NAME;
    // The answer is a chain of records with pointers into the same buffer. A buffer of
    // 64-bit numbers is aligned for them.
    let mut bytes = 32 * 1024u32;
    let mut buffer: Vec<u64> = Vec::new();
    let mut status = BUFFER_OVERFLOW;
    for _ in 0..4 {
        buffer = vec![0u64; (bytes as usize).div_ceil(8)];
        // SAFETY: `buffer` has room for `bytes` bytes, and the call writes how many it needs.
        status = unsafe { GetAdaptersAddresses(0, flags, std::ptr::null(), buffer.as_mut_ptr().cast(), &mut bytes) };
        if status != BUFFER_OVERFLOW {
            break;
        }
    }
    if status != 0 {
        return Err(Unknown::Windows(status));
    }

    let mut found = Vec::new();
    let mut next = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while !next.is_null() {
        // SAFETY: the call succeeded, so the buffer holds a chain of whole records, each
        // pointing to the next one inside the same buffer or to nothing.
        let adapter = unsafe { &*next };
        next = adapter.Next;
        let wireless = adapter.IfType == IF_TYPE_IEEE80211;
        if adapter.OperStatus != IfOperStatusUp || !(wireless || adapter.IfType == IF_TYPE_ETHERNET_CSMACD) {
            continue;
        }
        // SAFETY: the description is text in the same buffer, ended by a zero.
        let model = unsafe { from_wide_ptr(adapter.Description, 256) };
        let lower = model.to_lowercase();
        if NOT_A_CARD.iter().any(|word| lower.contains(word)) {
            continue;
        }
        // A card that does not know its speed says so with every bit set.
        let speed = Some(adapter.TransmitLinkSpeed).filter(|speed| *speed != u64::MAX && *speed > 0);
        found.push(Adapter { model, wireless, speed });
    }
    // Cards with a cable first, the fastest of each kind first.
    found.sort_by_key(|adapter| (adapter.wireless, std::cmp::Reverse(adapter.speed)));
    Ok(found)
}

/// Memory that is free for programs right now, in bytes.
pub fn memory_free() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut memory = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `memory` says its own size, as the call requires of what it fills in.
    (unsafe { GlobalMemoryStatusEx(&mut memory) } != 0).then_some(memory.ullAvailPhys)
}

/// The folder Windows itself is in, which tells the system drive.
pub fn windows_dir() -> std::path::PathBuf {
    let from_env = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("windir")).map(std::path::PathBuf::from);
    from_env.filter(|dir| Path::new(dir).is_absolute()).unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_pc_answers_the_questions_put_to_it() {
        // Whoever runs the tests has rights of some kind, and an elevated program belongs
        // to an administrator.
        let rights = rights().expect("the rights of this program could not be read");
        assert!(!rights.elevated || rights.administrator, "{rights:?}");
        // The cards that are listed are real ones with a name.
        for adapter in adapters().expect("the network cards could not be listed") {
            assert!(!adapter.model.is_empty(), "{adapter:?}");
        }
        assert!(memory_free().is_some_and(|free| free > 0));
        assert!(now() > 1_700_000_000);
        assert!(windows("Windows 11 Pro (26200)").starts_with("Windows 11 Pro (26200) · "));
        assert!(windows("").starts_with("확인 불가 · "));
        // The tests run from a drive of this PC.
        assert!(matches!(started_from(), Ok(DriveKind::Fixed | DriveKind::Removable | DriveKind::RamDisk)));
        assert!(windows_dir().is_absolute());
    }

    #[test]
    fn names_of_this_pc_never_come_through_the_scrub() {
        let scrub = hidden();
        for name in ["USERNAME", "COMPUTERNAME"] {
            let Ok(value) = std::env::var(name) else { continue };
            if value.chars().count() < 3 {
                continue;
            }
            let shown = scrub.text(&format!(r"D:\Games\{value}\Maple by {value}"));
            assert!(!shown.to_lowercase().contains(&value.to_lowercase()), "{name} came through");
        }
        if let Ok(home) = std::env::var("USERPROFILE") {
            assert_eq!(scrub.text(&format!(r"{home}\Maple")), r"%USERPROFILE%\Maple");
        }
        // A webview version, where there is one, is a version and nothing else.
        if let Some(found) = webview() {
            assert!(found.version.split('.').all(|part| part.parse::<u32>().is_ok()), "{found:?}");
        }
    }
}
