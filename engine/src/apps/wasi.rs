//! The slice of WASI preview 1 the template apps use, implemented against a simulated process:
//! arguments and environment from the container, stdout/stderr captured line by line, and a
//! virtual clock that sleeping moves forward instantly.
//!
//! Guest pointers are offsets into the guest's own linear memory (exported as `memory`), which
//! lives in the wasmi store, apart from the engine's memory. Every access is bounds-checked
//! by `Memory::read`/`write`; a bad pointer returns `EFAULT` to the guest instead of trapping.

use std::fmt;

use wasmi::errors::HostError;
use wasmi::{Caller, Error, Extern, Linker, Memory, Module, Val, ValType};

const MODULE: &str = "wasi_snapshot_preview1";

const ESUCCESS: i32 = 0;
const EBADF: i32 = 8;
const EFAULT: i32 = 21;
const ENOSYS: i32 = 52;
const ESPIPE: i32 = 70;

const NANOS: u64 = 1_000_000_000;
/// How far a run is simulated. A process still sleeping past this is treated as running forever,
/// which is how servers (`loop { sleep }`) end their run. Longer than any scenario warm-up.
pub const HORIZON_NANOS: u64 = 30 * 86_400 * NANOS;

/// The state of one container run.
pub struct Process {
    args: Vec<String>,
    /// `NAME=value` entries.
    env: Vec<String>,
    /// Wall-clock time the container started, epoch nanoseconds.
    started_at: u64,
    /// Simulated time since the start.
    elapsed: u64,
    /// Bytes written to stdout and stderr since their last newline.
    pending: [Vec<u8>; 2],
    /// Complete lines with the second (since start) they were written.
    pub lines: Vec<(i64, String)>,
    rng: u64,
}

/// Raised by the host to stop a process that will never exit (see `HORIZON_NANOS`).
#[derive(Debug)]
pub struct RunsForever;

impl fmt::Display for RunsForever {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("process runs forever")
    }
}

impl HostError for RunsForever {}

impl Process {
    pub fn new(args: Vec<String>, env: Vec<String>, started_at_secs: i64, seed: u64) -> Self {
        Process {
            args,
            env,
            started_at: u64::try_from(started_at_secs).unwrap_or(0) * NANOS,
            elapsed: 0,
            pending: [Vec::new(), Vec::new()],
            lines: Vec::new(),
            // xorshift needs a non-zero state.
            rng: seed | 1,
        }
    }

    /// Seconds since the start, as log offsets count them.
    pub fn elapsed_secs(&self) -> i64 {
        (self.elapsed / NANOS) as i64
    }

    /// Record what is left of a line written without a final newline (e.g. just before exit).
    pub fn flush(&mut self) {
        for stream in 0..2 {
            if !self.pending[stream].is_empty() {
                let rest = std::mem::take(&mut self.pending[stream]);
                self.push_line(&rest);
            }
        }
    }

    fn write(&mut self, stream: usize, bytes: &[u8]) {
        self.pending[stream].extend_from_slice(bytes);
        while let Some(newline) = self.pending[stream].iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending[stream].drain(..=newline).collect();
            self.push_line(&line[..newline]);
        }
    }

    fn push_line(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let text = text.strip_suffix('\r').unwrap_or(&text).to_string();
        let at = self.elapsed_secs();
        self.lines.push((at, text));
    }

    fn next_random(&mut self) -> u64 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

type Ctx<'a> = Caller<'a, Process>;

fn memory(caller: &Ctx<'_>) -> Option<Memory> {
    caller.get_export("memory").and_then(Extern::into_memory)
}

fn read(caller: &Ctx<'_>, ptr: i32, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0; len];
    memory(caller)?.read(caller, ptr as u32 as usize, &mut buf).ok()?;
    Some(buf)
}

fn write(caller: &mut Ctx<'_>, ptr: i32, bytes: &[u8]) -> bool {
    memory(caller).is_some_and(|m| m.write(&mut *caller, ptr as u32 as usize, bytes).is_ok())
}

fn read_u32(caller: &Ctx<'_>, ptr: i32) -> Option<u32> {
    read(caller, ptr, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

fn errno(ok: bool) -> i32 {
    if ok {
        ESUCCESS
    } else {
        EFAULT
    }
}

/// Write `strings` as a NUL-terminated buffer at `buf` and their pointers at `ptrs`
/// (`args_get`/`environ_get`).
fn write_strings(caller: &mut Ctx<'_>, strings: &[String], ptrs: i32, buf: i32) -> i32 {
    let mut offset = 0u32;
    for (i, s) in strings.iter().enumerate() {
        let at = (buf as u32).wrapping_add(offset);
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        if !write(caller, ptrs + 4 * i as i32, &at.to_le_bytes()) || !write(caller, at as i32, &bytes) {
            return EFAULT;
        }
        offset += bytes.len() as u32;
    }
    ESUCCESS
}

/// Write the count and total buffer size of `strings` (`args_sizes_get`/`environ_sizes_get`).
fn write_sizes(caller: &mut Ctx<'_>, strings: &[String], count_ptr: i32, size_ptr: i32) -> i32 {
    let size: usize = strings.iter().map(|s| s.len() + 1).sum();
    errno(
        write(caller, count_ptr, &(strings.len() as u32).to_le_bytes())
            && write(caller, size_ptr, &(size as u32).to_le_bytes()),
    )
}

/// The calls `linker` defines itself.
const IMPLEMENTED: &[&str] = &[
    "args_sizes_get",
    "args_get",
    "environ_sizes_get",
    "environ_get",
    "fd_write",
    "proc_exit",
    "clock_time_get",
    "poll_oneoff",
    "random_get",
    "sched_yield",
    "fd_fdstat_get",
    "fd_prestat_get",
    "fd_prestat_dir_name",
    "fd_close",
    "fd_seek",
];

/// A linker with every import `module` needs. Calls the apps should never make are answered
/// with `ENOSYS`, so a template linking more of libstd than it uses still instantiates.
pub fn linker(module: &Module) -> Result<Linker<Process>, Error> {
    let mut linker = Linker::new(module.engine());

    linker.func_wrap(MODULE, "args_sizes_get", |mut c: Ctx<'_>, count: i32, size: i32| {
        let args = std::mem::take(&mut c.data_mut().args);
        let r = write_sizes(&mut c, &args, count, size);
        c.data_mut().args = args;
        r
    })?;
    linker.func_wrap(MODULE, "args_get", |mut c: Ctx<'_>, ptrs: i32, buf: i32| {
        let args = std::mem::take(&mut c.data_mut().args);
        let r = write_strings(&mut c, &args, ptrs, buf);
        c.data_mut().args = args;
        r
    })?;
    linker.func_wrap(MODULE, "environ_sizes_get", |mut c: Ctx<'_>, count: i32, size: i32| {
        let env = std::mem::take(&mut c.data_mut().env);
        let r = write_sizes(&mut c, &env, count, size);
        c.data_mut().env = env;
        r
    })?;
    linker.func_wrap(MODULE, "environ_get", |mut c: Ctx<'_>, ptrs: i32, buf: i32| {
        let env = std::mem::take(&mut c.data_mut().env);
        let r = write_strings(&mut c, &env, ptrs, buf);
        c.data_mut().env = env;
        r
    })?;

    linker.func_wrap(
        MODULE,
        "fd_write",
        |mut c: Ctx<'_>, fd: i32, iovs: i32, iovs_len: i32, written_ptr: i32| {
            let stream = match fd {
                1 => 0,
                2 => 1,
                _ => return EBADF,
            };
            let mut total = 0u32;
            for i in 0..iovs_len {
                let iov = iovs + 8 * i;
                let (Some(buf), Some(len)) = (read_u32(&c, iov), read_u32(&c, iov + 4)) else {
                    return EFAULT;
                };
                let Some(bytes) = read(&c, buf as i32, len as usize) else {
                    return EFAULT;
                };
                c.data_mut().write(stream, &bytes);
                total += len;
            }
            errno(write(&mut c, written_ptr, &total.to_le_bytes()))
        },
    )?;

    linker.func_wrap(MODULE, "proc_exit", |_: Ctx<'_>, code: i32| -> Result<(), Error> {
        Err(Error::i32_exit(code))
    })?;

    linker.func_wrap(
        MODULE,
        "clock_time_get",
        |mut c: Ctx<'_>, clock: i32, _precision: i64, time_ptr: i32| {
            let p = c.data();
            // 0 is the realtime clock; monotonic and CPU-time clocks count from the start.
            let now = if clock == 0 {
                p.started_at + p.elapsed
            } else {
                p.elapsed
            };
            errno(write(&mut c, time_ptr, &now.to_le_bytes()))
        },
    )?;

    linker.func_wrap(
        MODULE,
        "poll_oneoff",
        |mut c: Ctx<'_>, subs: i32, events: i32, nsubs: i32, nevents_ptr: i32| -> Result<i32, Error> {
            // Subscriptions are 48 bytes: userdata u64 @0, tag u8 @8, then for a clock:
            // id u32 @16, timeout u64 @24, precision u64 @32, flags u16 @40.
            let Some(raw) = read(&c, subs, 48 * nsubs.max(0) as usize) else {
                return Ok(EFAULT);
            };
            let p = c.data();
            let wait = raw
                .as_chunks::<48>()
                .0
                .iter()
                .filter(|s| s[8] == 0)
                .map(|s| {
                    let userdata = u64::from_le_bytes(s[0..8].try_into().unwrap());
                    let clock = u32::from_le_bytes(s[16..20].try_into().unwrap());
                    let timeout = u64::from_le_bytes(s[24..32].try_into().unwrap());
                    let absolute = u16::from_le_bytes(s[40..42].try_into().unwrap()) & 1 == 1;
                    let wait = if !absolute {
                        timeout
                    } else if clock == 0 {
                        timeout.saturating_sub(p.started_at + p.elapsed)
                    } else {
                        timeout.saturating_sub(p.elapsed)
                    };
                    (wait, userdata)
                })
                .min();
            // Waiting on anything but the clock (stdin, sockets) never ends here.
            let Some((wait, userdata)) = wait else {
                return Err(Error::host(RunsForever));
            };
            if p.elapsed.saturating_add(wait) > HORIZON_NANOS {
                return Err(Error::host(RunsForever));
            }
            c.data_mut().elapsed += wait;
            // Events are 32 bytes: userdata u64 @0, error u16 @8, type u8 @10 (0 = clock).
            let mut event = [0u8; 32];
            event[0..8].copy_from_slice(&userdata.to_le_bytes());
            Ok(errno(
                write(&mut c, events, &event) && write(&mut c, nevents_ptr, &1u32.to_le_bytes()),
            ))
        },
    )?;

    linker.func_wrap(MODULE, "random_get", |mut c: Ctx<'_>, buf: i32, len: i32| {
        let bytes: Vec<u8> = (0..len.max(0) as usize)
            .map(|_| c.data_mut().next_random() as u8)
            .collect();
        errno(write(&mut c, buf, &bytes))
    })?;
    linker.func_wrap(MODULE, "sched_yield", |_: Ctx<'_>| ESUCCESS)?;

    // stdin/stdout/stderr are character devices; there are no other descriptors, and no
    // preopened directories.
    linker.func_wrap(MODULE, "fd_fdstat_get", |mut c: Ctx<'_>, fd: i32, stat: i32| {
        if !(0..=2).contains(&fd) {
            return EBADF;
        }
        let mut fdstat = [0u8; 24];
        fdstat[0] = 2; // filetype: character device
        fdstat[8..16].copy_from_slice(&u64::MAX.to_le_bytes()); // rights: everything
        errno(write(&mut c, stat, &fdstat))
    })?;
    linker.func_wrap(MODULE, "fd_prestat_get", |_: Ctx<'_>, _fd: i32, _buf: i32| EBADF)?;
    linker.func_wrap(
        MODULE,
        "fd_prestat_dir_name",
        |_: Ctx<'_>, _fd: i32, _p: i32, _l: i32| EBADF,
    )?;
    linker.func_wrap(MODULE, "fd_close", |_: Ctx<'_>, _fd: i32| EBADF)?;
    linker.func_wrap(MODULE, "fd_seek", |_: Ctx<'_>, _fd: i32, _o: i64, _w: i32, _p: i32| {
        ESPIPE
    })?;

    for import in module.imports() {
        if import.module() != MODULE || IMPLEMENTED.contains(&import.name()) {
            continue;
        }
        if let Some(ty) = import.ty().func() {
            let returns_errno = ty.results() == [ValType::I32];
            linker.func_new(MODULE, import.name(), ty.clone(), move |_, _, results| {
                if returns_errno {
                    results[0] = Val::I32(ENOSYS);
                    Ok(())
                } else {
                    Err(Error::new("unsupported WASI call"))
                }
            })?;
        }
    }
    Ok(linker)
}
