//! What Windows says about a volume and about the disk it is on.
//!
//! The disk is opened without any right of access, which is enough to ask how it is
//! attached and reads nothing from it. Its serial number is in the same answer as its
//! model; it is not taken out of the answer, and the serial number of the volume is not
//! asked for at all.

use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetDriveTypeW,
    GetLogicalDrives, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_PROPERTY_QUERY,
    StorageDeviceProperty, StorageDeviceSeekPenaltyProperty,
};

use super::last_error;
use crate::facts::{Bus, DriveKind, Fact, Unknown, Volume};
use crate::wide::{from_wide, to_wide};

/// The flag Windows sets for a volume that takes no writes (`FILE_READ_ONLY_VOLUME`).
const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;

/// The root of the volume `path` is on, as Windows writes it: `C:\`, or `\\server\share\`.
pub fn root_of(path: &Path) -> Fact<String> {
    let path = to_wide(path);
    // The root of the volume is no longer than the full form of the path it is found from.
    let mut root = vec![0u16; path.len() + 260];
    // SAFETY: `path` ends in a zero and `root` is passed with its true length.
    if unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return Err(last_error());
    }
    Ok(from_wide(&root))
}

/// The drive letter of a root with its colon, `C:`, or nothing for a root that has none.
fn letter_of(root: &str) -> String {
    let root = root.strip_prefix(r"\\?\").unwrap_or(root);
    match root.as_bytes() {
        [letter, b':', b'\\'] if letter.is_ascii_alphabetic() => format!("{}:", letter.to_ascii_uppercase() as char),
        _ => String::new(),
    }
}

/// The kind of drive `path` is on.
pub fn kind_of(path: &Path) -> DriveKind {
    let Ok(root) = root_of(path) else { return DriveKind::Other };
    let root = to_wide(&root);
    // SAFETY: the pointer is to a buffer that ends in a zero and outlives the call.
    DriveKind::from_windows(unsafe { GetDriveTypeW(root.as_ptr()) })
}

/// Bytes free on the volume with the root `root`, for the user who asks.
pub fn free(root: &str) -> Fact<u64> {
    space(root).map(|(_, free)| free)
}

/// The size of the volume with the root `root` and what is free on it, in bytes.
fn space(root: &str) -> Fact<(u64, u64)> {
    let root = to_wide(root);
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: the path ends in a zero, and the two figures asked for are written to
    // variables that outlive the call.
    if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) } == 0 {
        return Err(last_error());
    }
    Ok((total, free))
}

/// The drive letters of this PC with the kind of drive behind each, `C:` first.
pub fn letters() -> Vec<(String, DriveKind)> {
    // SAFETY: the call takes no arguments.
    let present = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|bit| present & (1 << bit) != 0)
        .map(|bit| {
            let letter = format!("{}:", (b'A' + bit) as char);
            let root = to_wide(format!("{letter}\\"));
            // SAFETY: the pointer is to a buffer that ends in a zero and outlives the call.
            (letter, DriveKind::from_windows(unsafe { GetDriveTypeW(root.as_ptr()) }))
        })
        .collect()
}

/// Looks at the volume `path` is on. `Err` only when Windows does not even say which
/// volume that is; whatever else it does not say is an error inside the answer.
pub fn look(path: &Path) -> Fact<Volume> {
    let root = root_of(path)?;
    let wide_root = to_wide(&root);
    // SAFETY: the pointer is to a buffer that ends in a zero and outlives the call.
    let kind = DriveKind::from_windows(unsafe { GetDriveTypeW(wide_root.as_ptr()) });

    // Windows documents 261 as the most either name takes.
    let (mut label, mut file_system) = ([0u16; 261], [0u16; 261]);
    let mut flags = 0u32;
    // SAFETY: the root ends in a zero, both buffers are passed with their true lengths, and
    // the pointers that are null are for answers not asked for: the serial number of the
    // volume is one of them.
    let named = unsafe {
        GetVolumeInformationW(
            wide_root.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            file_system.as_mut_ptr(),
            file_system.len() as u32,
        )
    } != 0;
    let file_system = if named { Ok(from_wide(&file_system)) } else { Err(last_error()) };
    let sizes = space(&root);

    let on_this_pc = matches!(kind, DriveKind::Fixed | DriveKind::Removable | DriveKind::RamDisk);
    let device = if on_this_pc { Device::open(&root) } else { Err(Unknown::Skipped) };
    let (bus, model) = match device.as_ref().map(Device::described) {
        Ok(Ok((bus, model))) => (Ok(bus), model),
        Ok(Err(why)) | Err(&why) => (Err(why), String::new()),
    };
    Ok(Volume {
        letter: letter_of(&root),
        kind,
        file_system,
        label: if named { from_wide(&label) } else { String::new() },
        read_only: named && flags & FILE_READ_ONLY_VOLUME != 0,
        bus,
        model,
        seek_penalty: device.as_ref().map_err(|why| *why).and_then(Device::seek_penalty),
        total: sizes.map(|(total, _)| total),
        free: sizes.map(|(_, free)| free),
        disk: device.as_ref().map_err(|why| *why).and_then(Device::number),
    })
}

/// A volume opened to be asked about the disk under it. Nothing can be read or written
/// through it.
struct Device(HANDLE);

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the handle is open, was opened by this program, and is not used again.
        unsafe { CloseHandle(self.0) };
    }
}

impl Device {
    /// Opens the volume with the root `root` without any right of access.
    fn open(root: &str) -> Fact<Device> {
        let letter = letter_of(root);
        let name = if letter.is_empty() {
            // A volume without a letter is opened by the name Windows keeps for it.
            let wide_root = to_wide(root);
            let mut name = [0u16; 64];
            // SAFETY: the root ends in a zero and `name` is passed with its true length.
            if unsafe { GetVolumeNameForVolumeMountPointW(wide_root.as_ptr(), name.as_mut_ptr(), name.len() as u32) }
                == 0
            {
                return Err(last_error());
            }
            from_wide(&name).trim_end_matches('\\').to_owned()
        } else {
            format!(r"\\.\{letter}")
        };
        let name = to_wide(name);
        // SAFETY: the name ends in a zero and outlives the call; no security attributes and
        // no template are passed.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE { Err(last_error()) } else { Ok(Device(handle)) }
    }

    /// Puts a question about the disk and returns the bytes of the answer.
    fn ask(&self, code: u32, question: Option<&STORAGE_PROPERTY_QUERY>) -> Fact<Vec<u8>> {
        // A buffer of 64-bit numbers is aligned for any answer.
        let mut answer = [0u64; 128];
        let mut returned = 0u32;
        let (input, input_len) = match question {
            Some(question) => (std::ptr::from_ref(question).cast(), size_of::<STORAGE_PROPERTY_QUERY>() as u32),
            None => (std::ptr::null(), 0),
        };
        // SAFETY: the handle is open, the question is a whole structure or nothing, and the
        // answer is written into a buffer of the length that is passed.
        let answered = unsafe {
            DeviceIoControl(
                self.0,
                code,
                input,
                input_len,
                answer.as_mut_ptr().cast(),
                size_of_val(&answer) as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        } != 0;
        if !answered {
            return Err(last_error());
        }
        let bytes: Vec<u8> = answer.iter().flat_map(|word| word.to_ne_bytes()).collect();
        Ok(bytes[..(returned as usize).min(bytes.len())].to_vec())
    }

    fn property(&self, id: i32) -> Fact<Vec<u8>> {
        let question =
            STORAGE_PROPERTY_QUERY { PropertyId: id, QueryType: PropertyStandardQuery, AdditionalParameters: [0] };
        self.ask(IOCTL_STORAGE_QUERY_PROPERTY, Some(&question))
    }

    /// How the disk is attached, and what it calls itself.
    fn described(&self) -> Fact<(Bus, String)> {
        described(&self.property(StorageDeviceProperty)?).ok_or(Unknown::NoAnswer)
    }

    /// Whether the disk has to move something to get from one place to another.
    fn seek_penalty(&self) -> Fact<bool> {
        // The answer is a version, a size, and the one byte that says it.
        self.property(StorageDeviceSeekPenaltyProperty)?.get(8).map(|byte| *byte != 0).ok_or(Unknown::NoAnswer)
    }

    /// The number Windows gives the disk.
    fn number(&self) -> Fact<u32> {
        // The answer is three numbers: the kind of device, its number, and the partition.
        number_at(&self.ask(IOCTL_STORAGE_GET_DEVICE_NUMBER, None)?, 4).ok_or(Unknown::NoAnswer)
    }
}

fn number_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Reads how a disk is attached and what it calls itself out of the answer Windows gives
/// about it (`STORAGE_DEVICE_DESCRIPTOR`).
///
/// The answer holds four places of text: maker, model, revision and serial number. Only
/// the first two are followed.
fn described(answer: &[u8]) -> Option<(Bus, String)> {
    const MAKER_AT: usize = 12;
    const MODEL_AT: usize = 16;
    const BUS_AT: usize = 28;
    let bus = Bus(number_at(answer, BUS_AT)?);
    let text_at = |place: usize| -> String {
        let start = number_at(answer, place).unwrap_or(0) as usize;
        // No place means no text, and so does a place outside the answer.
        let Some(rest) = answer.get(start..).filter(|_| start != 0) else { return String::new() };
        let end = rest.iter().position(|byte| *byte == 0).unwrap_or(rest.len());
        rest[..end].iter().filter(|byte| byte.is_ascii_graphic() || **byte == b' ').map(|byte| *byte as char).collect()
    };
    let named = format!("{} {}", text_at(MAKER_AT), text_at(MODEL_AT));
    Some((bus, named.split_whitespace().collect::<Vec<_>>().join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An answer about a disk as Windows gives it, with text at the four places.
    fn answer(bus: u32, maker: &str, model: &str, revision: &str, serial: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; 40];
        bytes[28..32].copy_from_slice(&bus.to_ne_bytes());
        for (place, text) in [(12, maker), (16, model), (20, revision), (24, serial)] {
            if text.is_empty() {
                continue;
            }
            let start = bytes.len() as u32;
            bytes[place..place + 4].copy_from_slice(&start.to_ne_bytes());
            bytes.extend(text.as_bytes());
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn disk_is_named_by_maker_and_model_and_never_by_its_serial_number() {
        let found = described(&answer(17, "", "SAMSUNG MZVL21T0HCLR-00BL2", "GXA7801Q", "S676NX0R123456")).unwrap();
        assert_eq!(found, (Bus::NVME, "SAMSUNG MZVL21T0HCLR-00BL2".to_owned()));
        // Padding with spaces, as disks do, is dropped.
        let found =
            described(&answer(11, "ATA     ", "WDC WD10EZEX-08WN4A0    ", "1A01", "     WD-WCC6Y0000000")).unwrap();
        assert_eq!(found, (Bus::SATA, "ATA WDC WD10EZEX-08WN4A0".to_owned()));
        assert!(!format!("{found:?}").contains("WCC6Y"));
    }

    #[test]
    fn answer_that_is_cut_short_or_points_outside_itself_names_nothing() {
        assert_eq!(described(&[0u8; 20]), None);
        // A place beyond the end of the answer, and a text without its ending zero.
        let mut beyond = answer(9, "", "", "", "");
        beyond[16..20].copy_from_slice(&5000u32.to_ne_bytes());
        assert_eq!(described(&beyond), Some((Bus::ISCSI, String::new())));
        let mut open_ended = answer(1, "", "", "", "");
        open_ended[16..20].copy_from_slice(&40u32.to_ne_bytes());
        open_ended.extend(b"Disk");
        assert_eq!(described(&open_ended), Some((Bus::SCSI, "Disk".to_owned())));
    }

    #[test]
    fn letters_are_read_from_roots() {
        assert_eq!(letter_of(r"C:\"), "C:");
        assert_eq!(letter_of(r"d:\"), "D:");
        assert_eq!(letter_of(r"\\?\E:\"), "E:");
        assert_eq!(letter_of(r"\\server\share\"), "");
        assert_eq!(letter_of(r"C:\mounted\"), "");
    }

    #[test]
    fn volume_the_tests_run_on_is_looked_at() {
        let dir = tempfile::tempdir().unwrap();
        let volume = look(dir.path()).expect("Windows did not say which volume the folder is on");
        assert!(volume.letter.len() == 2 && volume.letter.ends_with(':'), "{:?}", volume.letter);
        assert!(volume.file_system.as_ref().is_ok_and(|name| !name.is_empty()));
        let (total, free) = (volume.total.unwrap(), volume.free.unwrap());
        assert!(free > 0 && free <= total, "{free} of {total}");
        assert!(!volume.read_only);
        assert_eq!(kind_of(dir.path()), volume.kind);
        // The same volume is among the drive letters, with the same kind.
        assert!(letters().contains(&(volume.letter.clone(), volume.kind)));
        assert!(super::free(&format!("{}\\", volume.letter)).is_ok());
        // A disk of this PC says how it is attached without any right being asked for.
        if volume.kind == DriveKind::Fixed {
            assert!(volume.bus.is_ok(), "{:?}", volume.bus);
        }
    }

    #[test]
    fn folder_that_is_nowhere_has_no_volume() {
        assert!(
            look(Path::new(r"Q:\no\such\folder")).is_err()
                || kind_of(Path::new(r"Q:\no\such\folder")) != DriveKind::Fixed
        );
    }
}
