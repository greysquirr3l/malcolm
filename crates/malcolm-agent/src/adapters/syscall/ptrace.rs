//! `ptrace`-based syscall interception backend.
//!
//! Two attach paths, both converging on the same [`supervise`] loop:
//!
//! - [`Supervisor::spawn_under_supervision`] — the adapter forks and execs
//!   the target itself with `PTRACE_TRACEME` armed in the pre-exec hook.
//!   This is the preferred mode: the blast radius is a single child this
//!   process already owns.
//! - [`Supervisor::attach`] — `PTRACE_SEIZE` + `PTRACE_INTERRUPT` onto an
//!   already-running, allowlisted pid. Slower and more invasive; gated
//!   behind [`super::SyscallAdapter`]'s explicit attach-mode opt-in.
//!
//! # Mechanism
//!
//! `PTRACE_SYSCALL` stops the tracee twice per syscall: once on entry
//! (registers loaded, syscall not yet executed) and once on exit
//! (registers hold the return value). [`supervise`] tracks that parity
//! itself — the kernel does not label which is which beyond alternating.
//!
//! To fail a syscall, the entry-stop handler overwrites `orig_rax` with
//! `-1` (not a valid syscall number), so the kernel skips the real
//! syscall entirely and reports `-ENOSYS` at the exit-stop; the exit-stop
//! handler then overwrites `rax` with the caller-requested `-errno`. The
//! target never actually performs the syscall — matching real failure
//! semantics (a real `ENOSPC` from `write` means no bytes landed).
//!
//! To delay a syscall, the entry-stop handler sleeps before continuing,
//! which delays when the real syscall executes (and thus when it
//! returns) without touching any register.
//!
//! # Seccomp-unotify follow-up
//!
//! The T37 spec's preferred path for spawn-under-supervision is a
//! seccomp user-notification filter rather than `ptrace`. That path
//! needs either `libseccomp` (an unvetted new C dependency) or a
//! hand-rolled BPF program plus `SCM_RIGHTS` fd-passing implemented from
//! scratch — both a materially larger blast radius of new `unsafe` for
//! this crate than the `ptrace` path, which is fully covered by nix's
//! existing safe wrappers. `ptrace` is an explicitly sanctioned
//! alternative per the spec ("... via the `seccompiler`/`libseccomp` +
//! a supervisor) or `ptrace` via `nix`"). Seccomp-unotify remains a
//! tracked follow-up (see `crates/malcolm-agent/README.md`).

use std::os::raw::c_long;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::ptrace::{self, Options};
use nix::sys::signal::Signal;
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::error::AgentError;

use super::SyscallAdapter;

/// Offset of `orig_rax` in the `x86_64` user area: the 15th register
/// (after `r15`, `r14`, `r13`, `r12`, `rbp`, `rbx`, `r11`, `r10`,
/// `r9`, `r8`, `rax`, `rcx`, `rdx`, `rsi`, `rdi`) at 8 bytes per
/// register.
const ORIG_RAX_OFFSET: *mut std::ffi::c_void =
    (15 * std::mem::size_of::<u64>()) as *mut std::ffi::c_void;
/// Offset of `rax` in the `x86_64` user area: the 10th register.
const RAX_OFFSET: *mut std::ffi::c_void =
    (10 * std::mem::size_of::<u64>()) as *mut std::ffi::c_void;

/// What the supervisor does to a matching syscall.
#[derive(Debug, Clone, Copy)]
pub(super) enum InjectKind {
    /// Skip the real syscall and report `-errno` to the caller.
    FailWith {
        /// Positive errno value (e.g. `28` for `ENOSPC`); negated when
        /// written into the return register.
        errno: i32,
    },
    /// Let the real syscall execute, but sleep `duration` before letting
    /// it start.
    Delay {
        /// How long to hold the tracee at the syscall-entry stop.
        duration: Duration,
    },
}

/// Fully-resolved instruction for [`supervise`]. Resolving the
/// [`super::table::SyscallSelector`] to a raw number happens in
/// `SyscallAdapter::apply` so an unknown name surfaces as
/// `AgentError::InvalidPlan` before any process is spawned or attached.
#[derive(Debug, Clone)]
pub(super) struct InjectSpec {
    /// Raw `x86_64` syscall number to match against `orig_rax`.
    pub(super) syscall_nr: i64,
    /// Human-readable label for tracing events (e.g. `"write(1)"`).
    pub(super) syscall_label: String,
    /// The effect to apply on a match.
    pub(super) effect: InjectKind,
    /// Per-match injection probability in `[0.0, 1.0]`, validated by the
    /// caller before this reaches the supervisor thread.
    pub(super) probability: f32,
    /// Seed for the deterministic `StdRng` that drives `probability`
    /// sampling, so a fixed seed reproduces the same inject/skip
    /// sequence across runs (real OS scheduling jitter aside).
    pub(super) seed: u64,
}

/// A live `ptrace` supervisor for one applied fault. Owns the background
/// thread that runs the syscall-entry/exit-stop loop and the pid it
/// traces.
#[derive(Debug)]
pub(super) struct Supervisor {
    pid: u32,
    detach: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<(), AgentError>>>,
}

impl Supervisor {
    /// Pid of the traced process (the spawned child, or the attached
    /// target).
    pub(super) const fn pid(&self) -> u32 {
        self.pid
    }

    /// Spawn `command` as a child of this process with `PTRACE_TRACEME`
    /// armed in its pre-exec hook, wait for the post-`execve` trap, arm
    /// `PTRACE_O_TRACESYSGOOD | PTRACE_O_EXITKILL`, and enter the
    /// supervise loop — all on the dedicated supervisor thread (see
    /// [`Self::start`] for why).
    ///
    /// `PTRACE_O_EXITKILL` means the kernel kills the child if this
    /// process dies before reverting — a second, kernel-enforced
    /// dead-man switch on top of the `Cleanup` registry's `Drop` path.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::AdapterFailure`] if the child fails to
    /// spawn, the post-`execve` handshake doesn't produce the expected
    /// `SIGTRAP` stop, or any `ptrace` call fails.
    pub(super) fn spawn_under_supervision(
        command: &[String],
        spec: InjectSpec,
    ) -> Result<Self, AgentError> {
        let (program, rest) = command
            .split_first()
            .ok_or_else(|| AgentError::InvalidPlan {
                adapter: SyscallAdapter::KIND,
                reason: "spawn target requires a non-empty command".to_owned(),
            })?;
        let program = program.clone();
        let args = rest.to_vec();
        Self::start(spec, move || spawn_handshake(&program, &args))
    }

    /// Attach to an already-running pid via `PTRACE_SEIZE` +
    /// `PTRACE_INTERRUPT`, then enter the supervise loop — all on the
    /// dedicated supervisor thread (see [`Self::start`] for why).
    ///
    /// The caller (`SyscallAdapter::apply`) is responsible for running
    /// `pid` through `SafetyGuard::check_target` and the adapter's
    /// explicit attach-mode opt-in *before* calling this — this function
    /// performs no safety checks of its own.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::AdapterFailure`] if the seize, interrupt, or
    /// the resulting wait fails.
    pub(super) fn attach(pid: u32, spec: InjectSpec) -> Result<Self, AgentError> {
        Self::start(spec, move || attach_handshake(pid))
    }

    /// Start the dedicated supervisor thread: it runs `handshake` (the
    /// spawn-or-attach-specific `ptrace` setup) and, on success, falls
    /// straight into [`supervise`] without ever returning to this
    /// caller's thread.
    ///
    /// This single-thread-for-the-whole-lifetime shape is required, not
    /// a style choice: `ptrace` scopes the tracer to the *thread* that
    /// issued `PTRACE_TRACEME`/`PTRACE_SEIZE`, not the process. An
    /// earlier version of this code ran the handshake on the caller's
    /// thread and only handed the pid to a fresh thread for the
    /// supervise loop; every `ptrace` call from that second thread
    /// failed with `ESRCH` because the kernel did not recognise it as
    /// the tracee's tracer.
    ///
    /// `handshake`'s success/failure is reported back through a
    /// one-shot channel so this function can return a `Result`
    /// synchronously, matching `TargetAdapter::apply`'s contract.
    fn start<F>(spec: InjectSpec, handshake: F) -> Result<Self, AgentError>
    where
        F: FnOnce() -> Result<(u32, Pid), AgentError> + Send + 'static,
    {
        let detach = Arc::new(AtomicBool::new(false));
        let thread_detach = Arc::clone(&detach);
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("malcolm-syscall".to_owned())
            .spawn(move || match handshake() {
                Ok((pid, npid)) => {
                    let _ = tx.send(Ok(pid));
                    supervise(npid, &spec, &thread_detach)
                }
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    Err(e)
                }
            })
            .map_err(|e| adapter_failure(format!("failed to start supervisor thread: {e}")))?;
        match rx.recv() {
            Ok(Ok(pid)) => Ok(Self {
                pid,
                detach,
                handle: Some(handle),
            }),
            Ok(Err(reason)) => {
                let _ = handle.join();
                Err(adapter_failure(reason))
            }
            Err(_) => {
                let _ = handle.join();
                Err(adapter_failure(
                    "supervisor thread exited before reporting a handshake result".to_owned(),
                ))
            }
        }
    }

    /// Stop the supervisor: request detach and join the background
    /// thread. The tracee is left running unimpeded — this never kills
    /// a process, spawned or attached.
    ///
    /// The detach request is a shared flag only, checked by the
    /// supervisor thread each time it wakes from a stop; every `ptrace`
    /// call for a tracee must come from the single thread that attached
    /// to it (see [`Self::start`]), so this method — running on
    /// whichever thread calls `revert` — cannot itself issue a
    /// `ptrace` request to nudge a prompt stop. For a target that is
    /// blocked inside one long-running syscall (e.g. a multi-second
    /// `sleep`) rather than looping through short ones, detach is
    /// deferred until that syscall returns — a documented limitation,
    /// not a hang.
    ///
    /// The real dead-man switch for a crash of *this* process (not just
    /// a missed `stop()` call) is the kernel-enforced
    /// `PTRACE_O_EXITKILL` option set at spawn time: if this process
    /// dies outright, the kernel kills any spawned-under-supervision
    /// child itself, with no code of ours needing to run.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::AdapterFailure`] if the supervisor thread's
    /// own loop returned an error or panicked.
    pub(super) fn stop(mut self) -> Result<(), AgentError> {
        self.detach.store(true, Ordering::SeqCst);
        match self.handle.take() {
            Some(handle) => handle
                .join()
                .unwrap_or_else(|_| Err(adapter_failure("supervisor thread panicked".to_owned()))),
            None => Ok(()),
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // Fallback: if `stop` was never called (this `Supervisor`
        // dropped directly, bypassing `SyscallAdapter::revert`), still
        // request detach so the tracee is not left waiting on a
        // continue that will never come. A no-op if `stop` already ran
        // — `handle` is `None` by then.
        if let Some(handle) = self.handle.take() {
            self.detach.store(true, Ordering::SeqCst);
            let _ = handle.join();
        }
    }
}

/// The syscall-entry/exit-stop loop shared by both attach paths. Runs on
/// its own thread for the lifetime of one applied fault; returns once
/// the tracee exits, is killed, or `detach` is observed.
fn supervise(pid: Pid, spec: &InjectSpec, detach: &AtomicBool) -> Result<(), AgentError> {
    let mut rng = StdRng::seed_from_u64(spec.seed);
    // `PTRACE_SYSCALL` stops alternate entry/exit for a given syscall;
    // the kernel does not label which is which, so track the parity
    // ourselves starting from the first continue below (always an
    // entry).
    let mut entering = true;
    let mut inject_current = false;

    ptrace::syscall(pid, None)
        .map_err(|e| adapter_failure(format!("initial PTRACE_SYSCALL continue failed: {e}")))?;

    loop {
        let status = match waitpid(pid, None) {
            Ok(s) => s,
            Err(Errno::ECHILD) => return Ok(()),
            Err(e) => return Err(adapter_failure(format!("waitpid failed: {e}"))),
        };

        if detach.load(Ordering::SeqCst) {
            // Best-effort: the tracee may have exited independently
            // between the flag being set and this check, in which case
            // `detach` fails harmlessly with ESRCH.
            let _: Result<(), Errno> = ptrace::detach(pid, None);
            return Ok(());
        }

        match status {
            WaitStatus::Exited(..) | WaitStatus::Signaled(..) => return Ok(()),
            WaitStatus::PtraceSyscall(_) => {
                if entering {
                    inject_current = handle_syscall_entry(pid, spec, &mut rng)?;
                    entering = false;
                } else {
                    if inject_current {
                        handle_syscall_exit(pid, spec.effect)?;
                    }
                    inject_current = false;
                    entering = true;
                }
                ptrace::syscall(pid, None)
                    .map_err(|e| adapter_failure(format!("PTRACE_SYSCALL continue failed: {e}")))?;
            }
            WaitStatus::Stopped(_, sig) if sig != Signal::SIGTRAP => {
                // A genuine signal-delivery-stop, not a syscall-stop.
                // Forward the signal so the tracee still observes it and
                // keep tracing; this does not consume the entry/exit
                // parity above.
                ptrace::syscall(pid, Some(sig)).map_err(|e| {
                    adapter_failure(format!("PTRACE_SYSCALL (signal forward) failed: {e}"))
                })?;
            }
            _ => {
                // `PtraceEvent` (e.g. our own `PTRACE_INTERRUPT` nudge)
                // or a bare SIGTRAP stop unrelated to a syscall: nothing
                // to inject, keep tracing.
                ptrace::syscall(pid, None)
                    .map_err(|e| adapter_failure(format!("PTRACE_SYSCALL continue failed: {e}")))?;
            }
        }
    }
}

/// Handle a syscall-entry stop: read `orig_rax`, decide (via the seeded
/// RNG) whether this occurrence should be injected, and — for
/// `FailWith` — skip the real syscall by overwriting `orig_rax` with an
/// invalid number so the kernel reports `-ENOSYS` at the exit-stop
/// without performing the syscall. Returns whether the matching
/// exit-stop should also be handled.
fn handle_syscall_entry(pid: Pid, spec: &InjectSpec, rng: &mut StdRng) -> Result<bool, AgentError> {
    // Read `orig_rax` from the tracee's user area. We use
    // `PTRACE_PEEKUSER` (via `ptrace::read_user`) rather than
    // `PTRACE_GETREGS` because the latter has been observed to
    // return `EIO` on every syscall stop in some x86_64 emulation
    // environments (including Docker Desktop on Apple Silicon)
    // even when the tracee is correctly stopped. `PEEKUSER` is
    // the lower-level read-from-user-area request and behaves
    // reliably in every ptrace-capable kernel we target.
    let orig_rax = match ptrace::read_user(pid, ORIG_RAX_OFFSET) {
        Ok(v) => v,
        Err(Errno::EIO | Errno::ESRCH) => {
            // Tracee exited or was killed between `waitpid`
            // returning and our read; the injection is moot and
            // the supervisor should shut down cleanly.
            return Ok(false);
        }
        Err(e) => {
            return Err(adapter_failure(format!(
                "PTRACE_PEEKUSER (orig_rax) failed: {e}"
            )));
        }
    };
    // `read_user` returns `c_long`, which is `i64` on `x86_64`.
    // Syscall numbers fit in `i64` by ABI, so the conversion is
    // direct — no truncation or sign-flipping possible.
    let nr = orig_rax;
    let matched = nr == spec.syscall_nr;
    let inject = matched && rng.random_bool(f64::from(spec.probability));
    if !inject {
        return Ok(false);
    }
    match spec.effect {
        InjectKind::FailWith { .. } => {
            // Not a valid syscall number: the kernel skips
            // execution entirely and reports `-ENOSYS` at the
            // exit-stop, which `handle_syscall_exit` then
            // overwrites with the requested errno. The real
            // syscall never runs.
            // `u64::MAX as c_long` is a deliberate bit-pattern
            // reinterpretation (all-ones → `-1` as `i64` on
            // `x86_64`). The kernel reads the bits as a signed
            // value; we want `-1` so the syscall is rejected as
            // invalid. The clippy lint is overly conservative
            // for this specific ABI case.
            #[allow(
                clippy::cast_possible_wrap,
                reason = "deliberate bit-pattern reinterpretation for the syscall ABI"
            )]
            const INVALID_SYSCALL_NR: c_long = u64::MAX as c_long;
            if let Err(e) = ptrace::write_user(pid, ORIG_RAX_OFFSET, INVALID_SYSCALL_NR) {
                return Err(adapter_failure(format!(
                    "PTRACE_POKEUSER (skip syscall) failed: {e}"
                )));
            }
            tracing::info!(
                target: "malcolm_agent::syscall",
                fault_type = "syscall_fail",
                syscall = %spec.syscall_label,
                pid = pid.as_raw(),
                dry_run = false,
                "syscall adapter: skipping syscall to inject failure"
            );
        }
        InjectKind::Delay { duration } => {
            tracing::info!(
                target: "malcolm_agent::syscall",
                fault_type = "syscall_delay",
                syscall = %spec.syscall_label,
                pid = pid.as_raw(),
                delay_ms = duration_ms(duration),
                dry_run = false,
                "syscall adapter: delaying syscall entry"
            );
            std::thread::sleep(duration);
        }
    }
    Ok(true)
}

/// Handle the exit-stop of a syscall previously marked for injection by
/// [`handle_syscall_entry`]. Only `FailWith` needs action here: overwrite
/// `rax` (the return register at exit) with `-errno`.
fn handle_syscall_exit(pid: Pid, effect: InjectKind) -> Result<(), AgentError> {
    let InjectKind::FailWith { errno } = effect else {
        return Ok(());
    };
    // The tracee can exit or be killed between the entry-stop and
    // this exit-stop (e.g. a short script finishes its loop and
    // exits while the supervisor is still mid-iteration). Writing
    // the register area then returns `EIO` or `ESRCH`; treat
    // that as "the injection is moot" and return `Ok` so the
    // supervisor thread can shut down cleanly instead of
    // bubbling an error that fails the whole `revert` call.
    // We use `PTRACE_POKEUSER` here for the same reason as
    // `handle_syscall_entry`: it is the lower-level request and
    // is reliable in every ptrace-capable kernel we target,
    // including Docker Desktop's x86_64 emulation where
    // `PTRACE_GETREGS`/`PTRACE_SETREGS` return `EIO` on every
    // syscall stop.
    // `negate_errno` returns the two's-complement bit pattern of
    // `-errno` as `u64`; reinterpreted as `c_long` (`i64` on
    // `x86_64`) it is exactly what the kernel's syscall ABI wants
    // in `rax` for a failed call. This is a deliberate bit
    // reinterpretation, not a value conversion.
    #[allow(
        clippy::cast_possible_wrap,
        reason = "deliberate bit-pattern reinterpretation for the syscall ABI"
    )]
    let neg_errno: c_long = negate_errno(errno) as c_long;
    // The `Ok` and tracee-gone arms both mean "the injection is
    // in effect or moot, either way nothing further to do" —
    // merge them so clippy doesn't flag the identical bodies.
    match ptrace::write_user(pid, RAX_OFFSET, neg_errno) {
        Ok(()) | Err(Errno::EIO | Errno::ESRCH) => Ok(()),
        Err(e) => Err(adapter_failure(format!(
            "PTRACE_POKEUSER (errno) failed: {e}"
        ))),
    }
}

/// Two's-complement negation of a positive errno into the `u64` bit
/// pattern the `x86_64` syscall ABI expects in `rax` for a failed call
/// (e.g. `ENOSPC` = 28 becomes `0xffff...ffe4`, i.e. -28). This is a
/// deliberate bit-pattern reinterpretation, not a value-preserving
/// conversion, hence the explicit lint allow rather than `try_from`.
#[allow(
    clippy::cast_sign_loss,
    reason = "rax holds -errno as a two's-complement u64 bit pattern; this is the ABI, not a value cast"
)]
fn negate_errno(errno: i32) -> u64 {
    let widened = i64::from(errno);
    (-widened) as u64
}

/// Milliseconds in `d`, saturating at `u64::MAX` rather than panicking
/// or wrapping for implausibly large delays.
fn duration_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Spawn `program args` with `PTRACE_TRACEME` armed in its pre-exec
/// hook, wait for the post-`execve` trap, and arm
/// `PTRACE_O_TRACESYSGOOD | PTRACE_O_EXITKILL`. Must run on the thread
/// that will go on to call [`supervise`] — see [`Supervisor::start`].
fn spawn_handshake(program: &str, args: &[String]) -> Result<(u32, Pid), AgentError> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    // SAFETY: the closure calls only `ptrace::traceme`, which issues
    // exactly one `ptrace(2)` syscall via nix's safe wrapper and
    // allocates nothing — async-signal-safe, as `pre_exec`'s contract
    // requires for code that runs between `fork` and `execve` in the
    // child.
    #[allow(
        unsafe_code,
        reason = "CommandExt::pre_exec is unsafe by contract (the closure must be \
                   async-signal-safe); the crate-level deny is scoped to this one call"
    )]
    unsafe {
        cmd.pre_exec(|| {
            ptrace::traceme().map_err(|errno| std::io::Error::from_raw_os_error(errno as i32))
        });
    }
    let child = cmd.spawn().map_err(|e| {
        adapter_failure(format!("failed to spawn supervised child `{program}`: {e}"))
    })?;
    let pid = child.id();
    // This thread is the sole waiter on this pid via
    // `nix::sys::wait::waitpid`; dropping `child` here (rather than
    // calling `Child::wait`) avoids a second, racing waiter. `Child`
    // inherits stdio by default, so it owns no fds that need closing.
    drop(child);
    let npid = to_pid(pid)?;

    match waitpid(npid, None) {
        Ok(WaitStatus::Stopped(_, Signal::SIGTRAP)) => {}
        Ok(other) => {
            return Err(adapter_failure(format!(
                "unexpected wait status for supervised child's post-execve trap: {other:?}"
            )));
        }
        Err(e) => {
            return Err(adapter_failure(format!(
                "waitpid for supervised child's post-execve trap failed: {e}"
            )));
        }
    }
    ptrace::setoptions(
        npid,
        Options::PTRACE_O_TRACESYSGOOD | Options::PTRACE_O_EXITKILL,
    )
    .map_err(|e| adapter_failure(format!("PTRACE_SETOPTIONS failed: {e}")))?;

    Ok((pid, npid))
}

/// Attach to `pid` via `PTRACE_SEIZE` + `PTRACE_INTERRUPT` and wait for
/// the resulting stop. Must run on the thread that will go on to call
/// [`supervise`] — see [`Supervisor::start`].
fn attach_handshake(pid: u32) -> Result<(u32, Pid), AgentError> {
    let npid = to_pid(pid)?;
    ptrace::seize(npid, Options::PTRACE_O_TRACESYSGOOD)
        .map_err(|e| adapter_failure(format!("PTRACE_SEIZE of pid {pid} failed: {e}")))?;
    ptrace::interrupt(npid)
        .map_err(|e| adapter_failure(format!("PTRACE_INTERRUPT of pid {pid} failed: {e}")))?;
    // A seized process is not already stopped, so the interrupt's
    // resulting stop can surface as a plain signal-stop or a
    // ptrace-event-stop depending on what the tracee was doing; either
    // confirms it is now stopped and safe to hand to `supervise`.
    match waitpid(npid, None) {
        Ok(
            WaitStatus::PtraceEvent(..) | WaitStatus::Stopped(..) | WaitStatus::PtraceSyscall(_),
        ) => {}
        Ok(other) => {
            return Err(adapter_failure(format!(
                "unexpected wait status while seizing pid {pid}: {other:?}"
            )));
        }
        Err(e) => {
            return Err(adapter_failure(format!(
                "waitpid while seizing pid {pid} failed: {e}"
            )));
        }
    }
    Ok((pid, npid))
}

/// Convert a `u32` pid to a `nix::unistd::Pid`.
fn to_pid(pid: u32) -> Result<Pid, AgentError> {
    i32::try_from(pid)
        .map(Pid::from_raw)
        .map_err(|_| adapter_failure(format!("pid {pid} does not fit in i32 on this platform")))
}

fn adapter_failure(reason: String) -> AgentError {
    AgentError::AdapterFailure {
        adapter: SyscallAdapter::KIND,
        reason,
    }
}
