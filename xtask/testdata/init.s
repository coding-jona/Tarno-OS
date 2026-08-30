# SPDX-License-Identifier: GPL-2.0-or-later
# THOS fork / execve / wait4 test.  Static ET_EXEC, Linux x86-64 syscalls.
    .set SYS_write, 1
    .set SYS_fork, 57
    .set SYS_execve, 59
    .set SYS_exit, 60
    .set SYS_wait4, 61
    .set SYS_exit_group, 231

    .section .text
    .global _start
_start:
    mov     $SYS_fork, %eax
    syscall
    test    %rax, %rax
    jz      child

    # parent: wait4(-1, &status, 0, 0)
    sub     $16, %rsp
    mov     $-1, %rdi
    mov     %rsp, %rsi
    xor     %edx, %edx
    xor     %r10d, %r10d
    mov     $SYS_wait4, %eax
    syscall
    # write '0'+WEXITSTATUS, '\n'
    movzbl  1(%rsp), %eax
    add     $0x30, %al
    mov     %al, (%rsp)
    movb    $0x0a, 1(%rsp)
    mov     $SYS_write, %eax
    mov     $1, %edi
    mov     %rsp, %rsi
    mov     $2, %edx
    syscall
    add     $16, %rsp

    mov     $SYS_write, %eax
    mov     $1, %edi
    lea     pmsg(%rip), %rsi
    mov     $12, %edx
    syscall

    # --- open / lseek / read / write / close on a real ext2 file ---
    mov     $257, %eax              # openat
    mov     $-100, %edi             # AT_FDCWD
    lea     mpath(%rip), %rsi
    xor     %edx, %edx
    xor     %r10d, %r10d
    syscall
    mov     %rax, %r12             # fd

    mov     $8, %eax               # lseek(fd, 6, SEEK_SET)  -> skip "hello "
    mov     %r12, %rdi
    mov     $6, %esi
    xor     %edx, %edx
    syscall

    sub     $128, %rsp
    xor     %eax, %eax             # read(fd, buf, 128)
    mov     %r12, %rdi
    mov     %rsp, %rsi
    mov     $128, %edx
    syscall
    mov     %rax, %rdx            # nread

    mov     $SYS_write, %eax
    mov     $1, %edi
    mov     %rsp, %rsi
    syscall
    add     $128, %rsp

    mov     $3, %eax              # close(fd)
    mov     %r12, %rdi
    syscall

    xor     %edi, %edi
    mov     $SYS_exit_group, %eax
    syscall

child:
    lea     cpath(%rip), %rdi
    lea     cargv(%rip), %rsi
    lea     cenvp(%rip), %rdx
    mov     $SYS_execve, %eax
    syscall
    mov     $SYS_write, %eax
    mov     $1, %edi
    lea     efail(%rip), %rsi
    mov     $12, %edx
    syscall
    mov     $127, %edi
    mov     $SYS_exit, %eax
    syscall

    .section .rodata
pmsg:   .ascii "parent done\n"
efail:  .ascii "execve fail\n"
cpath:  .asciz "/child"
mpath:  .asciz "/message"

    .section .data
cargv:  .quad cpath
        .quad 0
cenvp:  .quad 0
