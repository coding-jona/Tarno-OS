/* SPDX-License-Identifier: GPL-2.0-or-later
 *
 * Milestone 3: a real, compiler-produced statically linked Win32 console .exe.
 * Built with mingw-w64, `-nostdlib` (own entry, no C runtime) so the only
 * import is KERNEL32.dll — a genuine toolchain PE (real import table, real
 * base relocations) that binds against THOS's synthetic kernel32 and runs
 * through the NT path with no Wine process in the tree.
 *
 *   x86_64-w64-mingw32-gcc -O2 -nostdlib -Wl,-e,wincon_start -o wincon.exe \
 *       wincon.c -lkernel32
 */
#include <windows.h>

static unsigned slen(const char *s) {
    unsigned n = 0;
    while (s[n]) n++;
    return n;
}
static void out(const char *s) {
    DWORD wr;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), s, slen(s), &wr, NULL);
}

void __stdcall wincon_start(void) {
    out("WINCON: hello from mingw\r\n");

    HANDLE f = CreateFileA("C:\\pe-read.txt", GENERIC_READ, FILE_SHARE_READ,
                           NULL, OPEN_EXISTING, 0, NULL);
    if (f != INVALID_HANDLE_VALUE) {
        char buf[64];
        DWORD n = 0;
        if (ReadFile(f, buf, sizeof buf - 1, &n, NULL) && n) {
            buf[n] = 0;
            out("WINCON: read C:\\pe-read.txt -> ");
            out(buf);
        }
        CloseHandle(f);
    }

    HANDLE ev = CreateEventA(NULL, TRUE, TRUE, NULL);
    if (WaitForSingleObject(ev, INFINITE) == WAIT_OBJECT_0)
        out("WINCON: WaitForSingleObject ok\r\n");
    CloseHandle(ev);

    out("WINCON: exit ok\r\n");
    ExitProcess(0);
}
