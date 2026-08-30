# SPDX-License-Identifier: GPL-2.0-or-later
# THOS execve target.  Prints and exits 7.
    .set SYS_write, 1
    .set SYS_exit, 60
    .section .text
    .global _start
_start:
    mov     $SYS_write, %eax
    mov     $1, %edi
    lea     msg(%rip), %rsi
    mov     $10, %edx
    syscall
    mov     $7, %edi
    mov     $SYS_exit, %eax
    syscall
    .section .rodata
msg:    .ascii "child ran\n"
