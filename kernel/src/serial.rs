// SPDX-License-Identifier: GPL-2.0-or-later
//! Minimal 16550 UART driver for COM1 (0x3F8).
//!
//! This is a bring-up crutch: polled, no interrupts, no locking beyond a spin
//! mutex. It is the headless output path for CI (`-serial stdio` under QEMU) and
//! for capturing logs from the real machine over a USB-serial adapter.
//! A real TTY / console subsystem arrives in Phase 2.

use spin::Mutex;

const COM1: u16 = 0x3F8;

static PORT: Mutex<Uart> = Mutex::new(Uart { base: COM1 });

struct Uart {
    base: u16,
}

impl Uart {
    unsafe fn outb(&self, offset: u16, value: u8) {
        core::arch::asm!(
            "out dx, al",
            in("dx") self.base + offset,
            in("al") value,
            options(nomem, nostack, preserves_flags),
        );
    }

    unsafe fn inb(&self, offset: u16) -> u8 {
        let value: u8;
        core::arch::asm!(
            "in al, dx",
            out("al") value,
            in("dx") self.base + offset,
            options(nomem, nostack, preserves_flags),
        );
        value
    }

    fn configure(&self) {
        unsafe {
            self.outb(1, 0x00); // disable interrupts
            self.outb(3, 0x80); // DLAB on
            self.outb(0, 0x03); // divisor low  -> 38400 baud
            self.outb(1, 0x00); // divisor high
            self.outb(3, 0x03); // 8N1, DLAB off
            self.outb(2, 0xC7); // enable FIFO, clear, 14-byte threshold
            self.outb(4, 0x0B); // RTS/DSR set
        }
    }

    fn write_byte(&self, byte: u8) {
        unsafe {
            while self.inb(5) & 0x20 == 0 {} // wait for THR empty
            self.outb(0, byte);
        }
    }
}

pub fn init() {
    PORT.lock().configure();
}

#[allow(dead_code)] // kept alongside kprintln! for callers that have a plain &str
pub fn print(s: &str) {
    let port = PORT.lock();
    write_str(&port, s);
}

/// Write raw bytes verbatim (no CR translation) — used by `write`/`writev`.
pub fn write_bytes(bytes: &[u8]) {
    let port = PORT.lock();
    for &b in bytes {
        port.write_byte(b);
    }
}

fn write_str(port: &Uart, s: &str) {
    for byte in s.bytes() {
        if byte == b'\n' {
            port.write_byte(b'\r');
        }
        port.write_byte(byte);
    }
}

struct Writer<'a>(&'a Uart);

impl core::fmt::Write for Writer<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write_str(self.0, s);
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    use core::fmt::Write;
    let port = PORT.lock();
    let _ = Writer(&port).write_fmt(args);
}

/// `kprintln!("frames: {}", n)` — formatted line to COM1.
#[macro_export]
macro_rules! kprintln {
    () => ($crate::serial::_print(format_args!("\n")));
    ($($arg:tt)*) => ($crate::serial::_print(format_args!("{}\n", format_args!($($arg)*))));
}
