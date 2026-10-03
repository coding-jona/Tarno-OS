// SPDX-License-Identifier: GPL-2.0-or-later
// THOS test: POSIX threads on glibc — clone(CLONE_THREAD), thread stacks (mmap + mprotect),
// TLS, futex-based mutex / condition variable / join, and exit_group while another thread is
// still running.
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cond = PTHREAD_COND_INITIALIZER;
static long counter = 0;
static int done = 0;
static __thread int tls_value;

static void *worker(void *arg) {
    long id = (long)arg;
    tls_value = (int)id * 10;                 // thread-local: must stay private
    for (int i = 0; i < 1000; i++) {
        pthread_mutex_lock(&lock);
        counter++;
        pthread_mutex_unlock(&lock);
    }
    if (tls_value != id * 10) counter += 1000000;   // would show a shared TLS
    pthread_mutex_lock(&lock);
    done++;
    pthread_cond_signal(&cond);
    pthread_mutex_unlock(&lock);
    return (void *)(id * 2);
}

static void *spinner(void *arg) { (void)arg; for (;;) { } return 0; }

int main(void) {
    pthread_t t[4];
    for (long i = 0; i < 4; i++)
        if (pthread_create(&t[i], 0, worker, (void *)i)) { puts("thr FAIL: pthread_create"); return 1; }
    long sum = 0;
    for (int i = 0; i < 4; i++) {
        void *r;
        if (pthread_join(t[i], &r)) { puts("thr FAIL: pthread_join"); return 1; }
        sum += (long)r;
    }
    pthread_mutex_lock(&lock);
    while (done < 4) pthread_cond_wait(&cond, &lock);
    pthread_mutex_unlock(&lock);
    int ok = counter == 4000 && sum == 12;
    printf(ok ? "thr ok: counter=%ld sum=%ld\n" : "thr FAIL: counter=%ld sum=%ld\n", counter, sum);
    fflush(stdout);
    pthread_t s;                                // a thread that never ends: exit must still end the process
    pthread_create(&s, 0, spinner, 0);
    usleep(50000);
    return !ok;
}
