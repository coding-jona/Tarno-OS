# SPDX-License-Identifier: GPL-2.0-or-later
# THOS ELF-loader / process test. Static ET_EXEC, freestanding.
# Uses the Linux x86-64 syscall ABI (write=1, exit_group=231).
# Prints:  argv0=<argv[0]> argc=<N>
    .set SYS_write, 1
    .set SYS_exit_group, 231

    .section .text
    .global _start
_start:
    mov     (%rsp), %r12            # argc
    mov     8(%rsp), %r13           # argv[0]

    # write(1, "argv0=", 6)
    mov     $SYS_write, %eax
    mov     $1, %edi
    lea     pfx(%rip), %rsi
    mov     $6, %edx
    syscall

    # strlen(argv[0]) -> rdx
    xor     %rcx, %rcx
1:  cmpb    $0, (%r13,%rcx)
    je      2f
    inc     %rcx
    jmp     1b
2:  mov     $SYS_write, %eax
    mov     $1, %edi
    mov     %r13, %rsi
    mov     %rcx, %rdx
    syscall

    # write(1, " argc=", 6)
    mov     $SYS_write, %eax
    mov     $1, %edi
    lea     mid(%rip), %rsi
    mov     $6, %edx
    syscall

    # write(1, {'0'+argc, '\n'}, 2) using stack scratch
    sub     $16, %rsp
    mov     %r12b, %al
    add     $'0', %al
    mov     %al, (%rsp)
    movb    $'\n', 1(%rsp)
    mov     $SYS_write, %eax
    mov     $1, %edi
    mov     %rsp, %rsi
    mov     $2, %edx
    syscall
    add     $16, %rsp

    xor     %edi, %edi
    mov     $SYS_exit_group, %eax
    syscall
    hlt

    .section .rodata
pfx:    .ascii "argv0="
mid:    .ascii " argc="
