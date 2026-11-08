//! Setup-phase support (WP02): dependency cache per project and the search
//! for secret values left in files after a run.
use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use queued_common::{CacheInfo, CacheSpec};
use sha2::{Digest, Sha256};

use crate::RunnerConfig;

/// Mount point of the cache inside containers.
pub const CONTAINER_CACHE: &str = "/cache";
/// Lock files larger than this count by size only (agent-controlled content).
const MAX_KEY_FILE: u64 = 64 * 1024 * 1024;
const MAX_KEY_FILES: usize = 64;

pub fn caches_root(cfg: &RunnerConfig, workspace: &str) -> PathBuf {
    cfg.data_dir.join("caches").join(workspace)
}

fn safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Cache key: SHA-256 over the material and, per file, its path and the hash
/// of its content (regular files only – a symlink planted by the agent is
/// neither followed nor read; missing files count as absent).
pub async fn cache_key(workspace: &Path, spec: &CacheSpec) -> Result<String> {
    anyhow::ensure!(
        spec.files.len() <= MAX_KEY_FILES,
        "too many cache key files"
    );
    let mut h = Sha256::new();
    h.update(spec.key_material.as_bytes());
    h.update([0]);
    for f in &spec.files {
        anyhow::ensure!(safe_rel(f), "invalid cache key file path");
        h.update(f.as_bytes());
        h.update([0]);
        let p = workspace.join(f);
        match tokio::fs::symlink_metadata(&p).await {
            Ok(m) if m.file_type().is_file() && m.len() <= MAX_KEY_FILE => {
                h.update(Sha256::digest(tokio::fs::read(&p).await?));
            }
            Ok(m) if m.file_type().is_file() => h.update(format!("size:{}", m.len())),
            Ok(_) => h.update(b"not-a-file"),
            Err(_) => h.update(b"absent"),
        }
        h.update([0]);
    }
    Ok(h.finalize()
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect())
}

pub struct Prepared {
    pub dir: PathBuf,
    pub info: CacheInfo,
}

/// Creates (or reuses) the cache directory for this key.
pub async fn prepare(
    cfg: &RunnerConfig,
    workspace_id: &str,
    workspace: &Path,
    spec: &CacheSpec,
) -> Result<Prepared> {
    // A different sandbox image means different toolchains: new cache.
    let spec = CacheSpec {
        key_material: format!("{}\0{}", cfg.image, spec.key_material),
        files: spec.files.clone(),
    };
    let key = cache_key(workspace, &spec).await?;
    let dir = caches_root(cfg, workspace_id).join(&key);
    let hit = tokio::fs::symlink_metadata(&dir)
        .await
        .is_ok_and(|m| m.file_type().is_dir());
    tokio::fs::create_dir_all(&dir).await?;
    Ok(Prepared {
        dir,
        info: CacheInfo { key, hit },
    })
}

/// After the setup run: on success older keys of the project are removed;
/// a new cache of a failed setup is discarded (half-filled, maybe broken).
pub async fn finish(p: &Prepared, success: bool) {
    if success {
        let Some(root) = p.dir.parent() else { return };
        if let Ok(mut rd) = tokio::fs::read_dir(root).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                if e.path() != p.dir {
                    let _ = tokio::fs::remove_dir_all(e.path()).await;
                }
            }
        }
    } else if !p.info.hit {
        let _ = tokio::fs::remove_dir_all(&p.dir).await;
    }
}

/// Variables that point common package managers into the cache.
pub fn cache_env(root: &str) -> Vec<(String, String)> {
    [
        ("QUEUE_CACHE_DIR", ""),
        ("XDG_CACHE_HOME", "/xdg"),
        ("npm_config_cache", "/npm"),
        ("YARN_CACHE_FOLDER", "/yarn"),
        ("PIP_CACHE_DIR", "/pip"),
        ("UV_CACHE_DIR", "/uv"),
        ("CARGO_HOME", "/cargo"),
        ("GOMODCACHE", "/go/mod"),
        ("GOCACHE", "/go/build"),
    ]
    .iter()
    .map(|(k, sub)| (k.to_string(), format!("{root}{sub}")))
    .collect()
}

/// Limits of the secret search (a huge `node_modules` must not stall a run).
const MAX_SCAN_FILE: u64 = 8 * 1024 * 1024;
const MAX_SCAN_ENTRIES: usize = 300_000;
const MAX_SCAN_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_LEAKS: usize = 20;

#[derive(Debug, Default, PartialEq)]
pub struct ScanResult {
    /// `"NAME in <label>/<path>"`.
    pub leaks: Vec<String>,
    /// All files were searched (limits not reached).
    pub complete: bool,
}

/// Searches regular files under each `(label, root)` for the secret values
/// (`(name, value)`). Symlinks are never followed; files over 8 MiB are
/// skipped. Blocking – call from `spawn_blocking`.
pub fn scan(roots: &[(String, PathBuf)], secrets: &[(String, String)]) -> ScanResult {
    let finders: Vec<(&str, memchr::memmem::Finder)> = secrets
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(n, v)| (n.as_str(), memchr::memmem::Finder::new(v.as_bytes())))
        .collect();
    let mut res = ScanResult {
        leaks: vec![],
        complete: true,
    };
    if finders.is_empty() {
        return res;
    }
    let (mut entries, mut bytes) = (0usize, 0u64);
    for (label, root) in roots {
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                entries += 1;
                if entries > MAX_SCAN_ENTRIES || bytes > MAX_SCAN_BYTES {
                    res.complete = false;
                    return res;
                }
                let Ok(m) = std::fs::symlink_metadata(e.path()) else {
                    continue;
                };
                if m.file_type().is_dir() {
                    stack.push(e.path());
                } else if m.file_type().is_file() && m.len() <= MAX_SCAN_FILE {
                    let Ok(data) = std::fs::read(e.path()) else {
                        continue;
                    };
                    bytes += data.len() as u64;
                    for (name, f) in &finders {
                        if f.find(&data).is_some() {
                            let rel = e.path();
                            let rel = rel.strip_prefix(root).unwrap_or(&rel);
                            res.leaks
                                .push(format!("{name} in {label}/{}", rel.display()));
                            if res.leaks.len() >= MAX_LEAKS {
                                return res;
                            }
                        }
                    }
                }
            }
        }
    }
    res
}

/// Removes leaked secret values from the workspace so they can neither be
/// read by a later agent run nor end up in a commit (the next `begin`
/// commits stray files): untracked files are deleted, tracked files are
/// restored from HEAD, files under `.git/` lose the lines containing a
/// value. Returns one note per file (`"NAME in workspace/x: entfernt"`).
pub async fn scrub(
    workspace: &Path,
    leaks: &[String],
    secrets: &[(String, String)],
) -> Vec<String> {
    let mut notes = vec![];
    for leak in leaks {
        let Some((_, rest)) = leak.split_once(" in workspace/") else {
            notes.push(format!("{leak}: Cache verworfen"));
            continue;
        };
        if !safe_rel(rest) {
            continue;
        }
        let path = workspace.join(rest);
        let action = if rest.starts_with(".git/") {
            match tokio::fs::read_to_string(&path).await {
                Ok(text) => {
                    let kept: Vec<&str> = text
                        .lines()
                        .filter(|l| {
                            !secrets
                                .iter()
                                .any(|(_, v)| !v.is_empty() && l.contains(v.as_str()))
                        })
                        .collect();
                    let _ = tokio::fs::write(&path, kept.join("\n") + "\n").await;
                    "Zeilen entfernt"
                }
                Err(_) => {
                    let _ = tokio::fs::remove_file(&path).await;
                    "entfernt"
                }
            }
        } else if git_ok(workspace, &["ls-files", "--error-unmatch", "--", rest]).await {
            if git_ok(workspace, &["checkout", "HEAD", "--", rest]).await {
                "auf den Stand im Repository zurückgesetzt"
            } else {
                let _ = tokio::fs::remove_file(&path).await;
                "entfernt"
            }
        } else {
            let _ = tokio::fs::remove_file(&path).await;
            "entfernt"
        };
        notes.push(format!("{leak}: {action}"));
    }
    notes
}

/// Git on the (agent- and setup-controlled) workspace: hardened like every
/// other git call of the runner (WP03), never with the repository's hooks,
/// filters or other configured programs.
async fn git_ok(dir: &Path, args: &[&str]) -> bool {
    let repo = crate::git::Repo {
        dir: dir.to_path_buf(),
    };
    if repo.harden().await.is_err() {
        return false;
    }
    tokio::process::Command::new("git")
        .args(crate::git::HARDEN)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Replaces every secret value in an output line by `***`.
pub fn redact(line: String, secrets: &[String]) -> String {
    let mut out = line;
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), "***");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn key_depends_on_material_and_lockfile_content() {
        let t = tempfile::tempdir().unwrap();
        let spec = CacheSpec {
            key_material: "npm ci".into(),
            files: vec!["package-lock.json".into()],
        };
        let absent = cache_key(t.path(), &spec).await.unwrap();
        std::fs::write(t.path().join("package-lock.json"), "{\"v\":1}").unwrap();
        let one = cache_key(t.path(), &spec).await.unwrap();
        assert_ne!(absent, one);
        assert_eq!(one, cache_key(t.path(), &spec).await.unwrap(), "stable");
        std::fs::write(t.path().join("package-lock.json"), "{\"v\":2}").unwrap();
        let two = cache_key(t.path(), &spec).await.unwrap();
        assert_ne!(one, two, "lockfile change invalidates");
        let other = CacheSpec {
            key_material: "npm install".into(),
            ..spec.clone()
        };
        assert_ne!(
            two,
            cache_key(t.path(), &other).await.unwrap(),
            "script change invalidates"
        );
        assert_eq!(two.len(), 32);
        // A symlink is not followed (content of the target never matters).
        std::fs::remove_file(t.path().join("package-lock.json")).unwrap();
        std::fs::write(t.path().join("target.json"), "{\"v\":2}").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            t.path().join("target.json"),
            t.path().join("package-lock.json"),
        )
        .unwrap();
        #[cfg(unix)]
        assert_ne!(two, cache_key(t.path(), &spec).await.unwrap());
        for bad in ["../x", "/etc/passwd", ""] {
            let s = CacheSpec {
                key_material: String::new(),
                files: vec![bad.into()],
            };
            assert!(cache_key(t.path(), &s).await.is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn prepare_hits_and_finish_prunes_or_discards() {
        let t = tempfile::tempdir().unwrap();
        let cfg = RunnerConfig::for_tests(t.path().to_path_buf());
        let ws = t.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let spec = |m: &str| CacheSpec {
            key_material: m.into(),
            files: vec![],
        };
        let a = prepare(&cfg, "p1", &ws, &spec("a")).await.unwrap();
        assert!(!a.info.hit && a.dir.is_dir());
        // Failed first setup: the new cache is discarded.
        finish(&a, false).await;
        assert!(!a.dir.exists());
        let a = prepare(&cfg, "p1", &ws, &spec("a")).await.unwrap();
        finish(&a, true).await;
        let again = prepare(&cfg, "p1", &ws, &spec("a")).await.unwrap();
        assert!(again.info.hit);
        // A failed run on an existing cache keeps it.
        finish(&again, false).await;
        assert!(again.dir.is_dir());
        let b = prepare(&cfg, "p1", &ws, &spec("b")).await.unwrap();
        finish(&b, true).await;
        assert!(b.dir.is_dir() && !a.dir.exists(), "old key pruned");
    }

    #[test]
    fn scan_finds_secrets_without_following_symlinks() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        std::fs::create_dir_all(ws.join("deep/er")).unwrap();
        std::fs::write(ws.join("ok.txt"), "nothing here").unwrap();
        std::fs::write(
            ws.join("deep/er/.npmrc"),
            "//r/:_authToken=tok-SECRET-123\n",
        )
        .unwrap();
        std::fs::write(
            ws.join(".git/config"),
            "[http] extraHeader = tok-SECRET-123",
        )
        .unwrap();
        let outside = t.path().join("outside.txt");
        std::fs::write(&outside, "tok-SECRET-123").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, ws.join("link.txt")).unwrap();
        let r = scan(
            &[("workspace".into(), ws.clone())],
            &[
                ("NPM_TOKEN".into(), "tok-SECRET-123".into()),
                ("OTHER".into(), "not-present-1".into()),
            ],
        );
        assert!(r.complete);
        let mut leaks = r.leaks.clone();
        leaks.sort();
        assert_eq!(
            leaks,
            vec![
                "NPM_TOKEN in workspace/.git/config".to_string(),
                "NPM_TOKEN in workspace/deep/er/.npmrc".to_string()
            ]
        );
        assert_eq!(
            scan(&[("w".into(), ws)], &[]),
            ScanResult {
                leaks: vec![],
                complete: true
            }
        );
    }

    #[tokio::test]
    async fn scrub_removes_untracked_restores_tracked_and_cleans_git_config() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path();
        let git = |a: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(a)
                .current_dir(ws)
                .status()
                .unwrap()
                .success())
        };
        git(&["init", "-q"]);
        std::fs::write(ws.join("tracked.cfg"), "token=none\n").unwrap();
        git(&["add", "tracked.cfg"]);
        git(&["commit", "-q", "-m", "init"]);
        let secret = "s3cret-token-xyz";
        std::fs::write(ws.join("tracked.cfg"), format!("token={secret}\n")).unwrap();
        std::fs::write(ws.join(".npmrc"), format!("//r/:_authToken={secret}\n")).unwrap();
        let cfg = std::fs::read_to_string(ws.join(".git/config")).unwrap();
        std::fs::write(
            ws.join(".git/config"),
            format!("{cfg}[http]\n\textraHeader = Bearer {secret}\n"),
        )
        .unwrap();
        let secrets = vec![("TOKEN".to_string(), secret.to_string())];
        let found = scan(&[("workspace".into(), ws.to_path_buf())], &secrets);
        assert_eq!(found.leaks.len(), 3, "{:?}", found.leaks);
        let notes = scrub(ws, &found.leaks, &secrets).await;
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(!ws.join(".npmrc").exists());
        assert_eq!(
            std::fs::read_to_string(ws.join("tracked.cfg")).unwrap(),
            "token=none\n"
        );
        let cfg_after = std::fs::read_to_string(ws.join(".git/config")).unwrap();
        assert!(!cfg_after.contains(secret) && cfg_after.contains("[core]"));
        assert!(scan(&[("workspace".into(), ws.to_path_buf())], &secrets)
            .leaks
            .is_empty());
        git(&["status", "--porcelain"]);
    }

    #[test]
    fn redacts_every_occurrence() {
        assert_eq!(
            redact(
                "a s3cret-value b s3cret-value".into(),
                &["s3cret-value".into(), String::new()]
            ),
            "a *** b ***"
        );
        assert_eq!(redact("clean".into(), &["x-secret".into()]), "clean");
    }

    #[test]
    fn cache_env_points_into_root() {
        let e = cache_env("/cache");
        assert!(e.contains(&("npm_config_cache".into(), "/cache/npm".into())));
        assert!(e.contains(&("QUEUE_CACHE_DIR".into(), "/cache".into())));
    }
}
