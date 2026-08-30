# SPDX-License-Identifier: GPL-2.0-or-later
# THOS ELF-loader test program. Static ET_EXEC, freestanding.
# THOS syscalls: rax=nr (1=write, 2=exit), args rdi, rsi.
    .section .text
    .global _start
_start:
    lea     msg(%rip), %rdi
    mov     $msglen, %rsi
    mov     $1, %rax
    syscall
    xor     %rdi, %rdi
    mov     $2, %rax
    syscall
    hlt

    .section .rodata
msg:
    .ascii  "hello from an ext2-loaded ELF\n"
    .equ    msglen, . - msg
