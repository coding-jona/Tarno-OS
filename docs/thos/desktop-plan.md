# THOS – Desktop plan (side quest)

*Opened 2026-10-02 (user request). A planning document: nothing below except Stage 0
is built yet. It is a **side quest** — it must not block the kernel roadmap in
[`roadmap.md`](roadmap.md), and it ships in independently useful stages.*

## 0. Where we are today

- A kernel-side **framebuffer text console** (`kernel/src/fbcon.rs`, 8x16 font)
  mirrors the serial output; BusyBox `sh` runs on it with the PS/2 keyboard.
  This is the shell-first baseline and the fallback for everything below.
- `gdi.rs` / `window.rs` give Win32 programs a window-relative DC on the raw
  framebuffer: **no compositor, no z-order, no occlusion, no input routing.**
- Phase 4 of the roadmap plans a Wayland compositor on a KMS object — written for
  the RX 6600. That GPU path does not exist on the Acer.

## 1. Constraints that shape every decision

| Constraint | Consequence |
|---|---|
| **Acer 5742G**: Westmere, 15.6" panel (1366x768 expected), Radeon HD 5470M with **no driver**, VBE linear framebuffer only | The desktop is **CPU-rendered into a linear framebuffer** first. GPU acceleration is a later, separate project (Evergreen on Acer, RDNA2 on the ASRock). The design must not *assume* a GPU. |
| Two machines, two firmware worlds (BIOS/MBR vs UEFI/GOP) | Display backend is an interface (`scanout`): `VbeFb` now, `Kms` later. The compositor never touches Limine/GOP directly. |
| **Two personalities** (POSIX/Wayland-style clients *and* Win32/GDI clients) | One compositor, one surface object (the "one object, many views" rule in `architecture.md`). Win32 `HWND` and a POSIX client surface are two views of the same kernel/userspace surface object. |
| Security is a product feature (Security Service, SAK trusted path, `elevate()`) | Login, UAC-style elevation and AV prompts live on a **secure desktop** that no client can draw over or read input from. |
| Memory/CPU are modest (2010 laptop, maybe 4 GiB) | Damage-tracked redraw, no per-frame full-screen composition, small toolkit, no browser-engine UI. |
| No ML/AI work for now (*user, 2026-10-02*) | No assistant panel in scope. |
| Licensing ([`licensing.md`](licensing.md)) | Only permissive/GPL-compatible fonts, icons and toolkit code; record every third-party asset. |

## 1b. Principle: everything is customisable (*user, 2026-10-02*)

The user can change **the whole system and the whole shell, to 100 %**, the way a
Linux desktop allows — not just colours. Designing for that now:

- **Everything is data, not code**: theme, layout, panel contents, shortcuts,
  window rules, launcher entries, icons and fonts live as plain files under
  `/etc/thos/` (system) and `~/.config/thos/` (user), user overrides system.
  Live-reload on change (the compositor watches them — same mechanism as the
  registry change-notify).
- **The shell is just a client.** Panel, launcher, file manager, lock screen are
  ordinary programs speaking the public surface protocol; the user can replace or
  kill any of them and run another (or none). Nothing in the compositor is
  privileged except the secure-desktop pieces (login, elevate, AV prompts, SAK),
  which stay fixed so customisation can never remove the security boundary.
- **Scriptable compositor**: a documented control socket (list windows, move,
  tile, bind keys, query/set config) and a small config/scripting language, so
  tiling, hot corners, per-app rules etc. are user-level code.
- **Swappable parts with stable interfaces**: shell, toolkit theme engine, input
  method/keyboard layout, notification daemon, terminal — each behind a protocol.
- **Source and rebuild on the machine**: the desktop ships with its sources and a
  documented build, so a user can modify and rebuild it in place (the "live
  developing" loop); safe-mode boot (stock shell, ignore user config) as the
  recovery path.
- **Reset is always one step away**: a broken user config must never lock the user
  out (config validation, last-known-good copy, Safe-mode).

## 2. Architecture (target)

```
 apps: POSIX (own protocol)      Win32 (user32/gdi32 over the same surfaces)
          \                         /
           +-----> thos-compositor (userspace, privileged service) <---- input events
                    |  surfaces (shared memory, damage rects, z-order, focus)
                    v
                 scanout  (VbeFb | later Kms)         secure desktop (login / elevate / AV)
```

- **Compositor is a userspace process**, not kernel code (same reasoning as the
  Security Service: a bug in window management must not panic the machine).
- **Kernel provides:** a scanout/framebuffer object, shared-memory surfaces
  (existing section objects), an **input event queue** (keyboard + mouse), a
  vsync/timer tick. The kernel text console keeps working underneath as the
  panic/fallback screen.
- **Protocol:** a small own protocol over the existing object/pipe primitives,
  Wayland-shaped (surfaces, buffers, damage, frame callbacks, seat). We borrow the
  *model*, and only decide later whether to speak real Wayland wire format so
  existing toolkits can attach — **Open decision D1**.
- **Win32 path:** `window.rs`/`gdi.rs` stop drawing straight onto the screen and
  instead render into the window's surface; `WM_PAINT`/`InvalidateRect` map to
  surface damage.
- **Rendering:** pixman-class software blitter (own, small). 32-bit XRGB, damage
  rectangles, optional back buffer, no per-pixel alpha beyond rectangles at first.
- **Text:** one font stack for the whole desktop: bitmap font for the console,
  a TrueType rasteriser (`stb_truetype`/`fontdue`-class) for the UI; a single
  shipped UI font (e.g. an OFL sans) — **D2**.

## 3. Stages (each independently useful and testable)

**Stage 0 — Shell-first (done / finishing).** Framebuffer console + PS/2 + BusyBox.
Remaining: scrollback (Shift+PgUp), key repeat, IRQ-driven i8042 instead of
polling (avoid lost scancodes), German layout switch, `reboot`/`poweroff`
(ACPI), `help`/`man`-style built-ins, clean shutdown (flush ext2).

**Stage 1 — Input foundation.** PS/2 **mouse/touchpad** (the 5742G uses a PS/2
Synaptics-class pad: start with plain relative mode, gestures later), a kernel
input queue with timestamps, EHCI (USB 2) for external mice/keyboards, a
keyboard-layout table in userspace rather than in the kernel.

**Stage 2 — Display service.** Move scanout ownership to a userspace
`display` service; mode info from VBE, clean hand-off from `fbcon`
(console returns on crash/panic). *Milestone:* a userspace program draws a
rectangle and a cursor that follows the touchpad.

**Stage 3 — Compositor core.** Surfaces, z-order, damage, focus, move/resize,
minimise, per-window clipping, software cursor. Win32 windows from
`window.rs` become surfaces. *Milestone:* two Win32 `.exe` and one POSIX client
share the screen, overlap correctly and keep focus/keyboard routing right.

**Stage 4 — Secure desktop & session.** Graphical login, lock screen, SAK
(Ctrl+Alt+Del) switching to the secure desktop, `elevate()` consent dialog,
Security Service alerts (quarantine / execgate verdicts) shown there. No client
can screenshot or inject input into the secure desktop.

**Stage 5 — Shell UX ("professional").** Panel/taskbar, launcher, window
switcher, notifications, file manager, terminal emulator (real VT in a window),
text editor, settings (display, keyboard, accounts, network later). A
consistent design language: spacing scale, one icon set, light/dark theme,
keyboard-first navigation, 1366x768 as the *reference* resolution (and 1920x1080
for the ASRock). Accessibility basics: scalable UI, high contrast, full keyboard
operability.

**Stage 6 — Polish & performance.** Antialiased text, rounded corners, shadows
and animations only where profiling shows headroom on Westmere; vsync/tearing
policy; multi-monitor; HiDPI scale factor.

**Stage 7 — GPU (separate project).** Evergreen KMS for the Acer, RDNA2 KMS +
RADV for the ASRock (roadmap Phase 4), then a GPU compositing backend behind the
same `scanout` interface. Nothing above depends on this.

## 4. Things to get right up front (cheap now, expensive later)

1. **Surfaces are kernel objects with handles** — capability-controlled, so the
   same security model covers windows (who may read another window's pixels?).
2. **Input goes to the compositor only**, which routes by focus; SAK is detected
   *before* routing (already true in `console::feed_report`).
3. **Fallback is always the text console** — a crashed compositor must restart
   or drop to the shell, never leave a dead screen.
4. **Deterministic rendering** so tests can compare screenshots: no
   time-dependent animation in test mode; fixed font; `screendump` golden images
   in `cargo xtask` (the VM `screendump` flow already works).
5. **Resolution independence in layout code** (a scale factor from day one).
6. **One event loop model** for compositor and toolkit; no per-toolkit threads
   fighting the scheduler.
7. **Power/idle**: compositor sleeps when nothing is damaged (laptop battery).

## 5. Open decisions

- **D1** Own protocol vs real Wayland wire format (reuse of GTK/Qt/SDL clients
  vs. simplicity). Lean: own protocol first, Wayland-compatible shim later.
- **D2** UI toolkit: write a small own widget layer vs port an existing one
  (e.g. an immediate-mode toolkit like Slint-/egui-class in Rust, or LVGL).
  Lean: small own layer on top of the surface API for system apps, port a
  toolkit only for third-party GUI apps.
- **D3** Font and icon set (licence-checked, OFL/permissive).
- **D4** Keep `window.rs`/`gdi.rs` in kernel, or move the Win32 window manager
  into the userspace compositor (Lean: move, once Stage 3 is in).
- **D5** Name/branding and visual identity of the shell.

## 6. How we verify

`cargo xtask` gains: `fb-test` (screendump of the console after scripted input,
compared to a golden image), later `compositor-test` (overlap/focus/damage
scenarios) and `desktop-smoke` (boot → login → launch app → screenshot). Every
stage lands with a QEMU test, as the kernel roadmap does.
