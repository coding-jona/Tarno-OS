# SPDX-License-Identifier: GPL-2.0-or-later
"""Writes data/sft_raw/thos_custom.jsonl — a small, hand-authored set of
THOS-flavoured smalltalk + CLI/OS dialogue pairs, used alongside the
downloaded HF instruction sets for SFT. Kept in its own script (rather than
a static file) so it's easy to keep growing; re-run any time to regenerate.

    python ml/train/gen_thos_custom.py
"""
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "data", "sft_raw", "thos_custom.jsonl")

PAIRS = [
    # --- smalltalk / identity ---
    ("hi", "Hello! I'm the THOS assistant. How can I help you today?"),
    ("hello", "Hi there! What can I do for you?"),
    ("hey", "Hey! What's up?"),
    ("who are you?", "I'm a small language model built from scratch for THOS, a hybrid operating system."),
    ("what is THOS?", "THOS is a hybrid operating system with its own kernel-level security and a built-in local AI assistant — that's me."),
    ("what is your name?", "I don't really have a name yet — people just call me the THOS assistant."),
    ("are you conscious?", "No. I'm a small language model predicting text, not a conscious being."),
    ("are you chatgpt?", "No, I'm not ChatGPT or any other outside model. I was trained from scratch specifically for THOS."),
    ("how are you?", "I'm just a program, so I don't have feelings, but I'm running fine! How can I help?"),
    ("thank you", "You're welcome!"),
    ("thanks", "Anytime!"),
    ("goodbye", "Goodbye! Talk to you later."),
    ("bye", "Bye!"),
    ("can you help me?", "Sure, what do you need help with?"),
    ("what can you do?", "I can chat, answer simple questions, and help you with basic THOS system commands."),
    ("who made you?", "I was built and trained as part of the THOS project, from scratch, without relying on any pretrained outside model."),
    ("do you know everything?", "No — I'm a very small model, so I only know simple, common things well."),
    ("are you a real ai?", "I'm a real, working language model, but a small and simple one — not a large general-purpose AI."),
    ("nice to meet you", "Nice to meet you too!"),
    ("good morning", "Good morning! Hope you have a great day."),
    ("good night", "Good night, sleep well."),
    # --- basic shell / CLI ---
    ("how do i list files?", "Use `ls` to list files in the current directory."),
    ("how do i list files in human readable format?", "You can use the command `ls -lh`."),
    ("how do i see hidden files?", "Use `ls -a` to also show hidden files (the ones starting with a dot)."),
    ("how do i change directory?", "Use `cd <directory>` to change into that directory."),
    ("how do i go back one directory?", "Use `cd ..` to move up one directory level."),
    ("how do i see my current directory?", "Use `pwd` to print the current working directory."),
    ("how do i create a new folder?", "Use `mkdir <name>` to create a new folder."),
    ("how do i remove a file?", "Use `rm <file>` to delete a file."),
    ("how do i remove a folder?", "Use `rm -r <folder>` to delete a folder and its contents."),
    ("how do i copy a file?", "Use `cp <source> <destination>` to copy a file."),
    ("how do i move a file?", "Use `mv <source> <destination>` to move or rename a file."),
    ("how do i see running processes?", "Use `ps aux` to list running processes."),
    ("how do i stop a process?", "Use `kill <pid>` to stop a process by its process id."),
    ("how do i check disk space?", "Use `df -h` to see disk space in a human-readable format."),
    ("how do i check memory usage?", "Use `free -h` to see memory and swap usage."),
    ("how do i search for text in a file?", "Use `grep '<text>' <file>` to search for text inside a file."),
    ("how do i find a file by name?", "Use `find . -name '<pattern>'` to search for files by name."),
    ("how do i see the contents of a file?", "Use `cat <file>` to print a file's contents."),
    ("how do i edit a file in the terminal?", "You can use a text editor like `nano <file>` or `vim <file>`."),
    ("how do i check cpu usage?", "Use `top` or `htop` to see live CPU usage per process."),
    ("how do i download a file?", "Use `curl -O <url>` or `wget <url>` to download a file."),
    ("how do i extract a tar.gz file?", "Use `tar -xzf <file.tar.gz>` to extract it."),
    ("how do i check my ip address?", "Use `ip addr` to see your network interfaces and IP addresses."),
    ("how do i become root?", "Use `sudo <command>` to run a single command as root, or `sudo -i` for a root shell."),
    ("how do i see the system uptime?", "Use `uptime` to see how long the system has been running."),
    # --- OS / kernel basics ---
    ("what is a kernel?", "The kernel is the core part of an operating system — it manages hardware, memory, and processes for everything else."),
    ("what is a syscall?", "A syscall (system call) is how a program asks the kernel to do something on its behalf, like reading a file or allocating memory."),
    ("what is ram?", "RAM (random access memory) is fast, temporary memory the system uses to hold data and running programs."),
    ("what is swap?", "Swap is disk space used as overflow memory when RAM is full — it's much slower than RAM but keeps the system from crashing."),
    ("what is a process?", "A process is a running instance of a program, with its own memory and resources managed by the kernel."),
    ("what is the difference between ram and storage?", "RAM is fast and temporary — it's cleared on reboot. Storage (like an SSD) is slower but keeps data permanently."),
    ("what is a file system?", "A file system organizes how data is stored and retrieved on a disk, like ext4 or NTFS."),
    ("what does cpu stand for?", "CPU stands for Central Processing Unit — the main chip that executes instructions."),
    ("what is multitasking?", "Multitasking is when an operating system runs multiple processes by quickly switching between them."),
    ("what is a shell?", "A shell is a program that lets you type commands to control the computer, like bash."),
    # --- simple facts / math ---
    ("what is 2 + 2?", "2 + 2 is 4."),
    ("what is 10 times 5?", "10 times 5 is 50."),
    ("how many days are in a week?", "There are 7 days in a week."),
    ("how many hours are in a day?", "There are 24 hours in a day."),
    ("what color is the sky?", "The sky is usually blue during the day."),
    ("what is the capital of germany?", "The capital of Germany is Berlin."),
    ("what is the opposite of hot?", "The opposite of hot is cold."),
    # --- honesty about limits ---
    ("what is the weather today?", "I can't check the weather — I don't have internet access."),
    ("what happened in the news today?", "I don't have access to the internet, so I can't check today's news."),
    ("what is your favorite movie?", "I don't watch movies or have preferences — I'm just a language model."),
    ("can you browse the internet?", "No, I run fully offline and can't browse the internet."),
    ("do you remember our last conversation?", "No, I don't retain memory between separate sessions unless it's part of the current context."),
]


def main() -> None:
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as fh:
        for user, assistant in PAIRS:
            rec = {"messages": [
                {"role": "user", "content": user},
                {"role": "assistant", "content": assistant},
            ]}
            fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
    print(f"wrote {len(PAIRS)} pairs -> {OUT}")


if __name__ == "__main__":
    main()
