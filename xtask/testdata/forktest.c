// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: fork + exit with an atexit handler under glibc (static or dynamic). The child
// must run the inherited handler and exit with its status; the parent must see a normal exit.
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static void bye(void) { write(1, "atexit-ran\n", 11); }

int main(void) {
    atexit(bye);
    pid_t p = fork();
    if (p == 0) exit(3);
    int st = 0;
    waitpid(p, &st, 0);
    int ok = WIFEXITED(st) && WEXITSTATUS(st) == 3;
    printf(ok ? "fork ok: child exited with 3\n" : "fork FAIL: status=%#x signaled=%d\n", st, WIFSIGNALED(st));
    fflush(stdout);
    return !ok;
}
