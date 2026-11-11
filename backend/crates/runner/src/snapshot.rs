//! Change detection for read-only runs (WP06 plan mode).
//!
//! A snapshot records every entry below the workspace – `.git` and ignored
//! files included – without following symlinks: type, size, mode, inode,
//! mtime, ctime and the symlink target. An unprivileged process can reset
//! the mtime of a file it changed (`touch -d`), but not its ctime, so any
//! content or metadata change shows up. Directories compare by type, mode
//! and inode only (their times change whenever an entry inside does, which
//! the entry itself already reports).
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// More entries than this make the comparison incomplete (counts as changed).
pub const MAX_ENTRIES: usize = 200_000;

/// Path reported when a snapshot could not be completed.
pub const INCOMPLETE: &str = "(Prüfung unvollständig)";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Sig {
    kind: u8,
    len: u64,
    mode: u32,
    ino: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
    link: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Snapshot {
    entries: BTreeMap<PathBuf, Sig>,
    complete: bool,
}

#[cfg(unix)]
fn sig(meta: &std::fs::Metadata, path: &Path) -> Sig {
    use std::os::unix::fs::MetadataExt;
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        2
    } else if ft.is_dir() {
        1
    } else if ft.is_file() {
        0
    } else {
        3
    };
    let dir = kind == 1;
    Sig {
        kind,
        len: if dir { 0 } else { meta.len() },
        mode: meta.mode(),
        ino: meta.ino(),
        mtime: if dir {
            (0, 0)
        } else {
            (meta.mtime(), meta.mtime_nsec())
        },
        ctime: if dir {
            (0, 0)
        } else {
            (meta.ctime(), meta.ctime_nsec())
        },
        link: if kind == 2 {
            std::fs::read_link(path).ok()
        } else {
            None
        },
    }
}

#[cfg(not(unix))]
fn sig(meta: &std::fs::Metadata, _path: &Path) -> Sig {
    Sig {
        kind: if meta.is_dir() { 1 } else { 0 },
        len: meta.len(),
        mode: 0,
        ino: 0,
        mtime: (0, 0),
        ctime: (0, 0),
        link: None,
    }
}

/// Walks `root` (blocking). Never follows symlinks.
pub fn take(root: &Path, limit: usize) -> Snapshot {
    let mut s = Snapshot {
        entries: BTreeMap::new(),
        complete: true,
    };
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = root.join(&rel);
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => {
                s.complete = false;
                continue;
            }
        };
        for e in rd {
            let Ok(e) = e else {
                s.complete = false;
                continue;
            };
            if s.entries.len() >= limit {
                s.complete = false;
                return s;
            }
            let r = rel.join(e.file_name());
            let p = root.join(&r);
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                s.complete = false;
                continue;
            };
            let g = sig(&meta, &p);
            if g.kind == 1 {
                stack.push(r.clone());
            }
            s.entries.insert(r, g);
        }
    }
    s
}

/// Paths that differ (created, removed, changed), sorted, at most `max`,
/// and their total number. An incomplete snapshot is itself a difference.
pub fn diff(before: &Snapshot, after: &Snapshot, max: usize) -> (Vec<String>, usize) {
    let mut out = vec![];
    let mut total = 0;
    let mut push = |p: &Path| {
        total += 1;
        if out.len() < max {
            out.push(p.display().to_string());
        }
    };
    if !before.complete || !after.complete {
        push(Path::new(INCOMPLETE));
    }
    for (p, s) in &before.entries {
        match after.entries.get(p) {
            Some(t) if t == s => {}
            _ => push(p),
        }
    }
    for p in after.entries.keys() {
        if !before.entries.contains_key(p) {
            push(p);
        }
    }
    out.sort();
    (out, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(root: &Path, script: &str) {
        let ok = std::process::Command::new("sh")
            .args(["-c", script])
            .current_dir(root)
            .status()
            .expect("sh")
            .success();
        assert!(ok, "{script}");
    }

    #[test]
    fn unchanged_tree_has_no_difference() {
        let t = tempfile::tempdir().unwrap();
        run(
            t.path(),
            "mkdir -p a/.git b && echo x > a/f && echo y > a/.git/HEAD && ln -s ../a/f b/l",
        );
        let a = take(t.path(), MAX_ENTRIES);
        // Reading changes nothing.
        run(
            t.path(),
            "cat a/f a/.git/HEAD b/l >/dev/null && ls -R >/dev/null",
        );
        let b = take(t.path(), MAX_ENTRIES);
        assert_eq!(diff(&a, &b, 50), (vec![], 0));
    }

    #[test]
    fn every_kind_of_change_is_found() {
        let t = tempfile::tempdir().unwrap();
        run(
            t.path(),
            "mkdir -p src .git/hooks node_modules/x && echo a > src/a.rs && echo b > src/b.rs && echo c > src/c.rs \
             && echo ref > .git/HEAD && echo ign > node_modules/x/i.js && ln -s src/a.rs link && echo m > mode.sh",
        );
        let before = take(t.path(), MAX_ENTRIES);
        std::thread::sleep(std::time::Duration::from_millis(20));
        run(
            t.path(),
            // content change with the old mtime restored, delete, create,
            // .git change, ignored file, symlink retarget, chmod, new dir
            "cp -p src/a.rs /tmp/.queue-snap-ref-$$ && echo A > src/a.rs && touch -r /tmp/.queue-snap-ref-$$ src/a.rs && rm /tmp/.queue-snap-ref-$$ \
             && rm src/b.rs && echo new > src/new.rs && echo hook > .git/hooks/post-commit \
             && echo changed > node_modules/x/i.js && ln -sfn src/c.rs link && chmod +x mode.sh && mkdir extra",
        );
        let after = take(t.path(), MAX_ENTRIES);
        let (paths, total) = diff(&before, &after, 50);
        for p in [
            "src/a.rs",
            "src/b.rs",
            "src/new.rs",
            ".git/hooks/post-commit",
            "node_modules/x/i.js",
            "link",
            "mode.sh",
            "extra",
        ] {
            assert!(paths.contains(&p.to_string()), "{p} missing in {paths:?}");
        }
        assert_eq!(total, paths.len());
        // The unchanged file is not reported.
        assert!(!paths.contains(&"src/c.rs".to_string()));
    }

    #[test]
    fn report_is_capped_and_incomplete_counts_as_change() {
        let t = tempfile::tempdir().unwrap();
        let before = take(t.path(), MAX_ENTRIES);
        run(t.path(), "for i in $(seq 1 30); do echo $i > f$i; done");
        let after = take(t.path(), MAX_ENTRIES);
        let (paths, total) = diff(&before, &after, 10);
        assert_eq!((paths.len(), total), (10, 30));
        // Over the entry limit: incomplete, reported even without other changes.
        let a = take(t.path(), 5);
        let b = take(t.path(), 5);
        let (paths, total) = diff(&a, &b, 10);
        assert_eq!(paths, vec![INCOMPLETE.to_string()]);
        assert_eq!(total, 1);
    }
}
