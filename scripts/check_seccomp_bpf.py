#!/usr/bin/env python3
"""Independent check of the product's seccomp BPF program (M7.2e review).

A second implementation, written separately from the Rust interpreter in
`src/sandbox/seccomp.rs`: it simulates the classic-BPF program that the Rust test
`dump_the_program_for_the_independent_check` writes, for every syscall number and
architecture value of interest, against a SPECIFICATION stated here in plain
terms. It also re-derives the jump targets. Usage:

    M72E_DUMP_BPF=/tmp/prog.txt cargo test --test seccomp_isolation dump_the_program
    python3 scripts/check_seccomp_bpf.py /tmp/prog.txt
"""
import sys

AUDIT_X86_64, AUDIT_I386, AUDIT_AARCH64 = 0xC000003E, 0x40000003, 0xC00000B7
KILL, ALLOW, EACCES = 0x80000000, 0x7FFF0000, 0x00050000 | 13
X32 = 0x40000000
# x86_64 numbers (arch/x86/entry/syscalls/syscall_64.tbl)
DENIED = {"socket": 41, "ptrace": 101, "process_vm_readv": 310, "process_vm_writev": 311,
          "io_uring_setup": 425, "pidfd_getfd": 438}


def load(path):
    prog = []
    for line in open(path):
        code, jt, jf, k = (int(x) for x in line.split())
        prog.append((code, jt, jf, k))
    return prog


def simulate(prog, nr, arch):
    """Classic BPF, only what the kernel allows in seccomp: LD W ABS, JEQ/JGE K, RET K."""
    a, pc, steps = 0, 0, 0
    while True:
        steps += 1
        assert steps < 1000 and 0 <= pc < len(prog), "ran off the program"
        code, jt, jf, k = prog[pc]
        pc += 1
        cls = code & 0x07
        if cls == 0x00:                     # BPF_LD
            assert code == 0x20, hex(code)  # W | ABS
            a = {0: nr, 4: arch}[k]         # seccomp_data: nr at 0, arch at 4
        elif cls == 0x05:                   # BPF_JMP
            op = code & 0xF0
            cond = (a == k) if op == 0x10 else (a >= k) if op == 0x30 else None
            assert cond is not None and code & 0x08 == 0, hex(code)
            pc += jt if cond else jf
        elif cls == 0x06:                   # BPF_RET
            assert code == 0x06
            return k
        else:
            raise AssertionError(f"opcode {code:#x}")


def spec(nr, arch):
    if arch != AUDIT_X86_64:
        return KILL
    if nr >= X32:
        return KILL
    if nr in DENIED.values():
        return EACCES
    return ALLOW


def main(path):
    prog = load(path)
    print(f"{len(prog)} instructions")
    # structure: every jump lands inside the program, forward only
    for i, (code, jt, jf, k) in enumerate(prog):
        if code & 0x07 == 0x05:
            for off in (jt, jf):
                assert 0 <= i + 1 + off < len(prog), f"jump out of range at {i}"
    assert prog[-1][0] == 0x06 and prog[-2][0] == 0x06, "program must end in two RETs"
    bad = 0
    arches = [AUDIT_X86_64, AUDIT_I386, AUDIT_AARCH64, 0, 0xFFFFFFFF, AUDIT_X86_64 | 1, 0x80000000 | AUDIT_X86_64]
    numbers = list(range(0, 1100)) + [X32 - 1, X32, X32 + 1, X32 + 41, X32 + 425, 0x7FFFFFFF,
                                      0x80000000, 0xFFFFFFFE, 0xFFFFFFFF]
    for arch in arches:
        for nr in numbers:
            got, want = simulate(prog, nr, arch), spec(nr, arch)
            if got != want:
                bad += 1
                print(f"MISMATCH arch={arch:#x} nr={nr:#x}: program {got:#x}, spec {want:#x}")
    n = len(arches) * len(numbers)
    print(f"{n} (arch, number) pairs compared against the specification: {bad} mismatches")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
