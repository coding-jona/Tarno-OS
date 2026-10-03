// SPDX-License-Identifier: GPL-2.0-or-later
// THOS desktop stage 2 demo: a userspace program draws on /dev/fb0 and a mouse-driven cursor
// follows /dev/input/mice. It starts the cursor at (400, 300), draws an orange 200x100 rectangle
// at (100, 450), then moves the magenta 12x12 cursor by every PS/2 packet it reads. The test
// harness moves the mouse through the QEMU monitor and checks the pixels of a screendump.
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

struct bitfield { uint32_t offset, length, msb_right; };
struct var_screeninfo {
    uint32_t xres, yres, xres_virtual, yres_virtual, xoffset, yoffset, bpp, grayscale;
    struct bitfield red, green, blue, transp;
    uint32_t rest[24];
};

static int fb;
static uint32_t W, H, pitch, rs, gs, bs;

static uint32_t rgb(int r, int g, int b) { return ((uint32_t)r << rs) | ((uint32_t)g << gs) | ((uint32_t)b << bs); }

static void fill_rect(int x, int y, int w, int h, uint32_t color) {
    static uint32_t row[2048];
    if (x < 0) { w += x; x = 0; }
    if (y < 0) { h += y; y = 0; }
    if (x + w > (int)W) w = W - x;
    if (y + h > (int)H) h = H - y;
    for (int i = 0; i < w; i++) row[i] = color;
    for (int j = 0; j < h; j++) {
        lseek(fb, (off_t)(y + j) * pitch + (off_t)x * 4, SEEK_SET);
        if (write(fb, row, (size_t)w * 4) < 0) return;
    }
}

int main(void) {
    fb = open("/dev/fb0", O_RDWR);
    int mice = open("/dev/input/mice", O_RDONLY);
    if (fb < 0 || mice < 0) { printf("fb FAIL: open fb=%d mice=%d\n", fb, mice); return 1; }
    struct var_screeninfo v;
    memset(&v, 0, sizeof v);
    if (ioctl(fb, 0x4600, &v) != 0 || v.bpp != 32) { puts("fb FAIL: FBIOGET_VSCREENINFO"); return 1; }
    W = v.xres; H = v.yres; rs = v.red.offset; gs = v.green.offset; bs = v.blue.offset;
    char fix[80];
    if (ioctl(fb, 0x4602, fix) != 0) { puts("fb FAIL: FBIOGET_FSCREENINFO"); return 1; }
    pitch = *(uint32_t *)(fix + 48);

    printf("fb ready %ux%u\n", W, H);
    fflush(stdout);
    sleep(1);

    // The orange rectangle is drawn through an mmap of the framebuffer (shared device memory),
    // the cursor below through write(): both paths must end up on screen.
    uint8_t *map = mmap(0, (size_t)pitch * H, PROT_READ | PROT_WRITE, MAP_SHARED, fb, 0);
    if (map == MAP_FAILED) {
        puts("fb mmap FAIL");
        fill_rect(100, 450, 200, 100, rgb(255, 136, 0));
    } else {
        for (int y = 450; y < 550; y++)
            for (int x = 100; x < 300; x++) *(uint32_t *)(map + (size_t)y * pitch + (size_t)x * 4) = rgb(255, 136, 0);
        /* "fb mmap ok" is printed at the end: a console line now could redraw the screen */
    }
    int cx = 400, cy = 300;
    fill_rect(cx, cy, 12, 12, rgb(255, 0, 255));           // the cursor, magenta
    // Run until a key is pressed on the console (the test sends one after its screenshot), so
    // nothing prints — and nothing redraws the console over the drawing — before the host looks.
    struct pollfd p[2] = { { mice, POLLIN, 0 }, { 0, POLLIN, 0 } };
    int idle = 0;
    while (idle < 600) {                                   // 60 s safety net
        if (poll(p, 2, 100) <= 0) { idle++; continue; }
        if (p[1].revents & POLLIN) { char c[16]; (void)!read(0, c, sizeof c); break; }
        idle = 0;
        unsigned char pkt[3];
        if (read(mice, pkt, 3) != 3) break;
        int dx = pkt[1] - ((pkt[0] & 0x10) ? 256 : 0);
        int dy = pkt[2] - ((pkt[0] & 0x20) ? 256 : 0);
        fill_rect(cx, cy, 12, 12, rgb(0, 0, 0));           // erase the old cursor
        cx += dx; cy -= dy;                                // PS/2 y grows upward
        if (cx < 0) cx = 0;
        if (cy < 0) cy = 0;
        if (cx > (int)W - 12) cx = W - 12;
        if (cy > (int)H - 12) cy = H - 12;
        fill_rect(cx, cy, 12, 12, rgb(255, 0, 255));
    }
    if (map != MAP_FAILED) { munmap(map, (size_t)pitch * H); puts("fb mmap ok"); }
    printf("fb ok: cursor ended at %d %d\n", cx, cy);
    return 0;
}
