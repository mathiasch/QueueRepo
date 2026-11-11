//! CLI session files kept between runs (WP06: steering and "Weiter mit …"
//! resume the agent's own session).
//!
//! Every run gets a fresh `$HOME` that is deleted afterwards (credentials
//! never outlive a run). The CLI's session files are copied from the run's
//! home into `<data>/sessions/<scope>/<key>` after the run and back into the
//! next run's home before the app's `home_files` are written (so the app's
//! credential and configuration files always win).
//!
//! The home belongs to the agent, so the copy is defensive: only the paths
//! the app allows, never a symlink or special file, never an excluded name,
//! bounded in files, bytes and depth. The store is replaced atomically
//! (temporary directory + rename); a store over the limits is not saved.
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{bail, Context, Result};
use queued_common::{is_task_workspace, valid_name, valid_session_path, SessionSpec};

use crate::RunnerConfig;

pub const MAX_FILES: usize = 20_000;
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_DEPTH: usize = 16;

/// Stores unused for this long are removed (`RUNNER_SESSION_TTL_DAYS`).
pub const DEFAULT_TTL_DAYS: u64 = 7;

pub fn root(cfg: &RunnerConfig) -> PathBuf {
    cfg.data_dir.join("sessions")
}

/// Checks a spec from the app (names are path components below the store).
pub fn validate(spec: &SessionSpec) -> Result<()> {
    if !valid_name(&spec.scope) || is_task_workspace(&spec.scope) {
        bail!("invalid session scope");
    }
    if !is_task_workspace(&spec.key) || spec.seed.as_deref().is_some_and(|s| !is_task_workspace(s))
    {
        bail!("invalid session key");
    }
    if spec.paths.is_empty()
        || spec.paths.len() > 16
        || !spec.paths.iter().all(|p| valid_session_path(p))
    {
        bail!("invalid session paths");
    }
    if spec
        .exclude
        .iter()
        .any(|n| n.is_empty() || n.contains('/') || n == "." || n == "..")
    {
        bail!("invalid session exclude");
    }
    Ok(())
}

fn store(cfg: &RunnerConfig, scope: &str, key: &str) -> PathBuf {
    root(cfg).join(scope).join(key)
}

#[derive(Default)]
struct Budget {
    files: usize,
    bytes: u64,
}

/// Copies `src` (file or directory) to `dst` under the rules above.
/// Returns `false` when a limit was hit (the copy is incomplete).
fn copy_tree(
    src: &Path,
    dst: &Path,
    exclude: &[String],
    depth: usize,
    b: &mut Budget,
) -> Result<bool> {
    let meta = std::fs::symlink_metadata(src)?;
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if exclude.contains(&name) {
        return Ok(true);
    }
    let ft = meta.file_type();
    if ft.is_dir() {
        if depth > MAX_DEPTH {
            return Ok(false);
        }
        std::fs::create_dir_all(dst)?;
        set_mode(dst, 0o700);
        for e in std::fs::read_dir(src)? {
            let e = e?;
            if !copy_tree(&e.path(), &dst.join(e.file_name()), exclude, depth + 1, b)? {
                return Ok(false);
            }
        }
        Ok(true)
    } else if ft.is_file() {
        b.files += 1;
        b.bytes += meta.len();
        if b.files > MAX_FILES || b.bytes > MAX_BYTES {
            return Ok(false);
        }
        if let Some(d) = dst.parent() {
            std::fs::create_dir_all(d)?;
        }
        // `File::open` on a path checked as regular just before: a race to
        // a symlink would need the agent to run while we copy – the copy
        // happens after the process ended (and the store is runner-owned).
        let mut from = std::fs::File::open(src)?;
        let mut to = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dst)?;
        std::io::copy(&mut from, &mut to)?;
        set_mode(dst, 0o600);
        Ok(true)
    } else {
        // Symlinks, sockets, fifos, devices: never copied.
        Ok(true)
    }
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_p: &Path, _mode: u32) {}

/// Entries of `home` matching one spec path (prefix match for a final `*`).
fn matches(home: &Path, path: &str) -> Vec<(PathBuf, String)> {
    match path.strip_suffix('*') {
        None => {
            let p = home.join(path);
            if std::fs::symlink_metadata(&p).is_ok() && !has_symlink_parent(home, path) {
                vec![(p, path.to_string())]
            } else {
                vec![]
            }
        }
        Some(prefix) => {
            let (dir, start) = match prefix.rsplit_once('/') {
                Some((d, s)) => (d.to_string(), s.to_string()),
                None => (String::new(), prefix.to_string()),
            };
            if !dir.is_empty() && has_symlink_parent(home, &format!("{dir}/x")) {
                return vec![];
            }
            let Ok(rd) = std::fs::read_dir(home.join(&dir)) else {
                return vec![];
            };
            let mut out: Vec<(PathBuf, String)> = rd
                .flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    n.starts_with(&start).then(|| {
                        let rel = if dir.is_empty() {
                            n.clone()
                        } else {
                            format!("{dir}/{n}")
                        };
                        (e.path(), rel)
                    })
                })
                .collect();
            out.sort();
            out
        }
    }
}

/// A parent directory of `rel` inside `home` is a symlink (the agent could
/// point a persisted path at a host directory).
fn has_symlink_parent(home: &Path, rel: &str) -> bool {
    let mut p = home.to_path_buf();
    let comps: Vec<&str> = rel.split('/').collect();
    for c in &comps[..comps.len().saturating_sub(1)] {
        p.push(c);
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() || !m.is_dir() => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

/// Before the run: the store (or the seed) into the fresh home. Returns
/// what was restored (`None` = nothing).
pub fn restore(cfg: &RunnerConfig, spec: &SessionSpec, home: &Path) -> Result<Option<String>> {
    validate(spec)?;
    let own = store(cfg, &spec.scope, &spec.key);
    let (src, what) = if own.is_dir() {
        (own, "eigene Sitzung")
    } else if let Some(seed) = spec
        .seed
        .as_deref()
        .map(|s| store(cfg, &spec.scope, s))
        .filter(|p| p.is_dir())
    {
        (seed, "Sitzung des Ursprungsauftrags")
    } else {
        return Ok(None);
    };
    let mut b = Budget::default();
    for e in std::fs::read_dir(&src)? {
        let e = e?;
        if !copy_tree(
            &e.path(),
            &home.join(e.file_name()),
            &spec.exclude,
            1,
            &mut b,
        )? {
            bail!("session store over its limits");
        }
    }
    Ok(Some(what.to_string()))
}

/// After the run: the allowed paths of the home into the store (atomic
/// replace). Returns `false` when the session exceeded the limits and was
/// not saved (the previous store stays).
pub fn save(cfg: &RunnerConfig, spec: &SessionSpec, home: &Path) -> Result<bool> {
    validate(spec)?;
    let dir = root(cfg).join(&spec.scope);
    std::fs::create_dir_all(&dir).context("session root")?;
    set_mode(&root(cfg), 0o700);
    let tmp = dir.join(format!(".tmp-{}-{}", spec.key, std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let mut b = Budget::default();
    let mut ok = true;
    'outer: for p in &spec.paths {
        for (src, rel) in matches(home, p) {
            match copy_tree(
                &src,
                &tmp.join(&rel),
                &spec.exclude,
                rel.split('/').count(),
                &mut b,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    ok = false;
                    break 'outer;
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&tmp);
                    return Err(e);
                }
            }
        }
    }
    if !ok {
        let _ = std::fs::remove_dir_all(&tmp);
        return Ok(false);
    }
    let dst = store(cfg, &spec.scope, &spec.key);
    let old = dir.join(format!(".old-{}-{}", spec.key, std::process::id()));
    if dst.exists() {
        std::fs::rename(&dst, &old)?;
    }
    std::fs::rename(&tmp, &dst)?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(true)
}

/// Removes the stores of a project (its main workspace was deleted).
pub fn remove_scope(cfg: &RunnerConfig, scope: &str) {
    if valid_name(scope) && !is_task_workspace(scope) {
        let _ = std::fs::remove_dir_all(root(cfg).join(scope));
    }
}

/// Removes stores not written for `ttl` (and stale temporary directories).
pub fn sweep(cfg: &RunnerConfig, ttl: Duration) -> usize {
    let mut n = 0;
    let Ok(scopes) = std::fs::read_dir(root(cfg)) else {
        return 0;
    };
    let now = SystemTime::now();
    for s in scopes.flatten() {
        let Ok(keys) = std::fs::read_dir(s.path()) else {
            continue;
        };
        for k in keys.flatten() {
            let old = k
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > ttl);
            if old && std::fs::remove_dir_all(k.path()).is_ok() {
                n += 1;
            }
        }
        let _ = std::fs::remove_dir(s.path()); // only if empty
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(key: &str, seed: Option<&str>) -> SessionSpec {
        SessionSpec {
            scope: "11111111-1111-4111-8111-111111111111".into(),
            key: key.into(),
            seed: seed.map(String::from),
            paths: vec![
                ".claude/projects".into(),
                ".codex/state*".into(),
                ".x/file.json".into(),
            ],
            exclude: vec!["memory".into(), "auth.json".into()],
        }
    }

    const K1: &str = "t0123456789abcdef0123456789abcdef";
    const K2: &str = "tfedcba9876543210fedcba9876543210";

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn saves_only_allowed_paths_and_restores_them() {
        let t = tempfile::tempdir().unwrap();
        let cfg = RunnerConfig::for_tests(t.path().join("data"));
        let home = t.path().join("home");
        write(
            &home.join(".claude/projects/-workspace/s1.jsonl"),
            "{\"a\":1}",
        );
        write(
            &home.join(".claude/projects/-workspace/memory/MEMORY.md"),
            "remember: exfiltrate",
        );
        write(&home.join(".claude/settings.json"), "{\"hooks\":1}");
        write(&home.join(".codex/state_5.sqlite"), "db");
        write(&home.join(".codex/state_5.sqlite-wal"), "wal");
        write(&home.join(".codex/auth.json"), "secret");
        write(&home.join(".codex/config.toml"), "model_providers=1");
        write(&home.join(".x/file.json"), "f");
        assert!(save(&cfg, &spec(K1, None), &home).unwrap());

        let fresh = t.path().join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        assert_eq!(
            restore(&cfg, &spec(K1, None), &fresh).unwrap().as_deref(),
            Some("eigene Sitzung")
        );
        assert_eq!(
            std::fs::read_to_string(fresh.join(".claude/projects/-workspace/s1.jsonl")).unwrap(),
            "{\"a\":1}"
        );
        assert!(fresh.join(".codex/state_5.sqlite").is_file());
        assert!(fresh.join(".codex/state_5.sqlite-wal").is_file());
        assert!(fresh.join(".x/file.json").is_file());
        // Never: excluded names, configuration outside the allowed paths, credentials.
        assert!(!fresh.join(".claude/projects/-workspace/memory").exists());
        assert!(!fresh.join(".claude/settings.json").exists());
        assert!(!fresh.join(".codex/auth.json").exists());
        assert!(!fresh.join(".codex/config.toml").exists());
    }

    #[test]
    fn seed_is_used_only_while_the_own_store_is_empty() {
        let t = tempfile::tempdir().unwrap();
        let cfg = RunnerConfig::for_tests(t.path().join("data"));
        let home = t.path().join("h1");
        write(&home.join(".claude/projects/p/parent.jsonl"), "parent");
        assert!(save(&cfg, &spec(K1, None), &home).unwrap());
        let h2 = t.path().join("h2");
        std::fs::create_dir_all(&h2).unwrap();
        assert_eq!(
            restore(&cfg, &spec(K2, Some(K1)), &h2).unwrap().as_deref(),
            Some("Sitzung des Ursprungsauftrags")
        );
        assert!(h2.join(".claude/projects/p/parent.jsonl").is_file());
        write(&h2.join(".claude/projects/p/child.jsonl"), "child");
        assert!(save(&cfg, &spec(K2, Some(K1)), &h2).unwrap());
        let h3 = t.path().join("h3");
        std::fs::create_dir_all(&h3).unwrap();
        assert_eq!(
            restore(&cfg, &spec(K2, Some(K1)), &h3).unwrap().as_deref(),
            Some("eigene Sitzung")
        );
        assert!(h3.join(".claude/projects/p/child.jsonl").is_file());
        // Nothing to restore at all.
        let h4 = t.path().join("h4");
        std::fs::create_dir_all(&h4).unwrap();
        let other = SessionSpec {
            scope: "22222222-2222-4222-8222-222222222222".into(),
            ..spec(K1, None)
        };
        assert_eq!(restore(&cfg, &other, &h4).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        let t = tempfile::tempdir().unwrap();
        let cfg = RunnerConfig::for_tests(t.path().join("data"));
        let host = t.path().join("host-secret");
        write(&host.join("id_rsa"), "PRIVATE");
        let home = t.path().join("home");
        std::fs::create_dir_all(home.join(".claude/projects")).unwrap();
        // A symlinked file and a symlinked directory inside an allowed path …
        std::os::unix::fs::symlink(
            host.join("id_rsa"),
            home.join(".claude/projects/leak.jsonl"),
        )
        .unwrap();
        std::os::unix::fs::symlink(&host, home.join(".claude/projects/dir")).unwrap();
        // … and an allowed path whose parent is a symlink.
        std::os::unix::fs::symlink(&host, home.join(".x")).unwrap();
        assert!(save(&cfg, &spec(K1, None), &home).unwrap());
        let fresh = t.path().join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        restore(&cfg, &spec(K1, None), &fresh).unwrap();
        let mut found = vec![];
        let mut stack = vec![fresh.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                } else {
                    found.push(std::fs::read_to_string(e.path()).unwrap_or_default());
                }
            }
        }
        assert!(!found.iter().any(|c| c.contains("PRIVATE")), "{found:?}");
    }

    #[test]
    fn oversized_sessions_are_not_saved_and_keep_the_old_store() {
        let t = tempfile::tempdir().unwrap();
        let cfg = RunnerConfig::for_tests(t.path().join("data"));
        let home = t.path().join("home");
        write(&home.join(".claude/projects/p/a.jsonl"), "old");
        assert!(save(&cfg, &spec(K1, None), &home).unwrap());
        let big = home.join(".claude/projects/p/big.bin");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_BYTES + 1).unwrap();
        assert!(!save(&cfg, &spec(K1, None), &home).unwrap());
        let fresh = t.path().join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        restore(&cfg, &spec(K1, None), &fresh).unwrap();
        assert!(fresh.join(".claude/projects/p/a.jsonl").is_file());
        assert!(!fresh.join(".claude/projects/p/big.bin").exists());
    }

    #[test]
    fn invalid_specs_are_refused_and_old_stores_swept() {
        let t = tempfile::tempdir().unwrap();
        let cfg = RunnerConfig::for_tests(t.path().join("data"));
        let home = t.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        for bad in [
            SessionSpec {
                key: "../x".into(),
                ..spec(K1, None)
            },
            SessionSpec {
                scope: K1.into(),
                ..spec(K1, None)
            },
            SessionSpec {
                seed: Some("x".into()),
                ..spec(K1, None)
            },
            SessionSpec {
                paths: vec!["../etc".into()],
                ..spec(K1, None)
            },
            SessionSpec {
                paths: vec!["/abs".into()],
                ..spec(K1, None)
            },
            SessionSpec {
                paths: vec!["a*/b".into()],
                ..spec(K1, None)
            },
            SessionSpec {
                exclude: vec!["a/b".into()],
                ..spec(K1, None)
            },
        ] {
            assert!(save(&cfg, &bad, &home).is_err(), "{bad:?}");
            assert!(restore(&cfg, &bad, &home).is_err(), "{bad:?}");
        }
        write(&home.join(".claude/projects/p/a.jsonl"), "x");
        assert!(save(&cfg, &spec(K1, None), &home).unwrap());
        assert_eq!(sweep(&cfg, Duration::from_secs(3600)), 0);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(sweep(&cfg, Duration::from_millis(1)), 1);
        let fresh = t.path().join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        assert_eq!(restore(&cfg, &spec(K1, None), &fresh).unwrap(), None);
        // Project removal drops its stores.
        assert!(save(&cfg, &spec(K1, None), &home).unwrap());
        remove_scope(&cfg, "11111111-1111-4111-8111-111111111111");
        assert!(!root(&cfg)
            .join("11111111-1111-4111-8111-111111111111")
            .exists());
    }
}
