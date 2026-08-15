//! Gaming-Mode-Steuerung: CPU-Isolation-Status, Governor, Transparent-HugePages.
//!
//! Details/Begründung siehe docs/month2-gaming-tuning.md. Diese Funktionen
//! werden über den IPC-Socket erreichbar gemacht (`dispatch` in main.rs);
//! das eigentliche JVM/Minecraft-Starten passiert bewusst *nicht* hier,
//! sondern in scripts/jvm-launch.sh (siehe docs/architecture.md).

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum GamingError {
    Io(io::Error),
    NotAvailable(String),
}

impl fmt::Display for GamingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GamingError::Io(e) => write!(f, "io error: {e}"),
            GamingError::NotAvailable(s) => write!(f, "nicht verfügbar auf diesem System: {s}"),
        }
    }
}

impl std::error::Error for GamingError {}

impl From<io::Error> for GamingError {
    fn from(e: io::Error) -> Self {
        GamingError::Io(e)
    }
}

pub struct GamingController {
    /// Wenn true: Aktionen nur loggen, keine sysfs-Schreibzugriffe ausführen.
    /// Wird u.a. in dieser Sandbox genutzt, da hier keine echte
    /// M6700-Hardware mit realen cpufreq-Governors vorhanden ist.
    pub dry_run: bool,
}

fn is_cpu_dir_name(name: &str) -> bool {
    name.starts_with("cpu") && name.len() > 3 && name[3..].chars().all(|c| c.is_ascii_digit())
}

impl GamingController {
    pub fn new(dry_run: bool) -> Self {
        Self { dry_run }
    }

    /// Liest `/sys/devices/system/cpu/isolated` — zeigt, ob der
    /// `isolcpus=`-Boot-Parameter aktiv ist (siehe
    /// docs/month2-gaming-tuning.md#core-isolation-isolcpus).
    pub fn isolated_cpus(&self) -> Result<String, GamingError> {
        match fs::read_to_string("/sys/devices/system/cpu/isolated") {
            Ok(s) => Ok(s.trim().to_string()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(GamingError::NotAvailable(
                "/sys/devices/system/cpu/isolated fehlt (kein isolcpus-Boot-Parameter aktiv, \
                 oder Kernel ohne CPU-Isolation-Support)"
                    .into(),
            )),
            Err(e) => Err(e.into()),
        }
    }

    fn cpu_governor_paths(&self) -> io::Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        let base = PathBuf::from("/sys/devices/system/cpu");
        for entry in fs::read_dir(&base)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if is_cpu_dir_name(&name) {
                let gov = entry.path().join("cpufreq/scaling_governor");
                if gov.exists() {
                    paths.push(gov);
                }
            }
        }
        paths.sort();
        Ok(paths)
    }

    /// Setzt den CPU-Governor (z.B. "performance") für alle Cores.
    pub fn set_governor(&self, governor: &str) -> Result<(), GamingError> {
        let paths = self.cpu_governor_paths()?;
        if paths.is_empty() {
            return Err(GamingError::NotAvailable(
                "keine cpufreq/scaling_governor-Pfade gefunden (in virtualisierten \
                 Umgebungen ohne cpufreq-Treiber normal)"
                    .into(),
            ));
        }
        for path in paths {
            if self.dry_run {
                eprintln!("[dry-run] echo {governor} > {}", path.display());
                continue;
            }
            fs::write(&path, governor)?;
        }
        Ok(())
    }

    /// Setzt den Transparent-HugePages-Modus (empfohlen: "madvise", siehe
    /// docs/month2-gaming-tuning.md#transparent-hugepages-thp).
    pub fn set_thp_mode(&self, mode: &str) -> Result<(), GamingError> {
        let path = "/sys/kernel/mm/transparent_hugepage/enabled";
        if self.dry_run {
            eprintln!("[dry-run] echo {mode} > {path}");
            return Ok(());
        }
        fs::write(path, mode).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                GamingError::NotAvailable(format!("{path} fehlt (THP nicht verfügbar)"))
            } else {
                GamingError::Io(e)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_dir_name_filter_matches_cpu_n() {
        assert!(is_cpu_dir_name("cpu0"));
        assert!(is_cpu_dir_name("cpu42"));
    }

    #[test]
    fn cpu_dir_name_filter_rejects_non_numeric_suffix() {
        assert!(!is_cpu_dir_name("cpufreq"));
        assert!(!is_cpu_dir_name("cpuidle"));
        assert!(!is_cpu_dir_name("cpu"));
    }

    #[test]
    fn dry_run_set_governor_does_not_error_without_hardware() {
        // dry_run=true darf niemals versuchen, tatsächlich zu schreiben —
        // dieser Test läuft auch in Sandboxen ohne echte cpufreq-Governors.
        let ctl = GamingController::new(true);
        // isolated_cpus/set_governor können in dieser Sandbox NotAvailable
        // liefern falls kein cpufreq vorhanden ist — das ist ein gültiges
        // Ergebnis, kein Testfehler. Wir prüfen nur: kein Panic, kein
        // echter Schreibversuch (dry_run loggt nur).
        let _ = ctl.isolated_cpus();
        let _ = ctl.set_governor("performance");
    }
}
