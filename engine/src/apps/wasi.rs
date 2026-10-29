//! The slice of WASI preview 1 the template apps use, implemented against a simulated process:
//! arguments and environment from the container, stdout/stderr captured line by line, a
//! virtual clock that sleeping moves forward instantly, and the container's volumes as a
//! filesystem preopened at `/` (see `vfs`).
//!
//! Guest pointers are offsets into the guest's own linear memory (exported as `memory`), which
//! lives in the wasmi store, apart from the engine's memory. Every access is bounds-checked
//! by `Memory::read`/`write`; a bad pointer returns `EFAULT` to the guest instead of trapping.

use std::fmt;

use wasmi::errors::HostError;
use wasmi::{Caller, Error, Extern, Linker, Memory, Module, Val, ValType};

use super::vfs::{Fs, Mount, Timeline, TYPE_DIRECTORY};

const MODULE: &str = "wasi_snapshot_preview1";

const ESUCCESS: i32 = 0;
const EBADF: i32 = 8;
const EFAULT: i32 = 21;
const ENOSYS: i32 = 52;
const ESPIPE: i32 = 70;

/// The one preopened directory: the container's root.
const ROOT_FD: i32 = 3;
/// WASI rights bit for writing.
const RIGHT_FD_WRITE: i64 = 1 << 6;

const NANOS: u64 = 1_000_000_000;
/// How far a run is simulated. A process still sleeping past this is treated as running forever,
/// which is how servers (`loop { sleep }`) end their run. Longer than any scenario warm-up.
pub const HORIZON_NANOS: u64 = 30 * 86_400 * NANOS;
/// A service that logs this much, or changes files this often, is simulated no further: it is
/// treated as running on. (Its first hours of output are enough for any session.)
pub const MAX_LINES: usize = 3_000;
pub const MAX_WRITES: usize = 3_000;

/// The state of one container run.
pub struct Process<'a> {
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
    /// Where the app's connections go, and what they got.
    net: super::Net<'a>,
    pub calls: Vec<super::NetCall>,
    /// Ports the app declared it listens on, with the second it did.
    pub listens: Vec<(i64, u16)>,
    /// The container's volumes.
    pub fs: Fs,
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

impl<'a> Process<'a> {
    pub fn new(
        args: Vec<String>,
        env: Vec<String>,
        started_at_secs: i64,
        seed: u64,
        mounts: &[Mount],
        timeline: &Timeline,
        net: super::Net<'a>,
    ) -> Self {
        Process {
            fs: Fs::new(mounts, timeline, started_at_secs),
            args,
            env,
            started_at: u64::try_from(started_at_secs).unwrap_or(0) * NANOS,
            elapsed: 0,
            pending: [Vec::new(), Vec::new()],
            lines: Vec::new(),
            listens: Vec::new(),
            net,
            calls: Vec::new(),
            // xorshift needs a non-zero state.
            rng: seed | 1,
        }
    }

    /// One connection attempt, answered live or from the recording of an earlier run.
    fn ask(&mut self, host: &str, port: i64, method: &str, path: &str) -> super::NetReply {
        let reply = match &self.net {
            super::Net::None => super::NetReply::refused(),
            super::Net::Live(oracle) => oracle(host, port, method, path),
            super::Net::Replay(calls) => calls
                .get(self.calls.len())
                .map_or_else(super::NetReply::refused, |c| c.reply.clone()),
        };
        self.calls.push(super::NetCall {
            host: host.to_string(),
            port,
            method: method.to_string(),
            path: path.to_string(),
            reply: reply.clone(),
        });
        reply
    }

    /// The wall-clock second now, which stamps the files this run changes.
    fn now_secs(&self) -> i64 {
        ((self.started_at + self.elapsed) / NANOS) as i64
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

type Ctx<'a, 'p> = Caller<'a, Process<'p>>;

fn memory(caller: &Ctx<'_, '_>) -> Option<Memory> {
    caller.get_export("memory").and_then(Extern::into_memory)
}

fn read(caller: &Ctx<'_, '_>, ptr: i32, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0; len];
    memory(caller)?.read(caller, ptr as u32 as usize, &mut buf).ok()?;
    Some(buf)
}

fn write(caller: &mut Ctx<'_, '_>, ptr: i32, bytes: &[u8]) -> bool {
    memory(caller).is_some_and(|m| m.write(&mut *caller, ptr as u32 as usize, bytes).is_ok())
}

fn read_u32(caller: &Ctx<'_, '_>, ptr: i32) -> Option<u32> {
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
fn write_strings(caller: &mut Ctx<'_, '_>, strings: &[String], ptrs: i32, buf: i32) -> i32 {
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
fn write_sizes(caller: &mut Ctx<'_, '_>, strings: &[String], count_ptr: i32, size_ptr: i32) -> i32 {
    let size: usize = strings.iter().map(|s| s.len() + 1).sum();
    errno(
        write(caller, count_ptr, &(strings.len() as u32).to_le_bytes())
            && write(caller, size_ptr, &(size as u32).to_le_bytes()),
    )
}

/// A `filestat` record: device, inode, type, links, size and three timestamps.
fn filestat(kind: u8, size: u64) -> [u8; 64] {
    let mut stat = [0u8; 64];
    stat[16] = kind;
    stat[24..32].copy_from_slice(&1u64.to_le_bytes());
    stat[32..40].copy_from_slice(&size.to_le_bytes());
    stat
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
    "fd_tell",
    "fd_read",
    "fd_readdir",
    "fd_filestat_get",
    "fd_sync",
    "fd_datasync",
    "path_open",
    "path_filestat_get",
    "path_create_directory",
    "path_remove_directory",
    "path_unlink_file",
];

/// A linker with every import `module` needs. Calls the apps should never make are answered
/// with `ENOSYS`, so a template linking more of libstd than it uses still instantiates.
pub fn linker<'p>(module: &Module) -> Result<Linker<Process<'p>>, Error> {
    let mut linker = Linker::new(module.engine());

    // The one call that is not WASI: an app says which port it accepts connections on. Nothing
    // is bound; the oracle uses the declaration (C6).
    linker.func_wrap("detective", "listen", |mut c: Ctx<'_, '_>, port: i32| {
        if let Ok(port) = u16::try_from(port) {
            let at = c.data().elapsed_secs();
            c.data_mut().listens.push((at, port));
        }
    })?;

    // Reaching another service: the host asks the connectivity oracle and, if something accepts,
    // the program listening there (C6). Returns the HTTP status, or a negative number for a
    // connection that failed (see `NetReply`). The body is cut to fit `out_cap`.
    linker.func_wrap(
        "detective",
        "request",
        |mut c: Ctx<'_, '_>,
         host: i32,
         host_len: i32,
         port: i32,
         method: i32,
         method_len: i32,
         path: i32,
         path_len: i32,
         out: i32,
         out_cap: i32,
         out_len: i32|
         -> i32 {
            let text = |c: &Ctx<'_, '_>, ptr, len| {
                read(c, ptr, len as usize).map(|b| String::from_utf8_lossy(&b).into_owned())
            };
            let (Some(host), Some(method), Some(path)) = (
                text(&c, host, host_len),
                text(&c, method, method_len),
                text(&c, path, path_len),
            ) else {
                return EFAULT;
            };
            let reply = c.data_mut().ask(&host, i64::from(port), &method, &path);
            let body = &reply.body.as_bytes()[..reply.body.len().min(out_cap.max(0) as usize)];
            if !write(&mut c, out, body) || !write(&mut c, out_len, &(body.len() as u32).to_le_bytes()) {
                return EFAULT;
            }
            reply.code
        },
    )?;

    linker.func_wrap(MODULE, "args_sizes_get", |mut c: Ctx<'_, '_>, count: i32, size: i32| {
        let args = std::mem::take(&mut c.data_mut().args);
        let r = write_sizes(&mut c, &args, count, size);
        c.data_mut().args = args;
        r
    })?;
    linker.func_wrap(MODULE, "args_get", |mut c: Ctx<'_, '_>, ptrs: i32, buf: i32| {
        let args = std::mem::take(&mut c.data_mut().args);
        let r = write_strings(&mut c, &args, ptrs, buf);
        c.data_mut().args = args;
        r
    })?;
    linker.func_wrap(
        MODULE,
        "environ_sizes_get",
        |mut c: Ctx<'_, '_>, count: i32, size: i32| {
            let env = std::mem::take(&mut c.data_mut().env);
            let r = write_sizes(&mut c, &env, count, size);
            c.data_mut().env = env;
            r
        },
    )?;
    linker.func_wrap(MODULE, "environ_get", |mut c: Ctx<'_, '_>, ptrs: i32, buf: i32| {
        let env = std::mem::take(&mut c.data_mut().env);
        let r = write_strings(&mut c, &env, ptrs, buf);
        c.data_mut().env = env;
        r
    })?;

    linker.func_wrap(
        MODULE,
        "fd_write",
        |mut c: Ctx<'_, '_>, fd: i32, iovs: i32, iovs_len: i32, written_ptr: i32| {
            let stream = match fd {
                1 => Some(0),
                2 => Some(1),
                f if f > ROOT_FD => None,
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
                match stream {
                    Some(stream) => c.data_mut().write(stream, &bytes),
                    None => {
                        let now = c.data().now_secs();
                        match c.data_mut().fs.write(fd as u32, &bytes, now) {
                            Ok(_) => {}
                            Err(e) => return e,
                        }
                    }
                }
                total += len;
            }
            errno(write(&mut c, written_ptr, &total.to_le_bytes()))
        },
    )?;

    linker.func_wrap(MODULE, "proc_exit", |_: Ctx<'_, '_>, code: i32| -> Result<(), Error> {
        Err(Error::i32_exit(code))
    })?;

    linker.func_wrap(
        MODULE,
        "clock_time_get",
        |mut c: Ctx<'_, '_>, clock: i32, _precision: i64, time_ptr: i32| {
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
        |mut c: Ctx<'_, '_>, subs: i32, events: i32, nsubs: i32, nevents_ptr: i32| -> Result<i32, Error> {
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
            if p.elapsed.saturating_add(wait) > HORIZON_NANOS
                || p.lines.len() >= MAX_LINES
                || p.fs.writes.len() >= MAX_WRITES
            {
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

    linker.func_wrap(MODULE, "random_get", |mut c: Ctx<'_, '_>, buf: i32, len: i32| {
        let bytes: Vec<u8> = (0..len.max(0) as usize)
            .map(|_| c.data_mut().next_random() as u8)
            .collect();
        errno(write(&mut c, buf, &bytes))
    })?;
    linker.func_wrap(MODULE, "sched_yield", |_: Ctx<'_, '_>| ESUCCESS)?;

    // Descriptors 0-2 are character devices, 3 is the preopened root, and files and
    // directories opened under it start at 4.
    linker.func_wrap(MODULE, "fd_fdstat_get", |mut c: Ctx<'_, '_>, fd: i32, stat: i32| {
        let kind = match fd {
            0..=2 => 2, // character device
            ROOT_FD => TYPE_DIRECTORY,
            _ => {
                let now = c.data().now_secs();
                match c.data_mut().fs.stat_fd(fd as u32, now) {
                    Ok((kind, _)) => kind,
                    Err(e) => return e,
                }
            }
        };
        let mut fdstat = [0u8; 24];
        fdstat[0] = kind;
        fdstat[8..16].copy_from_slice(&u64::MAX.to_le_bytes()); // rights: everything
        fdstat[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        errno(write(&mut c, stat, &fdstat))
    })?;
    linker.func_wrap(MODULE, "fd_prestat_get", |mut c: Ctx<'_, '_>, fd: i32, buf: i32| {
        if fd != ROOT_FD {
            return EBADF;
        }
        // tag 0 = directory, then the length of its name ("/").
        let mut prestat = [0u8; 8];
        prestat[4..8].copy_from_slice(&1u32.to_le_bytes());
        errno(write(&mut c, buf, &prestat))
    })?;
    linker.func_wrap(
        MODULE,
        "fd_prestat_dir_name",
        |mut c: Ctx<'_, '_>, fd: i32, path: i32, len: i32| {
            if fd != ROOT_FD {
                return EBADF;
            }
            if len < 1 {
                return EFAULT;
            }
            errno(write(&mut c, path, b"/"))
        },
    )?;
    linker.func_wrap(MODULE, "fd_close", |mut c: Ctx<'_, '_>, fd: i32| {
        if fd <= ROOT_FD {
            return EBADF;
        }
        c.data_mut().fs.close(fd as u32)
    })?;
    linker.func_wrap(
        MODULE,
        "fd_seek",
        |mut c: Ctx<'_, '_>, fd: i32, offset: i64, whence: i32, new_ptr: i32| {
            if fd <= ROOT_FD {
                return ESPIPE;
            }
            let now = c.data().now_secs();
            match c.data_mut().fs.seek(fd as u32, offset, whence as u8, now) {
                Ok(at) => errno(write(&mut c, new_ptr, &at.to_le_bytes())),
                Err(e) => e,
            }
        },
    )?;
    linker.func_wrap(MODULE, "fd_tell", |mut c: Ctx<'_, '_>, fd: i32, ptr: i32| {
        if fd <= ROOT_FD {
            return ESPIPE;
        }
        match c.data().fs.tell(fd as u32) {
            Ok(at) => errno(write(&mut c, ptr, &at.to_le_bytes())),
            Err(e) => e,
        }
    })?;
    linker.func_wrap(
        MODULE,
        "fd_read",
        |mut c: Ctx<'_, '_>, fd: i32, iovs: i32, iovs_len: i32, read_ptr: i32| {
            // Standard input is empty and never ends; only files can be read.
            if fd <= ROOT_FD {
                return ENOSYS;
            }
            let mut total = 0u32;
            for i in 0..iovs_len {
                let iov = iovs + 8 * i;
                let (Some(buf), Some(len)) = (read_u32(&c, iov), read_u32(&c, iov + 4)) else {
                    return EFAULT;
                };
                let now = c.data().now_secs();
                let chunk = match c.data_mut().fs.read(fd as u32, len as usize, now) {
                    Ok(chunk) => chunk,
                    Err(e) => return e,
                };
                if !write(&mut c, buf as i32, &chunk) {
                    return EFAULT;
                }
                total += chunk.len() as u32;
                if chunk.len() < len as usize {
                    break;
                }
            }
            errno(write(&mut c, read_ptr, &total.to_le_bytes()))
        },
    )?;
    linker.func_wrap(
        MODULE,
        "path_open",
        |mut c: Ctx<'_, '_>,
         dirfd: i32,
         _dirflags: i32,
         path_ptr: i32,
         path_len: i32,
         oflags: i32,
         rights: i64,
         _inheriting: i64,
         fdflags: i32,
         fd_ptr: i32| {
            if dirfd != ROOT_FD {
                return EBADF;
            }
            let Some(path) = read(&c, path_ptr, path_len.max(0) as usize) else {
                return EFAULT;
            };
            let path = String::from_utf8_lossy(&path).into_owned();
            let now = c.data().now_secs();
            let wants_write = rights & RIGHT_FD_WRITE != 0;
            let append = fdflags & 1 != 0;
            match c.data_mut().fs.open(&path, oflags as u16, wants_write, append, now) {
                Ok(fd) => errno(write(&mut c, fd_ptr, &fd.to_le_bytes())),
                Err(e) => e,
            }
        },
    )?;
    linker.func_wrap(MODULE, "fd_filestat_get", |mut c: Ctx<'_, '_>, fd: i32, buf: i32| {
        let (kind, size) = match fd {
            0..=2 => (2, 0),
            ROOT_FD => (TYPE_DIRECTORY, 0),
            _ => {
                let now = c.data().now_secs();
                match c.data_mut().fs.stat_fd(fd as u32, now) {
                    Ok(stat) => stat,
                    Err(e) => return e,
                }
            }
        };
        errno(write(&mut c, buf, &filestat(kind, size)))
    })?;
    linker.func_wrap(
        MODULE,
        "path_filestat_get",
        |mut c: Ctx<'_, '_>, dirfd: i32, _flags: i32, path_ptr: i32, path_len: i32, buf: i32| {
            if dirfd != ROOT_FD {
                return EBADF;
            }
            let Some(path) = read(&c, path_ptr, path_len.max(0) as usize) else {
                return EFAULT;
            };
            let now = c.data().now_secs();
            match c.data_mut().fs.stat_path(&String::from_utf8_lossy(&path), now) {
                Ok((kind, size)) => errno(write(&mut c, buf, &filestat(kind, size))),
                Err(e) => e,
            }
        },
    )?;
    linker.func_wrap(
        MODULE,
        "fd_readdir",
        |mut c: Ctx<'_, '_>, fd: i32, buf: i32, buf_len: i32, cookie: i64, used_ptr: i32| {
            let entries = match c.data().fs.readdir(fd as u32, cookie as u64) {
                Ok(entries) => entries,
                Err(e) => return e,
            };
            // A dirent is 24 bytes (next cookie, inode, name length, type) and then the name.
            let mut stream = Vec::new();
            for (i, (name, kind)) in entries.iter().enumerate() {
                stream.extend_from_slice(&(cookie as u64 + i as u64 + 1).to_le_bytes());
                stream.extend_from_slice(&(cookie as u64 + i as u64 + 1).to_le_bytes());
                stream.extend_from_slice(&(name.len() as u32).to_le_bytes());
                stream.push(*kind);
                stream.extend_from_slice(&[0, 0, 0]);
                stream.extend_from_slice(name.as_bytes());
            }
            stream.truncate(buf_len.max(0) as usize);
            errno(write(&mut c, buf, &stream) && write(&mut c, used_ptr, &(stream.len() as u32).to_le_bytes()))
        },
    )?;
    for name in ["path_create_directory", "path_remove_directory", "path_unlink_file"] {
        linker.func_wrap(
            MODULE,
            name,
            move |mut c: Ctx<'_, '_>, dirfd: i32, path_ptr: i32, path_len: i32| {
                if dirfd != ROOT_FD {
                    return EBADF;
                }
                let Some(path) = read(&c, path_ptr, path_len.max(0) as usize) else {
                    return EFAULT;
                };
                let path = String::from_utf8_lossy(&path).into_owned();
                let now = c.data().now_secs();
                let fs = &mut c.data_mut().fs;
                let result = match name {
                    "path_create_directory" => fs.mkdir(&path, now),
                    "path_remove_directory" => fs.rmdir(&path, now),
                    _ => fs.unlink(&path, now),
                };
                result.err().unwrap_or(ESUCCESS)
            },
        )?;
    }
    linker.func_wrap(MODULE, "fd_sync", |_: Ctx<'_, '_>, _fd: i32| ESUCCESS)?;
    linker.func_wrap(MODULE, "fd_datasync", |_: Ctx<'_, '_>, _fd: i32| ESUCCESS)?;

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
