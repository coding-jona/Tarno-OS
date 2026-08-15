//! Lokale Uhrzeit als "HH:MM:SS"-String, ohne `chrono`-Abhängigkeit
//! (Zeitzonendatenbank + Formatierungs-Stack wäre für eine simple
//! Taskleisten-Uhr unverhältnismäßig schwer) — direkt über die libc, die
//! dieses Projekt ohnehin schon nutzt (siehe `tarnod/tarnod/src/*.rs`).

pub fn current_time_hh_mm_ss() -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    unsafe {
        libc::localtime_r(&now, &mut tm);
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_hh_mm_ss_format() {
        let s = current_time_hh_mm_ss();
        assert_eq!(s.len(), 8);
        assert_eq!(s.as_bytes()[2], b':');
        assert_eq!(s.as_bytes()[5], b':');
        for (i, c) in s.chars().enumerate() {
            if i == 2 || i == 5 {
                continue;
            }
            assert!(c.is_ascii_digit(), "unerwartetes Zeichen an Position {i}: {c}");
        }
    }
}
