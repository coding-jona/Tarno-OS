# bpf-linker-Setup: Stolpersteine & funktionierende Konfiguration

Dieses Dokument hält fest, wie `bpf-linker` (nötig, um `tarnod-guard-ebpf`
von LLVM-IR zu einem finalen eBPF-ELF-Objekt zu linken) in dieser Umgebung
tatsächlich zum Laufen gebracht wurde — inklusive der Sackgassen, damit sie
nicht wiederholt werden.

## Kurzfassung (funktionierender Weg)

```sh
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
rustup component add llvm-tools --toolchain nightly   # optional, nicht zwingend
sudo apt-get install -y llvm-20-dev                    # liefert llvm-config-20
cargo install bpf-linker --version 0.10.4 --locked
```

Test: `bpf-linker --version` → `bpf-linker 0.10.4`.

## Warum nicht einfach `cargo install bpf-linker` (aktuelle Version)?

`bpf-linker` 0.11.0 (aktuelle Version zum Zeitpunkt dieser Notiz) hängt laut
Crate-Metadaten von `llvm-sys-21`, `llvm-sys-22` **oder** `llvm-sys-23` ab —
also LLVM 21, 22 oder 23. Diese Umgebung hatte über `apt` nur bis LLVM 20
verfügbar (`llvm-20-dev`). Der Build schlug entsprechend fehl:

```
error: undefined symbol: LLVMParseIRInContext2
       did you mean: LLVMParseIRInContext
```

`LLVMParseIRInContext2` existiert schlicht nicht in Upstream-LLVM 18 oder 20
(geprüft per `nm -D libLLVM.so`) — es ist eine neuere LLVM-C-API-Funktion.

**Rustc bringt zwar selbst ein passendes LLVM mit** (die nightly-Toolchain
hier hat `libLLVM.so.23.1-rust-...`, siehe `rustc +nightly --version`), aber
`llvm-sys-23` (die von bpf-linker 0.11 genutzte Crate) sucht zur Build-Zeit
zwingend nach einer **`llvm-config`-Binary** (Env `LLVM_SYS_231_PREFIX`, muss
`bin/llvm-config` enthalten) — die bringt rustc nicht mit, auch nicht über
die `llvm-tools`-Komponente. Ohne selbst LLVM 21-23 aus Quellcode zu bauen
(mehrere Stunden, in dieser Sandbox nicht sinnvoll) war das eine Sackgasse.

## Der funktionierende Weg: ältere `bpf-linker`-Version mit `aya-rustc-llvm-proxy`

`bpf-linker` 0.10.x nutzt noch die Crate `aya-rustc-llvm-proxy`, die
automatisch ein passendes LLVM findet/verlinkt (System-LLVM **oder** das von
rustc gebündelte), statt stur eine exakte `llvm-config`-Major-Version zu
verlangen. Mit `llvm-20-dev` installiert (liefert `llvm-config-20`) und
`cargo install bpf-linker --version 0.10.4 --locked` baute es sauber durch.

## Verifier-Fehler nach dem ersten erfolgreichen Build

Mit `debug = 2` im Release-Profil von `tarnod-guard-ebpf` (BTF.ext mit
Funcinfo) schlug das Laden des Programms mit folgendem Kernel-Verifier-Fehler
fehl:

```
BPF_PROG_LOAD syscall returned Invalid argument (os error 22).
Verifier output: number of funcs in func_info doesn't match number of subprogs
```

Vermutliche Ursache: Versions-Mismatch zwischen dem (bewusst älteren)
`bpf-linker` 0.10.4 und dem aktuellen `aya-ebpf` 0.2.1 bei der Erzeugung von
`.BTF.ext`/`func_info`. **Fix:** `debug = false` statt `debug = 2` im
`[profile.release.package.tarnod-guard-ebpf]`-Abschnitt (siehe `Cargo.toml`)
— das Objekt enthält dann kein `.BTF.ext` mit Funcinfo, der Verifier-Check
entfällt, das Programm lädt und attached sich fehlerfrei. Nachteil: kein
BTF-basiertes CO-RE-Debugging für dieses Programm; für die hier verwendeten
Helper-Funktionen (keine Struct-Feld-Relocations) ohne Belang.

## Verifiziert in dieser Sandbox

Nach obigem Fix: `tarnod-guard-standalone` lädt das Programm, attached an
`sched:sched_process_exec`, und empfängt reale Events für tatsächlich
ausgeführte Prozesse (`/bin/true`, `/usr/bin/id`, ...) mit korrektem
PID/UID/comm/Dateipfad über die RingBuf-Map. Siehe
`tarnod-guard/src/main.rs` für den Test-Client.

## Falls auf einem anderen System neu gebaut werden muss

Falls dort `llvm-21-dev`/`llvm-22-dev`/`llvm-23-dev` via Paketmanager
verfügbar ist, sollte auch aktuelles `bpf-linker` (0.11.x) direkt
funktionieren — dann kann ggf. auch `debug = 2` (mit BTF.ext) probiert
werden, da der oben beschriebene Verifier-Fehler mit einer zur `aya-ebpf`-
Version passenden `bpf-linker`-Version vermutlich nicht auftritt.
