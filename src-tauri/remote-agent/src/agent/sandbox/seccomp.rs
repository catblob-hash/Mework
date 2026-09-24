//! Linux: the system calls a cell and everything it starts may not make,
//! refused by a seccomp filter the cell installs on itself before it runs
//! anything. Filters are inherited across `fork` and `exec` and cannot be
//! removed, and `no_new_privs` keeps a set-uid program from shedding it.
//!
//! What is refused, with `EPERM`:
//!
//! * `socket(AF_UNIX, …)`: a Unix socket is how a process reaches the
//!   session bus, the keyring, an SSH or GPG agent, Docker — services that act
//!   outside the sandbox on the caller's behalf. The namespaces hide most of
//!   their paths; this closes the rest. `socketpair` stays: it connects a
//!   process to its own child and nothing else.
//! * `io_uring_*`: its operations open sockets and files without passing
//!   through the system calls this filter sees.
//! * `ptrace`, `process_vm_readv`/`writev`: reading or steering another
//!   process's memory.
//! * `keyctl`, `add_key`, `request_key`: the kernel keyrings, where some
//!   desktops keep credentials.
//! * `bpf`, `perf_event_open`, `userfaultfd`: kernel attack surface no build
//!   tool needs.
//!
//! Any architecture but the machine's own is killed outright — a 32-bit
//! `int 0x80` would reach `socketcall`, which carries its arguments in memory
//! a filter cannot read — and on x86-64 so is the x32 system call range.
//!
//! The filter is classic BPF written out by hand: twenty instructions are
//! easier to check than a dependency is to audit.

#[repr(C)]
#[derive(Clone, Copy)]
struct Instruction {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct Program {
    len: u16,
    filter: *const Instruction,
}

const LD_W_ABS: u16 = 0x00 | 0x00 | 0x20;
const JMP_JEQ_K: u16 = 0x05 | 0x10 | 0x00;
#[cfg(target_arch = "x86_64")]
const JMP_JGE_K: u16 = 0x05 | 0x30 | 0x00;
const RET_K: u16 = 0x06;

const RET_ALLOW: u32 = 0x7fff_0000;
const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_ERRNO: u32 = 0x0005_0000;

/// Offsets in `struct seccomp_data`.
const NR: u32 = 0;
const ARCH: u32 = 4;
/// The low half of the first argument (little-endian machines only).
const ARG0: u32 = 16;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;
const SECCOMP_FILTER_FLAG_TSYNC: libc::c_ulong = 1;

fn statement(code: u16, k: u32) -> Instruction {
    Instruction { code, jt: 0, jf: 0, k }
}

fn jump(code: u16, k: u32, jt: u8, jf: u8) -> Instruction {
    Instruction { code, jt, jf, k }
}

/// The system calls refused outright.
fn refused() -> Vec<u32> {
    [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_userfaultfd,
    ]
    .into_iter()
    .map(|number| number as u32)
    .collect()
}

fn filter() -> Vec<Instruction> {
    let eperm = RET_ERRNO | libc::EPERM as u32;
    let mut program = vec![
        statement(LD_W_ABS, ARCH),
        jump(JMP_JEQ_K, AUDIT_ARCH, 1, 0),
        statement(RET_K, RET_KILL_PROCESS),
        statement(LD_W_ABS, NR),
    ];
    #[cfg(target_arch = "x86_64")]
    {
        // x32 system calls are numbered from bit 30 up.
        program.push(jump(JMP_JGE_K, 0x4000_0000, 0, 1));
        program.push(statement(RET_K, RET_KILL_PROCESS));
    }
    let refused = refused();
    let count = refused.len();
    // Layout after the chain: socket test, load arg0, AF_UNIX test, allow,
    // refuse. A refused number jumps to the last of those.
    for (index, number) in refused.iter().enumerate() {
        let to_refuse = (count - index - 1) + 4;
        program.push(jump(JMP_JEQ_K, *number, to_refuse as u8, 0));
    }
    program.push(jump(JMP_JEQ_K, libc::SYS_socket as u32, 0, 2));
    program.push(statement(LD_W_ABS, ARG0));
    program.push(jump(JMP_JEQ_K, libc::AF_UNIX as u32, 1, 0));
    program.push(statement(RET_K, RET_ALLOW));
    program.push(statement(RET_K, eperm));
    program
}

/// Installs the filter on every thread of this process. Refuses to return
/// success without it: a cell that could not install it must not run
/// anything.
pub fn install() -> Result<(), String> {
    let instructions = filter();
    let program = Program {
        len: instructions.len() as u16,
        filter: instructions.as_ptr(),
    };
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(format!("Cannot set no_new_privs: {}", std::io::Error::last_os_error()));
        }
        let installed = libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            SECCOMP_FILTER_FLAG_TSYNC,
            &program as *const Program,
        );
        if installed != 0 {
            return Err(format!(
                "Cannot install the seccomp filter: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_jump_lands_inside_the_program_on_a_return() {
        let program = filter();
        for (index, instruction) in program.iter().enumerate() {
            if instruction.code & 0x07 == 0x05 {
                for offset in [instruction.jt, instruction.jf] {
                    let target = index + 1 + offset as usize;
                    assert!(target < program.len(), "jump from {index} leaves the program");
                }
            }
        }
        assert_eq!(program.last().unwrap().code, RET_K);
        // Every refused number reaches the final `EPERM`.
        let last = program.len() - 1;
        for (index, instruction) in program.iter().enumerate() {
            if instruction.code == JMP_JEQ_K && refused().contains(&instruction.k) {
                assert_eq!(index + 1 + instruction.jt as usize, last);
            }
        }
    }

    /// In a child process, so the test runner keeps its own sockets: after
    /// installing, a Unix socket and `ptrace` are refused and an internet
    /// socket is still allowed.
    #[test]
    fn the_filter_refuses_unix_sockets_and_ptrace() {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            let code = (|| {
                if install().is_err() {
                    return 10;
                }
                let unix = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
                if unix >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) {
                    return 11;
                }
                let inet = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
                if inet < 0 {
                    return 12;
                }
                let traced = unsafe { libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) };
                if traced >= 0 {
                    return 13;
                }
                let mut pair = [0; 2];
                if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, pair.as_mut_ptr()) } != 0 {
                    return 14;
                }
                0
            })();
            unsafe { libc::_exit(code) };
        }
        let mut status = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        assert!(libc::WIFEXITED(status), "the child was killed: {status}");
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }
}
