/* SPDX-License-Identifier: GPL-2.0-or-later
 * THOS kernel linker script — Limine, x86-64, higher-half at -2GB.
 */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(kmain)

PHDRS
{
    text    PT_LOAD FLAGS((1 << 0) | (1 << 2)) ;          /* R X */
    rodata  PT_LOAD FLAGS((1 << 2)) ;                      /* R   */
    data    PT_LOAD FLAGS((1 << 1) | (1 << 2)) ;          /* R W */
}

SECTIONS
{
    . = 0xffffffff80000000;
    __kernel_start = .;

    __text_start = .;
    .text : {
        *(.text .text.*)
    } :text
    . = ALIGN(CONSTANT(MAXPAGESIZE));
    __text_end = .;

    __rodata_start = .;
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    /* Limine scans this section for request structs. */
    .requests : {
        KEEP(*(.requests_start_marker))
        KEEP(*(.requests))
        KEEP(*(.requests_end_marker))
    } :rodata

    . = ALIGN(CONSTANT(MAXPAGESIZE));
    __rodata_end = .;

    __data_start = .;
    .data : {
        *(.data .data.*)
    } :data

    /* Relocated read-only data + GOT land in the writable segment; keep them
     * inside [__data_start, __data_end) so the VMM maps them. */
    .data.rel.ro : {
        *(.data.rel.ro .data.rel.ro.* .data.rel.ro.local)
    } :data
    .got : {
        *(.got)
    } :data
    .got.plt : {
        *(.got.plt)
    } :data

    .bss : {
        *(COMMON)
        *(.bss .bss.*)
    } :data
    . = ALIGN(CONSTANT(MAXPAGESIZE));
    __data_end = .;

    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
    }
}
