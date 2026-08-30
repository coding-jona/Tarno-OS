# FROZEN COMPONENTS

The project has pivoted from a **Devuan-based Linux distribution** to **THOS — a
clean-slate hybrid kernel** that runs native ELF/POSIX and native PE/Win32 programs side
by side on one target machine (see `docs/thos/`).

The Devuan-distro components below are **frozen**: they stay in the repository and in git
history but receive no further development. They may return later as userland packages
running on THOS's POSIX personality.

| Path | What it was | Status |
|---|---|---|
| `main.go`, `cmd/`, `go.mod`, `go.sum` | `tarnod` Go daemon + `tarno-install` / `tarnoctl` / `tarno-disk-install` CLIs | Frozen |
| `tarno/` | Assistant / Mistral client, desktop bits | Frozen |
| `tarno-devuan-live/` | `live-build` config for the Devuan live ISO | Frozen |
| `scripts/` | ISO / apt-repo / build scripts | Frozen |
| `docs/tarno-ai-roadmap.md` | AI-assistant roadmap | Frozen |
| `docs/legacy-roadmap-devuan.md` | the previous distro `ROADMAP.md` | Superseded by `docs/thos/roadmap.md` |

CI for the above is disabled (`.github/workflows/*.yml.frozen`:
`apt-repo`, `build-devuan-image`, `go-lint`). New CI: `.github/workflows/qemu-boot.yml`.

## Reviving a component

Nothing is deleted. Branch from a commit at or before the freeze and cherry-pick
forward, or port the component to run as a THOS POSIX-personality program once Phase 2
of `docs/thos/roadmap.md` is complete.
