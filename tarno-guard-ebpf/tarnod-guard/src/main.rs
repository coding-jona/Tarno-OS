//! `tarnod-guard-standalone` — CLI zum manuellen Testen des eBPF-Guards
//! unabhängig von `tarnod`. Lädt das Programm, druckt jedes `ExecEvent` auf
//! stdout. Nützlich zur Verifikation auf einem neuen System, bevor das
//! Feature in `tarnod` aktiviert wird (siehe docs/month3-tarno-layer.md).

use anyhow::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();

    let mut guard = tarnod_guard::Guard::load()?;
    println!("tarnod-guard-standalone: eBPF-Programm geladen und an sched_process_exec attached.");
    println!("Warte auf execve()-Events (Strg+C zum Beenden) ...");

    let mut events = guard.events()?;
    loop {
        let ev = events.next().await?;
        println!(
            "exec: pid={} uid={} comm={:?} filename={:?}",
            ev.pid,
            ev.uid,
            ev.comm_str(),
            ev.filename_str()
        );
    }
}
