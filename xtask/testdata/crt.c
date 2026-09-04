/* SPDX-License-Identifier: GPL-2.0-or-later
 *
 * Milestone 3+ : a mingw-w64 `int main` console .exe with the full C-runtime
 * startup — imports `msvcrt.dll` (CRT init + stdio) on top of KERNEL32.
 * Runs against THOS's synthetic `msvcrt` (no Wine, no real msvcrt).
 *
 *   x86_64-w64-mingw32-gcc -O2 -o crt.exe crt.c
 */
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    printf("CRT: hello from the mingw C runtime\n");
    printf("CRT: argc=%d argv0=%s\n", argc, argc > 0 ? argv[0] : "(null)");
    for (int i = 0; i < 3; i++)
        printf("CRT: row %d dec=%d hex=0x%x pad=%04d\n", i, i * 7, i * 7, i * 7);
    printf("CRT: str=%.4s width=[%8s] neg=%d\n", "truncated", "hi", -42);
    fprintf(stdout, "CRT: fprintf works too\n");
    return 0;
}
