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

    .text : {
        *(.text .text.*)
    } :text

    . = ALIGN(CONSTANT(MAXPAGESIZE));

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

    .data : {
        *(.data .data.*)
    } :data

    .bss : {
        *(COMMON)
        *(.bss .bss.*)
    } :data

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
    }
}
