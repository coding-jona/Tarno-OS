//! Windows-Backend für Geräte-Enumeration und Raw-Disk-Zugriff.
//!
//! Pendant zu `devices.rs`s `/sys/block`-Scan und `flasher.rs`s
//! `O_SYNC`-Pfad unter Linux — reines Win32 (`windows-sys`, keine COM/.NET-
//! Laufzeitabhängigkeit), damit `tarno-installer` auch nativ auf dem
//! Rechner läuft, mit dem der USB-Stick tatsächlich erstellt wird (z. B.
//! Windows 11), obwohl Tarno OS selbst Linux-basiert ist. Nur unter
//! `cfg(windows)` kompiliert — siehe `devices.rs`/`flasher.rs` für die
//! Plattform-Weiche.
//!
//! Cross-kompilier-verifiziert in dieser Sandbox gegen das
//! `x86_64-pc-windows-gnu`-Target (mingw-w64) — es entsteht eine reale
//! PE32+-EXE. **Nicht** auf echter Windows-Hardware laufzeitgetestet, da
//! diese Sandbox kein Windows hat; siehe `../README.md` bzw.
//! `../docs/architecture.md` für den Status.

use std::ffi::OsStr;
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetLogicalDrives, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::{
    PropertyStandardQuery, StorageDeviceProperty, FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME,
    GET_LENGTH_INFORMATION, IOCTL_DISK_GET_LENGTH_INFO, IOCTL_STORAGE_GET_DEVICE_NUMBER,
    IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER, STORAGE_PROPERTY_QUERY,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::UI::Shell::IsUserAnAdmin;

use crate::devices::BlockDevice;

/// Windows-Äquivalent zu `libc::geteuid() == 0` — Rohschreibzugriff auf
/// ein physisches Laufwerk braucht eine erhöhte ("Als Administrator
/// ausführen") Sitzung. `IsUserAnAdmin` ist offiziell als veraltet
/// markiert, aber für ein einfaches Ja/Nein ohne Token-Handle-Verwaltung
/// weiterhin der pragmatischste Weg (dieselbe Funktion, die z. B. auch
/// 7-Zip für denselben Zweck nutzt).
pub fn is_elevated() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}

/// `\\.\PhysicalDrive0` .. `PhysicalDrive31` werden abgefragt — 32 reicht
/// für jede realistische Desktop-/Laptop-Konfiguration bei Weitem.
const MAX_PHYSICAL_DRIVES: u32 = 32;

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn open_handle(path: &str, access: u32) -> Option<HANDLE> {
    let wide = to_wide(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        None
    } else {
        Some(handle)
    }
}

/// `IOCTL_STORAGE_GET_DEVICE_NUMBER`: welches physische Laufwerk (Index)
/// hinter einem Handle steckt — egal ob das Handle auf `\\.\PhysicalDriveN`
/// selbst oder auf einen Laufwerksbuchstaben zeigt, der auf diesem
/// physischen Gerät liegt. Grundlage sowohl für den Root-Geräte-Ausschluss
/// als auch für "welche Laufwerksbuchstaben muss ich vor dem Schreiben
/// dismounten".
fn device_number(handle: HANDLE) -> Option<u32> {
    let mut info: STORAGE_DEVICE_NUMBER = unsafe { std::mem::zeroed() };
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            std::ptr::null(),
            0,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        None
    } else {
        Some(info.DeviceNumber)
    }
}

/// `IOCTL_DISK_GET_LENGTH_INFO` — die Gesamtgröße in Bytes. Pendant zu
/// `/sys/block/<dev>/size` (dort in 512-Byte-Sektoren) unter Linux.
fn drive_length(handle: HANDLE) -> Option<u64> {
    let mut info: GET_LENGTH_INFORMATION = unsafe { std::mem::zeroed() };
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_DISK_GET_LENGTH_INFO,
            std::ptr::null(),
            0,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<GET_LENGTH_INFORMATION>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        None
    } else {
        Some(info.Length as u64)
    }
}

/// `IOCTL_STORAGE_QUERY_PROPERTY` mit `StorageDeviceProperty`: liefert u.a.
/// `RemovableMedia` (Pendant zu `/sys/block/<dev>/removable`) sowie
/// Hersteller-/Modellname als Offsets in denselben Puffer. Die
/// `STORAGE_DEVICE_DESCRIPTOR`-Struktur ist laut Win32-Doku variabel lang:
/// die Vendor-/Produkt-Strings liegen direkt hinter dem festen
/// Struktur-Teil im selben Antwortpuffer, referenziert über Byte-Offsets
/// ab Pufferanfang — daher der großzügig bemessene 1-KiB-Puffer statt nur
/// `size_of::<STORAGE_DEVICE_DESCRIPTOR>()`.
fn device_descriptor(handle: HANDLE) -> Option<(bool, Option<String>, Option<String>)> {
    const BUF_LEN: usize = 1024;
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut buf = [0u8; BUF_LEN];
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &query as *const _ as *const _,
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            buf.as_mut_ptr() as *mut _,
            BUF_LEN as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    // Sicher: buf ist mindestens BUF_LEN Bytes groß, size_of::<STORAGE_DEVICE_DESCRIPTOR>() << BUF_LEN.
    let desc = unsafe { &*(buf.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR) };
    let removable = desc.RemovableMedia != 0;
    let vendor = read_cstr_at(&buf, desc.VendorIdOffset);
    let model = read_cstr_at(&buf, desc.ProductIdOffset);
    Some((removable, vendor, model))
}

/// Liest einen NUL-terminierten ASCII-String ab `offset` innerhalb von
/// `buf` (0 = "kein String vorhanden", laut Win32-Doku zu
/// `STORAGE_DEVICE_DESCRIPTOR`).
fn read_cstr_at(buf: &[u8], offset: u32) -> Option<String> {
    if offset == 0 {
        return None;
    }
    let start = offset as usize;
    if start >= buf.len() {
        return None;
    }
    let end = buf[start..]
        .iter()
        .position(|&b| b == 0)
        .map(|p| start + p)
        .unwrap_or(buf.len());
    let s = String::from_utf8_lossy(&buf[start..end]).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Listet alle physischen Laufwerke, die Windows als Wechselmedium
/// (`RemovableMedia`) meldet — Pendant zu `list_removable_devices_in`
/// unter Linux. Geräte, die sich nicht öffnen/abfragen lassen (z. B. kein
/// Medium im Kartenleser), werden übersprungen statt einen Fehler zu
/// werfen — der Installer soll trotzdem starten.
pub fn list_physical_drives() -> Vec<BlockDevice> {
    let mut devices = Vec::new();
    for n in 0..MAX_PHYSICAL_DRIVES {
        let path = format!(r"\\.\PhysicalDrive{n}");
        let Some(handle) = open_handle(&path, GENERIC_READ) else {
            continue;
        };
        let result = (|| {
            let (removable, vendor, model) = device_descriptor(handle)?;
            if !removable {
                return None;
            }
            let size_bytes = drive_length(handle)?;
            if size_bytes == 0 {
                return None;
            }
            Some(BlockDevice {
                name: format!("PhysicalDrive{n}"),
                path: PathBuf::from(&path),
                size_bytes,
                model,
                vendor,
            })
        })();
        unsafe { CloseHandle(handle) };
        if let Some(device) = result {
            devices.push(device);
        }
    }
    devices
}

/// Ermittelt den Namen (`PhysicalDriveN`) des physischen Laufwerks, auf
/// dem `%SystemDrive%` (typischerweise `C:`) liegt — zusätzliche
/// Verteidigungsebene, analog zu `root_device_name()` unter Linux, die
/// dieses Gerät aus der Auswahlliste ausschließt.
pub fn system_physical_drive_name() -> Option<String> {
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    let path = format!(r"\\.\{system_drive}");
    let handle = open_handle(&path, GENERIC_READ)?;
    let number = device_number(handle);
    unsafe { CloseHandle(handle) };
    number.map(|n| format!("PhysicalDrive{n}"))
}

/// Öffnet ein physisches Laufwerk zum Schreiben und sperrt/dismountet
/// vorher alle Laufwerksbuchstaben, die demselben physischen Gerät
/// zugeordnet sind — Windows verweigert sonst i. d. R. rohe
/// Schreibzugriffe auf ein gemountetes Laufwerk (dasselbe Vorgehen wie
/// Rufus/balenaEtcher). Best-effort: schlägt das Sperren fehl, wird trotzdem
/// versucht zu schreiben (manche Konfigurationen erlauben das auch ohne).
pub fn open_dest_handle(dest: &Path) -> Result<File, String> {
    let path = dest.to_string_lossy().to_string();
    let handle = open_handle(&path, GENERIC_READ | GENERIC_WRITE).ok_or_else(|| {
        format!(
            "Ziel {} konnte nicht geöffnet werden (Win32-Fehler {})",
            dest.display(),
            unsafe { GetLastError() }
        )
    })?;

    if let Some(target_number) = device_number(handle) {
        lock_and_dismount_volumes_for(target_number);
    }

    // SAFETY: `handle` ist ein frisches, gültiges CreateFileW-Handle mit
    // GENERIC_READ|GENERIC_WRITE, das wir hier exklusiv an `File`
    // übergeben — `File` übernimmt ab hier den Besitz (schließt es beim
    // Drop via CloseHandle).
    Ok(unsafe { File::from_raw_handle(handle as *mut _) })
}

/// Iteriert alle vergebenen Laufwerksbuchstaben (`GetLogicalDrives`-
/// Bitmaske), findet über `IOCTL_STORAGE_GET_DEVICE_NUMBER` heraus, welche
/// davon auf `target_device_number` liegen, und sperrt/dismountet sie via
/// `FSCTL_LOCK_VOLUME`/`FSCTL_DISMOUNT_VOLUME`.
///
/// Die dafür geöffneten Handles werden absichtlich **nicht** geschlossen:
/// `HANDLE` ist unter `windows-sys` ein roher Zeiger ohne Drop-Semantik,
/// ein Volume-Lock gilt nur, solange das zugehörige Handle offen bleibt,
/// und `tarno-installer` ist ein kurzlebiger One-Shot-Prozess — das
/// Betriebssystem gibt alle offenen Handles beim Prozessende ohnehin frei.
fn lock_and_dismount_volumes_for(target_device_number: u32) {
    let drives = unsafe { GetLogicalDrives() };
    for letter in b'A'..=b'Z' {
        let bit = letter - b'A';
        if drives & (1 << bit) == 0 {
            continue;
        }
        let path = format!(r"\\.\{}:", letter as char);
        let Some(handle) = open_handle(&path, GENERIC_READ | GENERIC_WRITE) else {
            continue;
        };
        if device_number(handle) != Some(target_device_number) {
            unsafe { CloseHandle(handle) };
            continue;
        }
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                handle,
                FSCTL_LOCK_VOLUME,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            );
            DeviceIoControl(
                handle,
                FSCTL_DISMOUNT_VOLUME,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            );
        }
        // Kein CloseHandle hier — siehe Dokumentation oben.
    }
}
