//! The files a container sees: a pod's volumes mounted at paths, as one virtual filesystem.
//!
//! A run is simulated ahead of time, but a volume's contents change while the pod lives (a
//! ConfigMap is refreshed, a sibling container writes). Contents are therefore a *timeline*: a
//! base state plus events stamped with the second they happen. The filesystem a run reads is
//! that timeline as of the run's own virtual clock, plus the run's own writes, which it records
//! as events for the others to see.
//!
//! Files are text. Directories are the prefixes of file names, plus empty directories made with
//! a marker entry (`name/`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// WASI errno values the filesystem answers with.
pub const EEXIST: i32 = 20;
pub const EINVAL: i32 = 28;
pub const EISDIR: i32 = 31;
pub const ENOENT: i32 = 44;
pub const ENOTDIR: i32 = 54;
pub const ENOTEMPTY: i32 = 55;
pub const EROFS: i32 = 69;
pub const EBADF: i32 = 8;

/// WASI file types.
pub const TYPE_DIRECTORY: u8 = 3;
pub const TYPE_FILE: u8 = 4;

/// The files of one volume, by path within the volume.
pub type Files = BTreeMap<String, String>;
/// Every volume of a pod, by name.
pub type Volumes = BTreeMap<String, Files>;

/// A file written (`Some`) or removed (`None`) at a second.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolEvent {
    /// Epoch seconds.
    pub at: i64,
    pub volume: String,
    pub path: String,
    /// The new content, or `None` when the file was removed.
    pub content: Option<String>,
    /// `content` was added at the end of the file, not written over it (so a log that is only
    /// ever appended to is recorded line by line, not as a whole new file each time).
    #[serde(default)]
    pub append: bool,
}

/// Volume contents over time: the state at the start, then events.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    pub base: Volumes,
    pub events: Vec<VolEvent>,
}

/// A volume mounted into a container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    /// Absolute path inside the container, without a trailing slash.
    pub path: String,
    pub volume: String,
    /// `subPath`: the part of the volume that is mounted (empty: all of it).
    pub sub_path: String,
    pub read_only: bool,
    /// The mount sees the volume as it was when the container started and never refreshes (a
    /// `subPath` mount of a ConfigMap or Secret).
    pub frozen: bool,
}

fn apply(state: &mut Volumes, event: &VolEvent) {
    match &event.content {
        Some(text) if event.append => {
            state
                .entry(event.volume.clone())
                .or_default()
                .entry(event.path.clone())
                .or_default()
                .push_str(text);
        }
        Some(text) => {
            state
                .entry(event.volume.clone())
                .or_default()
                .insert(event.path.clone(), text.clone());
        }
        None => {
            if let Some(files) = state.get_mut(&event.volume) {
                files.remove(&event.path);
            }
        }
    }
}

fn join(prefix: &str, rel: &str) -> String {
    match (prefix.is_empty(), rel.is_empty()) {
        (true, _) => rel.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}/{rel}"),
    }
}

/// A path with `.` and `..` resolved, as a list of components.
fn components(path: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn is_dir(files: &Files, path: &str) -> bool {
    path.is_empty() || files.keys().any(|k| k.starts_with(&format!("{path}/")))
}

fn is_file(files: &Files, path: &str) -> bool {
    !path.ends_with('/') && files.contains_key(path)
}

/// The entries directly inside `path`: `(name, is_dir)`.
fn list(files: &Files, path: &str) -> Vec<(String, bool)> {
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}/")
    };
    let mut entries: BTreeMap<String, bool> = BTreeMap::new();
    for key in files.keys().filter(|k| k.starts_with(&prefix)) {
        let rest = &key[prefix.len()..];
        match rest.split_once('/') {
            Some(("", _)) | None if rest.is_empty() => {}
            Some((dir, _)) => {
                entries.insert(dir.to_string(), true);
            }
            None => {
                entries.entry(rest.to_string()).or_insert(false);
            }
        }
    }
    entries.into_iter().collect()
}

enum Open {
    File {
        mount: usize,
        /// Path within the volume.
        path: String,
        pos: u64,
        write: bool,
        append: bool,
    },
    Dir {
        entries: Vec<(String, u8)>,
    },
}

/// Where a guest path points.
enum Located {
    /// Inside a mount: its index and the path relative to the mount's root.
    Mounted(usize, String),
    /// A directory made of mount points only (`/`, `/etc` above `/etc/config`).
    Virtual(Vec<String>),
    Missing,
}

pub struct Fs {
    mounts: Vec<Mount>,
    state: Volumes,
    events: Vec<VolEvent>,
    next_event: usize,
    /// For frozen mounts: the part of the volume they see, as of the start.
    frozen: BTreeMap<usize, Files>,
    /// What this run wrote, for the other containers of the pod.
    pub writes: Vec<VolEvent>,
    open: BTreeMap<u32, Open>,
    next_fd: u32,
}

impl Fs {
    pub fn new(mounts: &[Mount], timeline: &Timeline, started_at: i64) -> Fs {
        let mut events = timeline.events.clone();
        events.sort_by_key(|e| e.at);
        let mut fs = Fs {
            mounts: mounts.to_vec(),
            state: timeline.base.clone(),
            events,
            next_event: 0,
            frozen: BTreeMap::new(),
            writes: Vec::new(),
            open: BTreeMap::new(),
            next_fd: 4,
        };
        fs.sync(started_at);
        for (index, mount) in mounts.iter().enumerate().filter(|(_, m)| m.frozen) {
            let files = fs.state.get(&mount.volume).cloned().unwrap_or_default();
            let sub = &mount.sub_path;
            let kept = files
                .into_iter()
                .filter(|(k, _)| sub.is_empty() || k == sub || k.starts_with(&format!("{sub}/")))
                .collect();
            fs.frozen.insert(index, kept);
        }
        fs
    }

    /// Bring the volumes up to the second `now`.
    pub fn sync(&mut self, now: i64) {
        while let Some(event) = self.events.get(self.next_event).filter(|e| e.at <= now) {
            apply(&mut self.state, event);
            self.next_event += 1;
        }
    }

    fn files(&self, mount: usize) -> Files {
        let m = &self.mounts[mount];
        match self.frozen.get(&mount) {
            Some(files) => files.clone(),
            None => self.state.get(&m.volume).cloned().unwrap_or_default(),
        }
    }

    fn locate(&self, guest: &str) -> Located {
        let parts = components(guest);
        let mut best: Option<(usize, usize)> = None;
        for (index, mount) in self.mounts.iter().enumerate() {
            let mount_parts = components(&mount.path);
            if parts.len() >= mount_parts.len()
                && parts[..mount_parts.len()] == mount_parts[..]
                && best.is_none_or(|(_, len)| mount_parts.len() > len)
            {
                best = Some((index, mount_parts.len()));
            }
        }
        if let Some((index, len)) = best {
            return Located::Mounted(index, parts[len..].join("/"));
        }
        let mut children: Vec<String> = self
            .mounts
            .iter()
            .filter_map(|m| {
                let mount_parts = components(&m.path);
                (mount_parts.len() > parts.len() && mount_parts[..parts.len()] == parts[..])
                    .then(|| mount_parts[parts.len()].to_string())
            })
            .collect();
        children.sort();
        children.dedup();
        if children.is_empty() {
            Located::Missing
        } else {
            Located::Virtual(children)
        }
    }

    fn writable(&self, mount: usize) -> bool {
        !self.mounts[mount].read_only
    }

    fn record(&mut self, mount: usize, path: &str, content: Option<String>, now: i64) {
        self.push_event(mount, path, content, false, now);
    }

    fn push_event(&mut self, mount: usize, path: &str, content: Option<String>, append: bool, now: i64) {
        let volume = self.mounts[mount].volume.clone();
        let event = VolEvent {
            at: now,
            volume: volume.clone(),
            path: path.to_string(),
            content,
            append,
        };
        apply(&mut self.state, &event);
        // Several changes to one file within a second are one event.
        match self.writes.last_mut() {
            Some(last) if last.at == now && last.volume == volume && last.path == path => match (last.append, append) {
                (true, true) | (false, true) if last.content.is_some() => {
                    let added = event.content.as_deref().unwrap_or_default();
                    last.content.get_or_insert_with(String::new).push_str(added);
                }
                _ => *last = event,
            },
            _ => self.writes.push(event),
        }
    }

    // ---- opening ----------------------------------------------------------------------------

    /// `oflags`: 1 create, 2 directory, 4 exclusive, 8 truncate (WASI).
    pub fn open(&mut self, guest: &str, oflags: u16, write: bool, append: bool, now: i64) -> Result<u32, i32> {
        self.sync(now);
        let (creat, directory, excl, trunc) = (oflags & 1 != 0, oflags & 2 != 0, oflags & 4 != 0, oflags & 8 != 0);
        let entry = match self.locate(guest) {
            Located::Missing => return Err(ENOENT),
            Located::Virtual(children) => {
                if write || trunc {
                    return Err(EISDIR);
                }
                Open::Dir {
                    entries: dir_entries(children.into_iter().map(|c| (c, true)).collect()),
                }
            }
            Located::Mounted(mount, rel) => {
                let files = self.files(mount);
                let path = join(&self.mounts[mount].sub_path, &rel);
                if is_dir(&files, &path) {
                    if write || trunc {
                        return Err(EISDIR);
                    }
                    Open::Dir {
                        entries: dir_entries(list(&files, &path)),
                    }
                } else if is_file(&files, &path) {
                    if creat && excl {
                        return Err(EEXIST);
                    }
                    if directory {
                        return Err(ENOTDIR);
                    }
                    if (write || trunc) && !self.writable(mount) {
                        return Err(EROFS);
                    }
                    if trunc {
                        self.record(mount, &path, Some(String::new()), now);
                    }
                    Open::File {
                        mount,
                        path,
                        pos: 0,
                        write,
                        append,
                    }
                } else {
                    if !creat {
                        return Err(ENOENT);
                    }
                    if !self.writable(mount) {
                        return Err(EROFS);
                    }
                    let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
                    if !is_dir(&files, parent) {
                        return Err(ENOENT);
                    }
                    self.record(mount, &path, Some(String::new()), now);
                    Open::File {
                        mount,
                        path,
                        pos: 0,
                        write,
                        append,
                    }
                }
            }
        };
        let fd = self.next_fd;
        self.next_fd += 1;
        self.open.insert(fd, entry);
        Ok(fd)
    }

    pub fn close(&mut self, fd: u32) -> i32 {
        if self.open.remove(&fd).is_some() {
            0
        } else {
            EBADF
        }
    }

    // ---- reading and writing ----------------------------------------------------------------

    fn content(&self, mount: usize, path: &str) -> Vec<u8> {
        self.files(mount)
            .get(path)
            .map(|c| c.as_bytes().to_vec())
            .unwrap_or_default()
    }

    pub fn read(&mut self, fd: u32, max: usize, now: i64) -> Result<Vec<u8>, i32> {
        self.sync(now);
        let Some(Open::File { mount, path, pos, .. }) = self.open.get(&fd) else {
            return Err(if self.open.contains_key(&fd) { EISDIR } else { EBADF });
        };
        let (mount, path, start) = (*mount, path.clone(), *pos as usize);
        let content = self.content(mount, &path);
        let end = (start + max).min(content.len());
        let chunk = content.get(start..end).unwrap_or_default().to_vec();
        if let Some(Open::File { pos, .. }) = self.open.get_mut(&fd) {
            *pos += chunk.len() as u64;
        }
        Ok(chunk)
    }

    pub fn write(&mut self, fd: u32, bytes: &[u8], now: i64) -> Result<usize, i32> {
        self.sync(now);
        let Some(Open::File {
            mount,
            path,
            pos,
            write,
            append,
        }) = self.open.get(&fd)
        else {
            return Err(if self.open.contains_key(&fd) { EISDIR } else { EBADF });
        };
        if !*write {
            return Err(EBADF);
        }
        if !self.writable(*mount) {
            return Err(EROFS);
        }
        let (mount, path, append, pos) = (*mount, path.clone(), *append, *pos as usize);
        let mut content = self.content(mount, &path);
        let old_len = content.len();
        let at = if append { old_len } else { pos };
        let end = at + bytes.len();
        // Writing exactly at the old end of the file is an append, recorded as just the new text.
        let pure_append = !bytes.is_empty() && at == old_len;
        if content.len() < end {
            content.resize(end, 0);
        }
        content[at..end].copy_from_slice(bytes);
        if pure_append {
            self.push_event(
                mount,
                &path,
                Some(String::from_utf8_lossy(bytes).into_owned()),
                true,
                now,
            );
        } else {
            self.record(mount, &path, Some(String::from_utf8_lossy(&content).into_owned()), now);
        }
        if let Some(Open::File { pos, .. }) = self.open.get_mut(&fd) {
            *pos = end as u64;
        }
        Ok(bytes.len())
    }

    /// `whence`: 0 from the start, 1 from here, 2 from the end.
    pub fn seek(&mut self, fd: u32, offset: i64, whence: u8, now: i64) -> Result<u64, i32> {
        self.sync(now);
        let Some(Open::File { mount, path, pos, .. }) = self.open.get(&fd) else {
            return Err(if self.open.contains_key(&fd) { EISDIR } else { EBADF });
        };
        let base = match whence {
            0 => 0,
            1 => *pos as i64,
            2 => self.content(*mount, path).len() as i64,
            _ => return Err(EINVAL),
        };
        let target = base.checked_add(offset).filter(|t| *t >= 0).ok_or(EINVAL)?;
        if let Some(Open::File { pos, .. }) = self.open.get_mut(&fd) {
            *pos = target as u64;
        }
        Ok(target as u64)
    }

    pub fn tell(&self, fd: u32) -> Result<u64, i32> {
        match self.open.get(&fd) {
            Some(Open::File { pos, .. }) => Ok(*pos),
            Some(Open::Dir { .. }) => Err(EISDIR),
            None => Err(EBADF),
        }
    }

    // ---- metadata ---------------------------------------------------------------------------

    /// `(file type, size)` of an open descriptor.
    pub fn stat_fd(&mut self, fd: u32, now: i64) -> Result<(u8, u64), i32> {
        self.sync(now);
        match self.open.get(&fd) {
            Some(Open::File { mount, path, .. }) => Ok((TYPE_FILE, self.content(*mount, path).len() as u64)),
            Some(Open::Dir { .. }) => Ok((TYPE_DIRECTORY, 0)),
            None => Err(EBADF),
        }
    }

    pub fn stat_path(&mut self, guest: &str, now: i64) -> Result<(u8, u64), i32> {
        self.sync(now);
        match self.locate(guest) {
            Located::Missing => Err(ENOENT),
            Located::Virtual(_) => Ok((TYPE_DIRECTORY, 0)),
            Located::Mounted(mount, rel) => {
                let files = self.files(mount);
                let path = join(&self.mounts[mount].sub_path, &rel);
                if is_dir(&files, &path) {
                    Ok((TYPE_DIRECTORY, 0))
                } else if is_file(&files, &path) {
                    Ok((TYPE_FILE, files[&path].len() as u64))
                } else {
                    Err(ENOENT)
                }
            }
        }
    }

    /// The directory entries of an open directory, from the `cookie`th, as `(name, type)`.
    pub fn readdir(&self, fd: u32, cookie: u64) -> Result<Vec<(String, u8)>, i32> {
        match self.open.get(&fd) {
            Some(Open::Dir { entries }) => Ok(entries.iter().skip(cookie as usize).cloned().collect()),
            Some(Open::File { .. }) => Err(ENOTDIR),
            None => Err(EBADF),
        }
    }

    // ---- changing the tree ------------------------------------------------------------------

    fn writable_target(&self, guest: &str) -> Result<(usize, String), i32> {
        match self.locate(guest) {
            Located::Missing => Err(ENOENT),
            Located::Virtual(_) => Err(EROFS),
            Located::Mounted(mount, rel) => {
                if rel.is_empty() {
                    return Err(EROFS);
                }
                Ok((mount, join(&self.mounts[mount].sub_path, &rel)))
            }
        }
    }

    pub fn mkdir(&mut self, guest: &str, now: i64) -> Result<(), i32> {
        self.sync(now);
        if let Ok((mount, path)) = self.writable_target(guest) {
            let files = self.files(mount);
            if is_dir(&files, &path) || is_file(&files, &path) {
                return Err(EEXIST);
            }
            if !self.writable(mount) {
                return Err(EROFS);
            }
            let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
            if !is_dir(&files, parent) {
                return Err(ENOENT);
            }
            self.record(mount, &format!("{path}/"), Some(String::new()), now);
            return Ok(());
        }
        match self.locate(guest) {
            Located::Missing => Err(ENOENT),
            _ => Err(EEXIST),
        }
    }

    pub fn unlink(&mut self, guest: &str, now: i64) -> Result<(), i32> {
        self.sync(now);
        let (mount, path) = self.writable_target(guest)?;
        let files = self.files(mount);
        if is_dir(&files, &path) {
            return Err(EISDIR);
        }
        if !is_file(&files, &path) {
            return Err(ENOENT);
        }
        if !self.writable(mount) {
            return Err(EROFS);
        }
        self.record(mount, &path, None, now);
        Ok(())
    }

    pub fn rmdir(&mut self, guest: &str, now: i64) -> Result<(), i32> {
        self.sync(now);
        let (mount, path) = self.writable_target(guest)?;
        let files = self.files(mount);
        if is_file(&files, &path) {
            return Err(ENOTDIR);
        }
        if !is_dir(&files, &path) {
            return Err(ENOENT);
        }
        if !self.writable(mount) {
            return Err(EROFS);
        }
        if !list(&files, &path).is_empty() {
            return Err(ENOTEMPTY);
        }
        self.record(mount, &format!("{path}/"), None, now);
        Ok(())
    }
}

/// `.` and `..` first, as a directory read gives them, then the entries.
fn dir_entries(entries: Vec<(String, bool)>) -> Vec<(String, u8)> {
    let mut out = vec![(".".to_string(), TYPE_DIRECTORY), ("..".to_string(), TYPE_DIRECTORY)];
    out.extend(
        entries
            .into_iter()
            .map(|(name, dir)| (name, if dir { TYPE_DIRECTORY } else { TYPE_FILE })),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(path: &str, volume: &str, sub_path: &str, read_only: bool, frozen: bool) -> Mount {
        Mount {
            path: path.into(),
            volume: volume.into(),
            sub_path: sub_path.into(),
            read_only,
            frozen,
        }
    }

    fn volumes(entries: &[(&str, &[(&str, &str)])]) -> Volumes {
        entries
            .iter()
            .map(|(v, files)| {
                (
                    v.to_string(),
                    files.iter().map(|(k, c)| (k.to_string(), c.to_string())).collect(),
                )
            })
            .collect()
    }

    fn event(at: i64, volume: &str, path: &str, content: Option<&str>) -> VolEvent {
        VolEvent {
            at,
            volume: volume.into(),
            path: path.into(),
            content: content.map(String::from),
            append: false,
        }
    }

    fn fs(mounts: &[Mount], base: Volumes, events: Vec<VolEvent>, start: i64) -> Fs {
        Fs::new(mounts, &Timeline { base, events }, start)
    }

    fn slurp(fs: &mut Fs, path: &str, now: i64) -> Result<String, i32> {
        let fd = fs.open(path, 0, false, false, now)?;
        let mut out = Vec::new();
        loop {
            let chunk = fs.read(fd, 4, now)?;
            if chunk.is_empty() {
                break;
            }
            out.extend(chunk);
        }
        fs.close(fd);
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn reads_files_at_their_mount_points_and_lists_directories() {
        let mut f = fs(
            &[mount("/etc/config", "cfg", "", true, false)],
            volumes(&[("cfg", &[("mode", "fast"), ("sub/deep", "x")])]),
            vec![],
            0,
        );
        assert_eq!(slurp(&mut f, "/etc/config/mode", 0).unwrap(), "fast");
        assert_eq!(slurp(&mut f, "/etc/config/sub/../mode", 0).unwrap(), "fast");
        assert_eq!(slurp(&mut f, "/etc/config/sub/deep", 0).unwrap(), "x");
        assert_eq!(slurp(&mut f, "/etc/config/nope", 0), Err(ENOENT));
        assert_eq!(slurp(&mut f, "/etc/other", 0), Err(ENOENT));
        let fd = f.open("/etc/config", 2, false, false, 0).unwrap();
        let names: Vec<_> = f.readdir(fd, 0).unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, [".", "..", "mode", "sub"]);
        // Directories above a mount are made of the mounts.
        let fd = f.open("/etc", 2, false, false, 0).unwrap();
        assert_eq!(f.readdir(fd, 2).unwrap(), [("config".to_string(), TYPE_DIRECTORY)]);
        let fd = f.open("/", 2, false, false, 0).unwrap();
        assert_eq!(f.readdir(fd, 2).unwrap(), [("etc".to_string(), TYPE_DIRECTORY)]);
        assert_eq!(f.stat_path("/etc/config/mode", 0), Ok((TYPE_FILE, 4)));
        assert_eq!(f.stat_path("/etc/config/sub", 0), Ok((TYPE_DIRECTORY, 0)));
        assert_eq!(f.stat_path("/etc/config/gone", 0), Err(ENOENT));
    }

    #[test]
    fn read_only_mounts_refuse_every_change() {
        let mut f = fs(
            &[mount("/etc/config", "cfg", "", true, false)],
            volumes(&[("cfg", &[("mode", "fast")])]),
            vec![],
            0,
        );
        assert_eq!(f.open("/etc/config/mode", 0, true, false, 0), Err(EROFS));
        assert_eq!(f.open("/etc/config/new", 1, true, false, 0), Err(EROFS));
        assert_eq!(f.open("/etc/config/mode", 8, true, false, 0), Err(EROFS));
        assert_eq!(f.unlink("/etc/config/mode", 0), Err(EROFS));
        assert_eq!(f.mkdir("/etc/config/d", 0), Err(EROFS));
        assert_eq!(f.mkdir("/etc/config", 0), Err(EEXIST));
        assert_eq!(f.open("/etc", 0, true, false, 0), Err(EISDIR));
        assert!(f.writes.is_empty());
    }

    #[test]
    fn scratch_space_is_read_write_and_records_what_was_written() {
        let mut f = fs(
            &[mount("/data", "scratch", "", false, false)],
            volumes(&[("scratch", &[])]),
            vec![],
            10,
        );
        let fd = f.open("/data/log.txt", 1, true, false, 10).unwrap();
        assert_eq!(f.write(fd, b"one\n", 10), Ok(4));
        assert_eq!(f.write(fd, b"two\n", 11), Ok(4));
        assert_eq!(f.tell(fd), Ok(8));
        assert_eq!(slurp(&mut f, "/data/log.txt", 11).unwrap(), "one\ntwo\n");
        assert_eq!(f.stat_fd(fd, 11), Ok((TYPE_FILE, 8)));
        assert_eq!(f.writes.len(), 2, "one event per second");
        // The second write only adds text at the end, so only that text is recorded.
        assert_eq!(
            f.writes[1],
            VolEvent {
                append: true,
                ..event(11, "scratch", "log.txt", Some("two\n"))
            }
        );
        assert_eq!(
            f.writes[0].content.as_deref(),
            Some("one\n"),
            "creating then appending in one second"
        );

        // Appending, truncating, seeking.
        let fd = f.open("/data/log.txt", 0, true, true, 12).unwrap();
        f.write(fd, b"three\n", 12).unwrap();
        assert_eq!(slurp(&mut f, "/data/log.txt", 12).unwrap(), "one\ntwo\nthree\n");
        let fd = f.open("/data/log.txt", 8, true, false, 13).unwrap();
        assert_eq!(f.stat_fd(fd, 13), Ok((TYPE_FILE, 0)));
        f.write(fd, b"abcdef", 13).unwrap();
        assert_eq!(f.seek(fd, 2, 0, 13), Ok(2));
        f.write(fd, b"XY", 13).unwrap();
        assert_eq!(slurp(&mut f, "/data/log.txt", 13).unwrap(), "abXYef");
        assert_eq!(f.seek(fd, -1, 2, 13), Ok(5));
        assert_eq!(f.seek(fd, -9, 1, 13), Err(EINVAL));
    }

    #[test]
    fn creating_needs_a_parent_directory_and_exclusive_refuses_existing_files() {
        let mut f = fs(
            &[mount("/data", "scratch", "", false, false)],
            volumes(&[("scratch", &[])]),
            vec![],
            0,
        );
        assert_eq!(f.open("/data/a/b.txt", 1, true, false, 0), Err(ENOENT));
        f.mkdir("/data/a", 0).unwrap();
        assert_eq!(f.mkdir("/data/a", 0), Err(EEXIST));
        assert_eq!(f.mkdir("/data/x/y", 0), Err(ENOENT));
        f.open("/data/a/b.txt", 1, true, false, 0).unwrap();
        assert_eq!(f.open("/data/a/b.txt", 1 | 4, true, false, 0), Err(EEXIST));
        assert_eq!(f.open("/data/a/b.txt", 2, false, false, 0), Err(ENOTDIR));
        assert_eq!(f.open("/data/a", 0, true, false, 0), Err(EISDIR));
        assert_eq!(f.open("/data/missing", 0, false, false, 0), Err(ENOENT));
        // Removing.
        assert_eq!(f.rmdir("/data/a", 0), Err(ENOTEMPTY));
        assert_eq!(f.unlink("/data/a", 0), Err(EISDIR));
        f.unlink("/data/a/b.txt", 0).unwrap();
        assert_eq!(f.unlink("/data/a/b.txt", 0), Err(ENOENT));
        f.rmdir("/data/a", 0).unwrap();
        assert_eq!(f.stat_path("/data/a", 0), Err(ENOENT));
        assert_eq!(f.rmdir("/data", 0), Err(EROFS), "a mount's root cannot go");
    }

    #[test]
    fn other_writers_events_appear_when_their_second_comes() {
        let mut f = fs(
            &[mount("/cfg", "cfg", "", true, false)],
            volumes(&[("cfg", &[("mode", "fast")])]),
            vec![
                event(60, "cfg", "mode", Some("slow")),
                event(90, "cfg", "extra", Some("x")),
                event(120, "cfg", "extra", None),
            ],
            0,
        );
        assert_eq!(slurp(&mut f, "/cfg/mode", 59).unwrap(), "fast");
        assert_eq!(slurp(&mut f, "/cfg/mode", 60).unwrap(), "slow");
        assert_eq!(slurp(&mut f, "/cfg/extra", 89), Err(ENOENT));
        assert_eq!(slurp(&mut f, "/cfg/extra", 95).unwrap(), "x");
        assert_eq!(slurp(&mut f, "/cfg/extra", 120), Err(ENOENT));
    }

    #[test]
    fn events_before_the_start_are_already_applied() {
        let mut f = fs(
            &[mount("/cfg", "cfg", "", true, false)],
            volumes(&[("cfg", &[("mode", "fast")])]),
            vec![event(5, "cfg", "mode", Some("slow"))],
            10,
        );
        assert_eq!(slurp(&mut f, "/cfg/mode", 10).unwrap(), "slow");
    }

    #[test]
    fn a_frozen_sub_path_mount_never_refreshes_but_a_live_one_does() {
        let base = volumes(&[("cfg", &[("mode", "fast"), ("dir/a", "1")])]);
        let events = vec![
            event(30, "cfg", "mode", Some("slow")),
            event(30, "cfg", "dir/a", Some("2")),
        ];
        let mounts = [
            mount("/app/mode", "cfg", "mode", true, true),
            mount("/app/dir", "cfg", "dir", true, true),
            mount("/live/mode", "cfg", "mode", true, false),
        ];
        let mut f = fs(&mounts, base, events, 0);
        assert_eq!(slurp(&mut f, "/app/mode", 100).unwrap(), "fast");
        assert_eq!(slurp(&mut f, "/app/dir/a", 100).unwrap(), "1");
        assert_eq!(slurp(&mut f, "/live/mode", 100).unwrap(), "slow");
        assert_eq!(f.stat_path("/app/mode", 100), Ok((TYPE_FILE, 4)));
    }

    #[test]
    fn a_sub_path_of_scratch_space_is_a_live_directory() {
        let mut f = fs(
            &[mount("/data", "scratch", "pod-a", false, false)],
            volumes(&[("scratch", &[("pod-a/seed", "s"), ("other/file", "o")])]),
            vec![],
            0,
        );
        assert_eq!(slurp(&mut f, "/data/seed", 0).unwrap(), "s");
        assert_eq!(slurp(&mut f, "/data/file", 0), Err(ENOENT));
        let fd = f.open("/data/new", 1, true, false, 1).unwrap();
        f.write(fd, b"n", 1).unwrap();
        assert_eq!(f.writes[0].path, "pod-a/new", "recorded by its path in the volume");
    }

    #[test]
    fn bad_descriptors_and_directory_reads() {
        let mut f = fs(
            &[mount("/d", "v", "", false, false)],
            volumes(&[("v", &[("f", "x")])]),
            vec![],
            0,
        );
        assert_eq!(f.read(99, 4, 0), Err(EBADF));
        assert_eq!(f.close(99), EBADF);
        let dir = f.open("/d", 2, false, false, 0).unwrap();
        assert_eq!(f.read(dir, 4, 0), Err(EISDIR));
        assert_eq!(f.write(dir, b"x", 0), Err(EISDIR));
        let file = f.open("/d/f", 0, false, false, 0).unwrap();
        assert_eq!(f.write(file, b"x", 0), Err(EBADF), "opened read-only");
        assert_eq!(f.readdir(file, 0), Err(ENOTDIR));
        assert_eq!(f.close(file), 0);
        assert_eq!(f.close(file), EBADF, "already closed");
    }
}
