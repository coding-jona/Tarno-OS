//! Sichere Enumeration von Wechseldatenträgern zum Beschreiben.
//!
//! Sicherheitsprinzip (wie Raspberry Pi Imager/Rufus/balenaEtcher): nur
//! Geräte mit `/sys/block/<dev>/removable == "1"` werden überhaupt
//! gelistet — Festplatten/SSDs/virtio-Root-Disks (`removable=0`) tauchen
//! gar nicht erst auf. `root_device_name()` ist eine zusätzliche
//! Verteidigungsebene, die das Root-Gerät explizit ausschließt, selbst
//! falls es fälschlich als removable markiert wäre. Beides ist eine
//! Komfort-/Sicherheitsschicht zusätzlich zur expliziten Bestätigung in
//! der UI (siehe `app.rs`), kein Ersatz dafür.

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use crate::flasher::format_bytes;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDevice {
    /// z.B. "sdb"
    pub name: String,
    /// z.B. "/dev/sdb"
    pub path: PathBuf,
    pub size_bytes: u64,
    pub model: Option<String>,
    pub vendor: Option<String>,
}

impl BlockDevice {
    pub fn label(&self) -> String {
        let size = format_bytes(self.size_bytes);
        match (&self.vendor, &self.model) {
            (Some(v), Some(m)) => format!("{} — {} {} ({size})", self.path.display(), v.trim(), m.trim()),
            (None, Some(m)) => format!("{} — {} ({size})", self.path.display(), m.trim()),
            _ => format!("{} ({size})", self.path.display()),
        }
    }
}

#[cfg(unix)]
const NON_PHYSICAL_PREFIXES: &[&str] = &["loop", "zram", "dm-", "md", "sr", "ram"];

/// Listet alle als Wechseldatenträger erkannten Blockgeräte, abzüglich des
/// Root-/Systemgeräts. Plattformunabhängige Fassade — die eigentliche
/// Enumeration (`/sys/block` unter Linux, Win32-`IOCTL`s unter Windows,
/// siehe `win32.rs`) und Root-Geräte-Erkennung stecken hinter
/// `list_removable_devices_raw()`/`root_device_name()`. Gibt bei
/// Zugriffsproblemen eine leere Liste zurück statt eines Fehlers — der
/// Installer soll auch dann noch starten, nur eben ohne automatisch
/// erkannte Geräte.
pub fn list_removable_devices() -> Vec<BlockDevice> {
    let root = root_device_name();
    list_removable_devices_raw()
        .into_iter()
        .filter(|d| Some(&d.name) != root.as_ref())
        .collect()
}

#[cfg(unix)]
fn list_removable_devices_raw() -> Vec<BlockDevice> {
    list_removable_devices_in(Path::new("/sys/block"))
}

#[cfg(windows)]
fn list_removable_devices_raw() -> Vec<BlockDevice> {
    crate::win32::list_physical_drives()
}

#[cfg(not(any(unix, windows)))]
fn list_removable_devices_raw() -> Vec<BlockDevice> {
    Vec::new()
}

#[cfg(unix)]
fn list_removable_devices_in(sys_block: &Path) -> Vec<BlockDevice> {
    let Ok(entries) = fs::read_dir(sys_block) else {
        return Vec::new();
    };

    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if NON_PHYSICAL_PREFIXES.iter().any(|p| name.starts_with(p)) {
            continue;
        }

        let dev_dir = entry.path();
        if !is_removable(&dev_dir) {
            continue;
        }
        let Some(size_bytes) = read_size_bytes(&dev_dir) else {
            continue;
        };
        if size_bytes == 0 {
            continue;
        }

        devices.push(BlockDevice {
            path: PathBuf::from(format!("/dev/{name}")),
            model: read_trimmed(&dev_dir.join("device/model")),
            vendor: read_trimmed(&dev_dir.join("device/vendor")),
            name,
            size_bytes,
        });
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    devices
}

#[cfg(unix)]
fn is_removable(dev_dir: &Path) -> bool {
    read_trimmed(&dev_dir.join("removable")).as_deref() == Some("1")
}

/// `/sys/block/<dev>/size` ist in 512-Byte-Sektoren angegeben.
#[cfg(unix)]
fn read_size_bytes(dev_dir: &Path) -> Option<u64> {
    let raw = read_trimmed(&dev_dir.join("size"))?;
    raw.parse::<u64>().ok().map(|sectors| sectors * 512)
}

#[cfg(unix)]
fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Liefert den Namen des Root-/System-Geräts, falls ermittelbar — unter
/// Linux z.B. "vda" (aus `/proc/mounts`), unter Windows z.B.
/// "PhysicalDrive0" (aus `win32::system_physical_drive_name`). Wird gegen
/// `BlockDevice::name` verglichen, um dieses Gerät aus der Auswahlliste
/// auszuschließen (siehe `list_removable_devices`).
#[cfg(unix)]
pub fn root_device_name() -> Option<String> {
    let mounts = fs::read_to_string("/proc/mounts").ok()?;
    root_device_name_from_mounts(&mounts)
}

#[cfg(windows)]
pub fn root_device_name() -> Option<String> {
    crate::win32::system_physical_drive_name()
}

#[cfg(not(any(unix, windows)))]
pub fn root_device_name() -> Option<String> {
    None
}

#[cfg(unix)]
fn root_device_name_from_mounts(mounts: &str) -> Option<String> {
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let source = fields.next()?;
        let mountpoint = fields.next()?;
        if mountpoint == "/" {
            let base = source.rsplit('/').next()?;
            return Some(strip_partition_suffix(base));
        }
    }
    None
}

/// "vda2" -> "vda", "sda1" -> "sda", "nvme0n1p2" -> "nvme0n1". Eine
/// Heuristik (kein vollständiger Geräte-Namen-Parser) — ausreichend als
/// zusätzliche Sicherheitsebene, siehe Modul-Kommentar.
#[cfg(unix)]
fn strip_partition_suffix(name: &str) -> String {
    if let Some(idx) = name.rfind('p') {
        let (head, tail) = name.split_at(idx);
        let suffix = &tail[1..];
        if head.ends_with(|c: char| c.is_ascii_digit()) && !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return head.to_string();
        }
    }
    let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if trimmed.is_empty() {
        name.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn make_fake_sys_block(root: &Path) {
        // Ein "echtes" (nicht-removable) System-Laufwerk - darf NIE auftauchen.
        write(&root.join("vda/removable"), "0\n");
        write(&root.join("vda/size"), "536870912\n");

        // Ein Wechseldatenträger, wie er bei einem echten USB-Stick aussähe.
        write(&root.join("sdb/removable"), "1\n");
        write(&root.join("sdb/size"), "31266816\n"); // ~14.9 GiB
        write(&root.join("sdb/device/model"), "Cruzer Blade   \n");
        write(&root.join("sdb/device/vendor"), "SanDisk \n");

        // Ein removable-Gerät ohne lesbare Model/Vendor-Datei (z.B. Kartenleser-Slot ohne Karte) -
        // sollte trotzdem auftauchen, nur ohne Modellname.
        write(&root.join("mmcblk0/removable"), "1\n");
        write(&root.join("mmcblk0/size"), "0\n"); // kein Medium eingelegt -> size 0 -> wird gefiltert

        // loop-Geräte müssen ausgeschlossen bleiben, selbst falls (untypisch) removable=1 gesetzt wäre.
        write(&root.join("loop0/removable"), "1\n");
        write(&root.join("loop0/size"), "204800\n");
    }

    #[test]
    fn lists_only_removable_physical_devices_with_media() {
        let dir = std::env::temp_dir().join(format!(
            "tarno-installer-test-sysblock-{}-{}",
            std::process::id(),
            "lists_only_removable"
        ));
        let _ = fs::remove_dir_all(&dir);
        make_fake_sys_block(&dir);

        let devices = list_removable_devices_in(&dir);

        assert_eq!(devices.len(), 1, "erwartet genau ein Gerät (sdb), gefunden: {devices:?}");
        let sdb = &devices[0];
        assert_eq!(sdb.name, "sdb");
        assert_eq!(sdb.path, PathBuf::from("/dev/sdb"));
        assert_eq!(sdb.size_bytes, 31266816 * 512);
        assert_eq!(sdb.vendor.as_deref(), Some("SanDisk"));
        assert_eq!(sdb.model.as_deref(), Some("Cruzer Blade"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn label_includes_size_and_model() {
        let dev = BlockDevice {
            name: "sdb".into(),
            path: PathBuf::from("/dev/sdb"),
            size_bytes: 16 * 1024 * 1024 * 1024,
            model: Some("Cruzer Blade".into()),
            vendor: Some("SanDisk".into()),
        };
        let label = dev.label();
        assert!(label.contains("/dev/sdb"));
        assert!(label.contains("SanDisk"));
        assert!(label.contains("Cruzer Blade"));
        assert!(label.contains("GiB"));
    }

    #[test]
    fn strip_partition_suffix_handles_common_schemes() {
        assert_eq!(strip_partition_suffix("vda2"), "vda");
        assert_eq!(strip_partition_suffix("sda1"), "sda");
        assert_eq!(strip_partition_suffix("nvme0n1p2"), "nvme0n1");
        assert_eq!(strip_partition_suffix("sdb"), "sdb");
    }

    #[test]
    fn root_device_name_parses_proc_mounts_format() {
        let mounts = "/dev/vda / ext4 rw,relatime 0 0\n/dev/vdb /boot vfat rw 0 0\n";
        assert_eq!(root_device_name_from_mounts(mounts).as_deref(), Some("vda"));
    }

    #[test]
    fn root_device_name_none_when_no_root_mount_present() {
        let mounts = "/dev/vdb /data ext4 rw 0 0\n";
        assert_eq!(root_device_name_from_mounts(mounts), None);
    }

    /// Realer, nicht-synthetischer Test gegen das tatsächliche /sys/block
    /// dieser Sandbox: bestätigt die zentrale Sicherheitseigenschaft direkt
    /// gegen echte Daten — das Root-Gerät (in dieser Sandbox: vda) darf
    /// unter keinen Umständen in der Liste auftauchen.
    #[test]
    fn real_sys_block_excludes_root_device() {
        let devices = list_removable_devices();
        let root = root_device_name();
        if let Some(root_name) = root {
            assert!(
                !devices.iter().any(|d| d.name == root_name),
                "Root-Gerät {root_name} tauchte in der removable-Liste auf: {devices:?}"
            );
        }
    }
}
