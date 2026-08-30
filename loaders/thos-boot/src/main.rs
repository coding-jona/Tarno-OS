// SPDX-License-Identifier: GPL-2.0-or-later
//! THOS boot picker — a standalone UEFI application.
//!
//! Runs before any kernel. It finds the OS loaders on *every* disk the firmware
//! can see — from the `Boot####` NVRAM entries and by probing each EFI System
//! Partition for well-known loader paths — draws a menu, counts down to a
//! default, and `LoadImage`/`StartImage`s the chosen one. Picking "Windows"
//! chainloads `bootmgfw.efi`; picking "THOS" chainloads our own kernel loader.
//! No `BootOrder` rewriting, no virtualization — this is what rEFInd and the
//! systemd-boot menu do.
//!
//! Config (optional): `\EFI\thos\boot.conf` on the ESP we launched from —
//! `timeout=<seconds>` and `default=<index>` or `default=<substring>`.

#![no_std]
#![no_main]

extern crate alloc;

use thos_boot as _; // link the wide-string libc shims (see lib.rs)

use core::time::Duration;

use alloc::borrow::ToOwned;
use alloc::string::ToString;
use alloc::vec::Vec;

use uefi::boot::{self, LoadImageSource, ScopedProtocol, SearchType};
use uefi::proto::device_path::build::{media::FilePath, DevicePathBuilder};
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::file::{Directory, File, FileAttribute, FileMode};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::proto::BootPolicy;
use uefi::runtime::VariableVendor;
use uefi::{cstr16, CStr16, CString16, Char16, Status};

/// Well-known loader paths to probe on every volume, with the label to show.
/// `\EFI\BOOT\BOOTX64.EFI` is deliberately absent — that is this picker.
const KNOWN: &[(&CStr16, &str)] = &[
    (cstr16!("\\EFI\\Microsoft\\Boot\\bootmgfw.efi"), "Windows Boot Manager"),
    (cstr16!("\\EFI\\thos\\BOOTX64.EFI"), "THOS"),
    (cstr16!("\\EFI\\limine\\BOOTX64.EFI"), "THOS"),
    (cstr16!("\\EFI\\systemd\\systemd-bootx64.efi"), "systemd-boot"),
    (cstr16!("\\EFI\\debian\\shimx64.efi"), "Debian"),
    (cstr16!("\\EFI\\debian\\grubx64.efi"), "Debian (GRUB)"),
    (cstr16!("\\EFI\\devuan\\grubx64.efi"), "Devuan (GRUB)"),
    (cstr16!("\\EFI\\ubuntu\\shimx64.efi"), "Ubuntu"),
    (cstr16!("\\EFI\\ubuntu\\grubx64.efi"), "Ubuntu (GRUB)"),
    (cstr16!("\\EFI\\fedora\\shimx64.efi"), "Fedora"),
    (cstr16!("\\EFI\\grub\\grubx64.efi"), "GRUB"),
];

struct Entry {
    label: CString16,
    /// Full device path to the loader, as raw bytes (owned).
    dp: Vec<u8>,
    from_nvram: bool,
}

#[uefi::entry]
fn main() -> Status {
    uefi::helpers::init().expect("helpers::init");
    let _ = uefi::system::with_stdout(|o| o.clear());

    let entries = enumerate();
    if entries.is_empty() {
        uefi::println!("thos-boot: no OS loaders found on any disk.");
        boot::stall(Duration::from_secs(10));
        return Status::NOT_FOUND;
    }

    let (timeout, default) = read_config();
    let mut sel = resolve_default(&entries, &default);

    loop {
        match menu(&entries, timeout, sel) {
            Some(i) => {
                sel = i;
                uefi::println!("\r\nthos-boot: starting `{}` ...", entries[i].label);
                match chainload(&entries[i]) {
                    Ok(()) => uefi::println!("thos-boot: `{}` returned; back to the menu.", entries[i].label),
                    Err(e) => {
                        uefi::println!("thos-boot: failed to start `{}`: {:?}", entries[i].label, e);
                        boot::stall(Duration::from_secs(3));
                    }
                }
            }
            None => return Status::SUCCESS, // (unreachable: menu always returns a pick)
        }
    }
}

// --- enumeration -----------------------------------------------------------

fn enumerate() -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let self_dp = self_loader_dp();

    collect_nvram(&mut out, self_dp.as_deref());
    collect_probe(&mut out);
    out
}

/// The device path this picker was itself loaded from — so we never offer to
/// boot ourselves.
fn self_loader_dp() -> Option<Vec<u8>> {
    let li = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()).ok()?;
    let dp = li.file_path()?;
    Some(dp.as_bytes().to_vec())
}

fn label_present(out: &[Entry], label: &CStr16) -> bool {
    out.iter().any(|e| &*e.label == label)
}

/// Parse `BootOrder` + each `Boot####` EFI_LOAD_OPTION. The description field is
/// the human label; the device path is exactly what the firmware would boot.
fn collect_nvram(out: &mut Vec<Entry>, self_dp: Option<&[u8]>) {
    let mut order_buf = [0u8; 512];
    let Ok((order, _)) =
        uefi::runtime::get_variable(cstr16!("BootOrder"), &VariableVendor::GLOBAL_VARIABLE, &mut order_buf)
    else {
        return;
    };

    let mut var_buf = [0u8; 2048];
    for chunk in order.chunks_exact(2) {
        let id = u16::from_le_bytes([chunk[0], chunk[1]]);
        let name = boot_var_name(id);
        let Ok((data, _)) =
            uefi::runtime::get_variable(&name, &VariableVendor::GLOBAL_VARIABLE, &mut var_buf)
        else {
            continue;
        };
        if data.len() < 6 {
            continue;
        }
        // UINT32 Attributes, UINT16 FilePathListLength, CHAR16 Description[],
        // EFI_DEVICE_PATH FilePathList[FilePathListLength], UINT8 Optional[].
        let attrs = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        const LOAD_OPTION_ACTIVE: u32 = 0x0000_0001;
        if attrs & LOAD_OPTION_ACTIVE == 0 {
            continue;
        }
        let fpl_len = u16::from_le_bytes([data[4], data[5]]) as usize;

        // Description: CHAR16s from offset 6 up to (and past) a 0x0000.
        let mut end = 6;
        while end + 1 < data.len() && !(data[end] == 0 && data[end + 1] == 0) {
            end += 2;
        }
        let desc = decode_utf16(&data[6..end]);
        end += 2; // skip the NUL

        let Some(dp) = data.get(end..end + fpl_len) else { continue };
        if Some(dp) == self_dp || label_present(out, &desc) {
            continue;
        }
        if !real_os_entry(dp, &desc) {
            continue;
        }
        out.push(Entry { label: desc, dp: dp.to_vec(), from_nvram: true });
    }
}

/// Keep a `Boot####` entry only if it looks like an on-disk OS loader: its
/// device path must reach a hard-drive partition and end in a `.efi` file, and
/// it must not be one of the firmware's own apps (setup UI, shell, our picker).
fn real_os_entry(dp_bytes: &[u8], desc: &CStr16) -> bool {
    use uefi::proto::device_path::{DeviceSubType, DeviceType};

    let d = desc.to_string();
    if d.is_empty() || d.starts_with("UEFI ") || d.contains("THOS Boot") {
        return false;
    }
    for fw in ["UiApp", "Shell", "Boot Manager", "BootManager", "EnrollDefaultKeys", "Setup"] {
        if d.contains(fw) {
            return false;
        }
    }

    let Ok(dp) = <&DevicePath>::try_from(dp_bytes) else { return false };
    let mut on_disk = false;
    let mut has_efi = false;
    for node in dp.node_iter() {
        match (node.device_type(), node.sub_type()) {
            (DeviceType::MEDIA, DeviceSubType::MEDIA_HARD_DRIVE) => on_disk = true,
            (DeviceType::MEDIA, DeviceSubType::MEDIA_PIWG_FIRMWARE_FILE)
            | (DeviceType::MEDIA, DeviceSubType::MEDIA_PIWG_FIRMWARE_VOLUME) => return false,
            (DeviceType::MEDIA, DeviceSubType::MEDIA_FILE_PATH) => {
                if node.data().windows(4).any(|w| w == [b'.', 0, b'e', 0]) {
                    has_efi = true; // "...efi" as UCS-2
                }
            }
            _ => {}
        }
    }
    on_disk && has_efi
}

/// Probe every filesystem the firmware exposes for the `KNOWN` loader paths.
fn collect_probe(out: &mut Vec<Entry>) {
    let Ok(handles) = boot::locate_handle_buffer(SearchType::from_proto::<SimpleFileSystem>()) else {
        return;
    };

    for &handle in handles.iter() {
        let Ok(vol_dp) = boot::open_protocol_exclusive::<DevicePath>(handle) else { continue };
        let Ok(mut fs) = boot::open_protocol_exclusive::<SimpleFileSystem>(handle) else { continue };
        let Ok(mut root) = fs.open_volume() else { continue };

        for &(path, label) in KNOWN {
            let label = CString16::try_from(label).unwrap();
            if label_present(out, &label) || !file_exists(&mut root, path) {
                continue;
            }
            if let Some(dp) = append_file_path(&vol_dp, path) {
                out.push(Entry { label, dp, from_nvram: false });
            }
        }
    }
}

fn file_exists(root: &mut Directory, path: &CStr16) -> bool {
    match root.open(path, FileMode::Read, FileAttribute::empty()) {
        Ok(h) => {
            h.close();
            true
        }
        Err(_) => false,
    }
}

/// `<volume device path>` + a `MEDIA/FilePath(path)` node, as owned bytes.
fn append_file_path(vol: &DevicePath, path: &CStr16) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut b = DevicePathBuilder::with_vec(&mut buf);
    for node in vol.node_iter() {
        // Stop at the volume's END node; we re-terminate after our FilePath.
        b = b.push(&node).ok()?;
    }
    b = b.push(&FilePath { path_name: path }).ok()?;
    let dp = b.finalize().ok()?;
    Some(dp.as_bytes().to_vec())
}

// --- config --------------------------------------------------------------

enum Default {
    Index(usize),
    Match(CString16),
    First,
}

fn read_config() -> (u64, Default) {
    let default_timeout = 5u64;
    let Some(text) = read_self_file(cstr16!("\\EFI\\thos\\boot.conf")) else {
        return (default_timeout, Default::First);
    };

    let mut timeout = default_timeout;
    let mut def = Default::First;
    for line in text.split(['\n', '\r']) {
        let line = line.trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "timeout" => {
                if let Ok(n) = v.parse::<u64>() {
                    timeout = n;
                }
            }
            "default" => {
                def = match v.parse::<usize>() {
                    Ok(i) => Default::Index(i),
                    Err(_) => CString16::try_from(v).map(Default::Match).unwrap_or(Default::First),
                };
            }
            _ => {}
        }
    }
    (timeout, def)
}

fn resolve_default(entries: &[Entry], d: &Default) -> usize {
    match d {
        Default::Index(i) => (*i).min(entries.len() - 1),
        Default::First => 0,
        Default::Match(s) => {
            let needle = s.to_string();
            entries
                .iter()
                .position(|e| e.label.to_string().contains(&needle))
                .unwrap_or(0)
        }
    }
}

/// Read a text file off the volume this picker was loaded from.
fn read_self_file(path: &CStr16) -> Option<alloc::string::String> {
    let li = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()).ok()?;
    let dev = li.device()?;
    let mut fs: ScopedProtocol<SimpleFileSystem> = boot::open_protocol_exclusive(dev).ok()?;
    let mut root = fs.open_volume().ok()?;
    let mut file = root.open(path, FileMode::Read, FileAttribute::empty()).ok()?.into_regular_file()?;

    let mut data = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let n = file.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..n]);
    }
    Some(alloc::string::String::from_utf8_lossy(&data).into_owned())
}

// --- menu --------------------------------------------------------------

fn menu(entries: &[Entry], timeout: u64, mut sel: usize) -> Option<usize> {
    use uefi::proto::console::text::{Key, ScanCode};

    let mut remaining_ticks = timeout.saturating_mul(10); // 100 ms per tick
    let mut counting = timeout > 0;
    let mut shown: Option<(usize, Option<u64>)> = None; // (sel, countdown) last drawn

    loop {
        let countdown = if counting { Some(remaining_ticks.div_ceil(10)) } else { None };
        if shown != Some((sel, countdown)) {
            draw(entries, sel, countdown);
            shown = Some((sel, countdown));
        }

        // Poll the keyboard for ~100 ms.
        let key = uefi::system::with_stdin(|stdin| stdin.read_key().ok().flatten());
        match key {
            Some(Key::Special(ScanCode::UP)) => {
                sel = (sel + entries.len() - 1) % entries.len();
                counting = false;
            }
            Some(Key::Special(ScanCode::DOWN)) => {
                sel = (sel + 1) % entries.len();
                counting = false;
            }
            Some(Key::Special(ScanCode::ESCAPE)) => return Some(sel),
            Some(Key::Printable(c)) => {
                let ch = char::from(c);
                if ch == '\r' || ch == '\n' {
                    return Some(sel);
                }
                if let Some(d) = ch.to_digit(10) {
                    let d = d as usize;
                    if d < entries.len() {
                        return Some(d);
                    }
                }
                counting = false;
            }
            _ => {}
        }

        if counting {
            boot::stall(Duration::from_millis(100));
            remaining_ticks = remaining_ticks.saturating_sub(1);
            if remaining_ticks == 0 {
                return Some(sel);
            }
        } else {
            boot::stall(Duration::from_millis(30));
        }
    }
}

fn draw(entries: &[Entry], sel: usize, countdown: Option<u64>) {
    let _ = uefi::system::with_stdout(|o| o.clear());
    uefi::println!("  THOS boot picker");
    uefi::println!("  ----------------");
    for (i, e) in entries.iter().enumerate() {
        let mark = if i == sel { ">" } else { " " };
        let src = if e.from_nvram { "nvram" } else { "disk " };
        uefi::println!("  {mark} {i}. [{src}] {}", e.label);
    }
    uefi::println!();
    match countdown {
        Some(s) => uefi::println!("  booting `{}` in {s}s   (up/down = choose, enter = now)", entries[sel].label),
        None => uefi::println!("  up/down = choose, enter = boot, digit = boot that entry"),
    }
}

// --- chainload --------------------------------------------------------------

fn chainload(entry: &Entry) -> uefi::Result<()> {
    let dp = <&DevicePath>::try_from(entry.dp.as_slice()).map_err(|_| Status::INVALID_PARAMETER)?;
    let image = boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromDevicePath { device_path: dp, boot_policy: BootPolicy::ExactMatch },
    )?;
    boot::start_image(image).map(|_| ())
}

// --- small helpers --------------------------------------------------------

fn boot_var_name(id: u16) -> CString16 {
    let hex = b"0123456789ABCDEF";
    let buf: [u16; 9] = [
        b'B' as u16,
        b'o' as u16,
        b'o' as u16,
        b't' as u16,
        hex[(id >> 12 & 0xF) as usize] as u16,
        hex[(id >> 8 & 0xF) as usize] as u16,
        hex[(id >> 4 & 0xF) as usize] as u16,
        hex[(id & 0xF) as usize] as u16,
        0,
    ];
    CStr16::from_u16_with_nul(&buf).unwrap().to_owned()
}

fn decode_utf16(bytes: &[u8]) -> CString16 {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let mut s = CString16::new();
    for ch in char::decode_utf16(units).map(|r| r.unwrap_or('?')) {
        if let Ok(c) = Char16::try_from(ch) {
            let _ = s.push(c);
        }
    }
    s
}
