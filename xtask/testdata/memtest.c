// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: address-space integrity. Many mmap / munmap / mprotect / fork rounds with a
// per-page pattern that is verified after every step. A frame handed out twice (aliasing), freed
// while still mapped, or copied wrongly by fork shows up as a pattern mismatch.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#define PAGES 64
#define PS 4096

static unsigned char *region[PAGES];
static unsigned tag[PAGES];

static void fill(int i, unsigned t) {
    for (int j = 0; j < PS; j += 8) *(unsigned long *)(region[i] + j) = ((unsigned long)t << 32) | (unsigned)(j * 2654435761u + i);
    tag[i] = t;
}

static int check(int i) {
    for (int j = 0; j < PS; j += 8)
        if (*(unsigned long *)(region[i] + j) != (((unsigned long)tag[i] << 32) | (unsigned)(j * 2654435761u + i))) return 0;
    return 1;
}

static int check_all(const char *when) {
    for (int i = 0; i < PAGES; i++)
        if (region[i] && !check(i)) { printf("mem FAIL: page %d corrupted %s\n", i, when); return 0; }
    return 1;
}

int main(void) {
    unsigned t = 1;
    srand(12345);
    for (int round = 0; round < 120; round++) {
        for (int i = 0; i < PAGES; i++) {
            int act = rand() % 4;
            if (!region[i] && act != 3) {
                region[i] = mmap(0, PS, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
                if (region[i] == MAP_FAILED) { puts("mem FAIL: mmap"); return 1; }
                fill(i, t++);
            } else if (region[i] && act == 0) {
                if (!check(i)) { printf("mem FAIL: page %d bad before munmap\n", i); return 1; }
                munmap(region[i], PS);
                region[i] = 0;
            } else if (region[i] && act == 1) {
                mprotect(region[i], PS, PROT_READ);          // read-only, content must survive
                if (!check(i)) { printf("mem FAIL: page %d bad after mprotect(R)\n", i); return 1; }
                mprotect(region[i], PS, PROT_READ | PROT_WRITE);
            }
        }
        if (!check_all("after the round")) return 1;
        if (round % 10 == 0) {
            pid_t p = fork();
            if (p == 0) {                                   // the child sees the same data, then churns on its own
                if (!check_all("in the fork child")) _exit(2);
                for (int i = 0; i < PAGES; i += 3) if (region[i]) { munmap(region[i], PS); region[i] = 0; }
                for (int i = 0; i < PAGES; i++) if (!region[i]) { region[i] = mmap(0, PS, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0); fill(i, 900000 + i); }
                _exit(check_all("in the child after churn") ? 0 : 3);
            }
            int st = 0;
            waitpid(p, &st, 0);
            if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) { printf("mem FAIL: fork child status %#x\n", st); return 1; }
            if (!check_all("in the parent after the child's churn")) return 1;   // the child must not touch the parent
        }
    }
    puts("mem ok: 120 rounds of mmap/munmap/mprotect with fork, patterns intact");
    return 0;
}
