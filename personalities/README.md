# personalities/

Per-process API personalities over the one Executive Core.

- `posix/` — Linux syscall-ABI subset, signals, futex, `/proc` view. Phase 2.
- `nt/` — SSDT dispatch, `Ob/Ps/Io/Se`, SEH, APC, `\Device` namespace, registry view. Phase 3.

Neither personality calls the other. Cross-talk is only through shared executive objects.
