// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: a dynamically linked, position-independent glibc program. The kernel must load
// it at a PIE base, load /lib64/ld-linux-x86-64.so.2 as its interpreter, and let ld.so map
// libc.so.6 with file-backed mmap; then libc initialisation, malloc (brk + mmap), stdio and
// qsort must all work.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int cmp(const void *a, const void *b) { return *(const int *)a - *(const int *)b; }

int main(int argc, char **argv) {
    int v[5] = {5, 2, 9, 1, 7};
    qsort(v, 5, sizeof v[0], cmp);
    char *big = malloc(300000);          // above the mmap threshold
    char *small = malloc(100);
    if (!big || !small) { puts("dyn FAIL: malloc"); return 1; }
    memset(big, 'x', 300000);
    strcpy(small, "heap");
    int ok = v[0] == 1 && v[2] == 5 && v[4] == 9 && big[299999] == 'x' && !strcmp(small, "heap");
    free(big);
    free(small);
    printf(ok ? "dyn ok: sorted=%d,%d,%d argc=%d\n" : "dyn FAIL: sorted=%d,%d,%d argc=%d\n", v[0], v[2], v[4], argc);
    return !ok;
}
