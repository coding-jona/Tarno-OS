// SPDX-License-Identifier: GPL-2.0-or-later
//! The NT object namespace's `\Device\` + drive-letter view — real name
//! resolution, not the drive-letter-agnostic string strip this replaces.
//!
//! Real NT: `\Device\HarddiskVolume1` etc. are device objects; `\??\C:`
//! (aka `\DosDevices\C:`) is a *symbolic link* in the object manager
//! pointing at one of them — `CreateFileA("C:\...")` resolves the drive
//! letter to its device, then walks the rest of the path on that device.
//! THOS models exactly that indirection, just with a fixed, small table
//! instead of a full dynamic object manager (nothing mounts/unmounts yet):
//!
//! - `\Device\HarddiskVolume1` — the ext2 root, read/write.
//! - `\Device\CdRom0` — the boot ISO's EFI System Partition (FAT32,
//!   read-only) — real content already mounted at boot for the GPT/FAT
//!   milestone (`/EFI/THOS/HELLO.TXT`), not a synthetic second volume
//!   invented just to test this module.
//! - `C:` → `\Device\HarddiskVolume1`, `D:` → `\Device\CdRom0`.
//!
//! An unmapped drive letter, or an unrecognised `\Device\...` name, is now
//! a real failure (`resolve` returns `None`) — before this module existed,
//! *every* drive letter silently aliased the same ext2 root, so `D:\x` and
//! `C:\x` were indistinguishable and a typo'd drive letter just worked by
//! accident.

use alloc::string::String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    /// `\Device\HarddiskVolume1` — the ext2 filesystem.
    Ext2,
    /// `\Device\CdRom0` — the FAT32 ESP on the boot media.
    Cdrom,
}

/// Resolve an NT-ish path — `C:\...`, `\Device\HarddiskVolume1\...`,
/// `\Device\CdRom0\...`, or a bare `\`-rooted path with no device prefix at
/// all (e.g. `\Windows\System32\...` from a DLL lookup that never had a
/// drive letter to begin with — treated as the default volume, same as
/// every such caller got before this module existed) — to
/// `(device, posix path on that device)`. `None` for a drive letter or
/// `\Device\...` name nothing backs.
pub fn resolve(win: &str) -> Option<(Device, String)> {
    let b = win.as_bytes();
    if b.len() >= 2 && b[1] == b':' {
        let dev = match b[0].to_ascii_uppercase() {
            b'C' => Device::Ext2,
            b'D' => Device::Cdrom,
            _ => return None,
        };
        return Some((dev, to_posix(&win[2..])));
    }
    for (prefix, dev) in [
        ("\\Device\\HarddiskVolume1", Device::Ext2),
        ("\\Device\\CdRom0", Device::Cdrom),
    ] {
        if let Some(rest) = win.strip_prefix(prefix) {
            return Some((dev, to_posix(rest)));
        }
    }
    if win.starts_with("\\Device\\") {
        return None; // a real, but unmapped, device name
    }
    Some((Device::Ext2, to_posix(win)))
}

fn to_posix(s: &str) -> String {
    let mut out = String::from("/");
    for part in s.split(|c| c == '\\' || c == '/').filter(|p| !p.is_empty()) {
        if out.len() > 1 {
            out.push('/');
        }
        out.push_str(part);
    }
    out
}

/// Mount `\Device\CdRom0` fresh (cheap — the same on-demand re-open pattern
/// `ext2::open()` already uses; there is no long-lived global mount table
/// yet). `None` if the boot media has no ESP (never true for THOS's own
/// disk images, but a real possibility on unusual hardware).
pub fn open_cdrom() -> Option<crate::fat::Fat> {
    let esp_lba = crate::gpt::find_esp(51_000)?;
    crate::fat::Fat::open(esp_lba).ok()
}
