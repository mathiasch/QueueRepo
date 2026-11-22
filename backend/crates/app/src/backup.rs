//! WP11: backups and restore – an operations command (`queued backup`,
//! `queued restore`), started by an operator or a systemd timer as the
//! service user, never by the server process.
//!
//! A backup is a directory (`0700`, files `0600`):
//! - `db.dump.qbk` – `pg_dump --format=custom`,
//! - `workspaces/<runner>/<workspace>.bundle.qbk` – `git bundle --all` of
//!   every workspace, made by the runner that owns it,
//! - `app/vapid_private.key.qbk` – the Web Push key of the app,
//! - `manifest.json` (no secrets: sizes, SHA-256 of the plaintext, HEAD refs)
//!   and `manifest.mac` (HMAC-SHA-256 with the backup key).
//!
//! Every file is encrypted with `BACKUP_KEY` (not `MASTER_KEY`, which is
//! never part of a backup): AES-256-GCM in 64 KiB chunks, nonce = 7 random
//! bytes ‖ chunk counter ‖ last-chunk flag, the file's path as associated
//! data – truncation, reordering, swapping files and every changed byte are
//! detected. Plaintext backups need an explicit `--unencrypted`.
//! A backup is written to `<name>.partial` and renamed when complete.
use std::{
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
};

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures::StreamExt;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{fleet, runner_client::RunnerClient, state::AppState};

pub const MAGIC: &[u8; 4] = b"QBK1";
pub const CHUNK: usize = 64 * 1024;
const TAG: usize = 16;
pub const SUFFIX: &str = ".qbk";
pub const FORMAT_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Stream encryption
// ---------------------------------------------------------------------------

fn nonce(prefix: &[u8; 7], counter: u32, last: bool) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[..7].copy_from_slice(prefix);
    n[7..11].copy_from_slice(&counter.to_be_bytes());
    n[11] = u8::from(last);
    n
}

/// Encrypts a stream of plaintext chunks (any sizes) into `out`.
pub struct Encryptor {
    cipher: Aes256Gcm,
    prefix: [u8; 7],
    aad: Vec<u8>,
    counter: u32,
    buf: Vec<u8>,
    header_written: bool,
}

impl Encryptor {
    pub fn new(key: &[u8; 32], path: &str) -> Self {
        let mut prefix = [0u8; 7];
        rand::thread_rng().fill_bytes(&mut prefix);
        Self {
            cipher: Aes256Gcm::new_from_slice(key).expect("32-byte key"),
            prefix,
            aad: path.as_bytes().to_vec(),
            counter: 0,
            buf: Vec::with_capacity(CHUNK),
            header_written: false,
        }
    }

    fn header(&mut self, out: &mut Vec<u8>) {
        if !self.header_written {
            out.extend_from_slice(MAGIC);
            out.extend_from_slice(&self.prefix);
            self.header_written = true;
        }
    }

    fn seal(&mut self, chunk: &[u8], last: bool, out: &mut Vec<u8>) -> Result<()> {
        let mut aad = Vec::with_capacity(11 + self.aad.len());
        aad.extend_from_slice(MAGIC);
        aad.extend_from_slice(&self.prefix);
        aad.extend_from_slice(&self.aad);
        let ct = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce(&self.prefix, self.counter, last)),
                Payload {
                    msg: chunk,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow!("encryption failed"))?;
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or_else(|| anyhow!("file too large for one backup stream"))?;
        out.extend_from_slice(&ct);
        Ok(())
    }

    /// Ciphertext for the given plaintext (full chunks only; the rest waits).
    pub fn update(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        let mut out = vec![];
        self.header(&mut out);
        self.buf.extend_from_slice(data);
        // Keep at least one byte back: the last chunk must carry the flag.
        while self.buf.len() > CHUNK {
            let rest = self.buf.split_off(CHUNK);
            let chunk = std::mem::replace(&mut self.buf, rest);
            self.seal(&chunk, false, &mut out)?;
        }
        Ok(out)
    }

    pub fn finish(mut self) -> Result<Vec<u8>> {
        let mut out = vec![];
        self.header(&mut out);
        let chunk = std::mem::take(&mut self.buf);
        self.seal(&chunk, true, &mut out)?;
        Ok(out)
    }
}

/// Decrypts what [`Encryptor`] wrote; fails on any change, truncation or
/// a wrong key/path.
pub struct Decryptor {
    cipher: Aes256Gcm,
    prefix: Option<[u8; 7]>,
    aad: Vec<u8>,
    path: Vec<u8>,
    counter: u32,
    buf: Vec<u8>,
    done: bool,
}

impl Decryptor {
    pub fn new(key: &[u8; 32], path: &str) -> Self {
        Self {
            cipher: Aes256Gcm::new_from_slice(key).expect("32-byte key"),
            prefix: None,
            aad: vec![],
            path: path.as_bytes().to_vec(),
            counter: 0,
            buf: vec![],
            done: false,
        }
    }

    fn open(&mut self, chunk: &[u8], last: bool) -> Result<Vec<u8>> {
        let prefix = self.prefix.expect("header read");
        let pt = self
            .cipher
            .decrypt(
                Nonce::from_slice(&nonce(&prefix, self.counter, last)),
                Payload {
                    msg: chunk,
                    aad: &self.aad,
                },
            )
            .map_err(|_| anyhow!("backup file is damaged, truncated or the key is wrong"))?;
        self.counter += 1;
        Ok(pt)
    }

    pub fn update(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if self.done {
            if data.is_empty() {
                return Ok(vec![]);
            }
            bail!("backup file has data after its last chunk");
        }
        self.buf.extend_from_slice(data);
        if self.prefix.is_none() {
            if self.buf.len() < 11 {
                return Ok(vec![]);
            }
            if &self.buf[..4] != MAGIC {
                bail!("not an encrypted backup file");
            }
            let mut p = [0u8; 7];
            p.copy_from_slice(&self.buf[4..11]);
            self.prefix = Some(p);
            self.aad = [&self.buf[..11], &self.path[..]].concat();
            self.buf.drain(..11);
        }
        let mut out = vec![];
        // A full sealed chunk followed by more data is never the last one.
        while self.buf.len() > CHUNK + TAG {
            let rest = self.buf.split_off(CHUNK + TAG);
            let chunk = std::mem::replace(&mut self.buf, rest);
            out.extend(self.open(&chunk, false)?);
        }
        Ok(out)
    }

    pub fn finish(mut self) -> Result<Vec<u8>> {
        if self.prefix.is_none() {
            bail!("backup file is truncated");
        }
        let chunk = std::mem::take(&mut self.buf);
        let pt = self.open(&chunk, true)?;
        self.done = true;
        Ok(pt)
    }
}

// ---------------------------------------------------------------------------
// Keys, manifest
// ---------------------------------------------------------------------------

/// `BACKUP_KEY` (base64 or hex of 32 bytes) or `BACKUP_KEY_FILE`.
pub fn load_key(env: &dyn Fn(&str) -> Option<String>) -> Result<Option<[u8; 32]>> {
    let raw = match (env("BACKUP_KEY"), env("BACKUP_KEY_FILE")) {
        (Some(k), _) => k,
        (None, Some(f)) => {
            let meta = std::fs::metadata(&f).with_context(|| format!("BACKUP_KEY_FILE {f}"))?;
            if meta.permissions().mode() & 0o077 != 0 {
                bail!("BACKUP_KEY_FILE {f} must not be readable by group/others (chmod 600)");
            }
            std::fs::read_to_string(&f).with_context(|| format!("read {f}"))?
        }
        (None, None) => return Ok(None),
    };
    parse_key(raw.trim()).map(Some)
}

pub fn parse_key(s: &str) -> Result<[u8; 32]> {
    let bytes = if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        (0..32)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16))
            .collect::<std::result::Result<Vec<u8>, _>>()?
    } else {
        STANDARD
            .decode(s)
            .map_err(|_| anyhow!("backup key: expected 32 bytes as base64 or 64 hex digits"))?
    };
    bytes
        .try_into()
        .map_err(|_| anyhow!("backup key must be exactly 32 bytes"))
}

/// Writes a new random key file (`0600`); refuses to overwrite.
pub fn generate_key_file(path: &Path) -> Result<()> {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    writeln!(f, "{}", STANDARD.encode(k))?;
    Ok(())
}

/// Short public fingerprint of a key (detects a wrong key before work).
pub fn key_id(key: &[u8; 32]) -> String {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("key");
    m.update(b"queue-backup-key-id-v1");
    m.finalize().into_bytes()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn mac(key: &[u8; 32], data: &[u8]) -> String {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("key");
    m.update(b"queue-backup-manifest-v1");
    m.update(data);
    m.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    /// Relative path inside the backup (without the `.qbk` suffix).
    pub path: String,
    pub bytes: u64,
    /// SHA-256 of the plaintext.
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceEntry {
    pub runner: Uuid,
    pub id: String,
    pub head: String,
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub format: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub app_version: String,
    pub encrypted: bool,
    pub key_id: Option<String>,
    pub files: Vec<FileEntry>,
    pub workspaces: Vec<WorkspaceEntry>,
}

fn private_dir(p: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(p)
        .with_context(|| format!("create {}", p.display()))?;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

async fn private_file(p: &Path) -> Result<tokio::fs::File> {
    if let Some(d) = p.parent() {
        private_dir(d)?;
    }
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(p)
        .with_context(|| format!("create {}", p.display()))?;
    Ok(tokio::fs::File::from_std(f))
}

/// Writes a byte stream into the backup (encrypted unless `key` is `None`).
async fn write_stream<S>(
    dir: &Path,
    rel: &str,
    key: Option<&[u8; 32]>,
    mut s: S,
) -> Result<FileEntry>
where
    S: futures::Stream<Item = Result<bytes::Bytes>> + Unpin,
{
    let name = if key.is_some() {
        format!("{rel}{SUFFIX}")
    } else {
        rel.to_string()
    };
    let mut f = private_file(&dir.join(&name)).await?;
    let mut enc = key.map(|k| Encryptor::new(k, rel));
    let mut sha = Sha256::new();
    let mut n = 0u64;
    while let Some(chunk) = s.next().await {
        let chunk = chunk?;
        sha.update(&chunk);
        n += chunk.len() as u64;
        match &mut enc {
            Some(e) => f.write_all(&e.update(&chunk)?).await?,
            None => f.write_all(&chunk).await?,
        }
    }
    if let Some(e) = enc {
        f.write_all(&e.finish()?).await?;
    }
    f.flush().await?;
    f.sync_all().await?;
    Ok(FileEntry {
        path: rel.to_string(),
        bytes: n,
        sha256: hex(&sha.finalize()),
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Reads a file of the backup as plaintext chunks, checking the AEAD (and
/// at the end the SHA-256 of the manifest).
fn read_stream(
    dir: PathBuf,
    entry: FileEntry,
    key: Option<[u8; 32]>,
) -> impl futures::Stream<Item = Result<bytes::Bytes>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes>>(4);
    tokio::spawn(async move {
        let res: Result<()> = async {
            let name = if key.is_some() {
                format!("{}{SUFFIX}", entry.path)
            } else {
                entry.path.clone()
            };
            let mut f = tokio::fs::File::open(dir.join(&name))
                .await
                .with_context(|| format!("open {name}"))?;
            let mut dec = key.map(|k| Decryptor::new(&k, &entry.path));
            let mut sha = Sha256::new();
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = f.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                let pt = match &mut dec {
                    Some(d) => d.update(&buf[..n])?,
                    None => buf[..n].to_vec(),
                };
                if !pt.is_empty() {
                    sha.update(&pt);
                    if tx.send(Ok(pt.into())).await.is_err() {
                        return Ok(());
                    }
                }
            }
            if let Some(d) = dec {
                let pt = d.finish()?;
                if !pt.is_empty() {
                    sha.update(&pt);
                    let _ = tx.send(Ok(pt.into())).await;
                }
            }
            if hex(&sha.finalize()) != entry.sha256 {
                bail!("{}: checksum does not match the manifest", entry.path);
            }
            Ok(())
        }
        .await;
        if let Err(e) = res {
            let _ = tx.send(Err(e)).await;
        }
    });
    tokio_stream::wrappers::ReceiverStream::new(rx)
}

/// Checks a whole file (AEAD + checksum) without using its content.
pub async fn verify_file(dir: &Path, entry: &FileEntry, key: Option<[u8; 32]>) -> Result<()> {
    let mut s = Box::pin(read_stream(dir.to_path_buf(), entry.clone(), key));
    while let Some(c) = s.next().await {
        c?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// PostgreSQL
// ---------------------------------------------------------------------------

/// Connection of `pg_dump`/`pg_restore` through `PG*` variables: the
/// password never appears on a command line (`/proc/<pid>/cmdline`).
pub fn pg_env(database_url: &str) -> Result<Vec<(&'static str, String)>> {
    let u = url::Url::parse(database_url).context("DATABASE_URL")?;
    let dec = |s: &str| {
        url::form_urlencoded::parse(format!("x={s}").as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default()
    };
    let mut env = vec![
        ("PGHOST", u.host_str().unwrap_or("localhost").to_string()),
        ("PGPORT", u.port().unwrap_or(5432).to_string()),
        ("PGUSER", dec(u.username())),
        ("PGDATABASE", u.path().trim_start_matches('/').to_string()),
    ];
    if let Some(p) = u.password() {
        env.push(("PGPASSWORD", dec(p)));
    }
    for (k, v) in u.query_pairs() {
        if k == "sslmode" {
            env.push(("PGSSLMODE", v.into_owned()));
        }
    }
    Ok(env)
}

fn pg_command(bin: &str, database_url: &str) -> Result<tokio::process::Command> {
    let mut c = tokio::process::Command::new(bin);
    for (k, v) in pg_env(database_url)? {
        c.env(k, v);
    }
    c.env_remove("PGSERVICE").env_remove("PGPASSFILE");
    Ok(c)
}

// ---------------------------------------------------------------------------
// Backup
// ---------------------------------------------------------------------------

pub struct BackupOptions {
    pub out: PathBuf,
    pub key: Option<[u8; 32]>,
    /// Complete backups kept in `out` (older ones are removed; 0 = all).
    pub keep: usize,
}

/// Runner clients of all runners that are not revoked.
async fn runner_clients(st: &AppState) -> Result<Vec<(Uuid, RunnerClient)>> {
    fleet::refresh(st).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM runners WHERE status <> 'revoked' ORDER BY NOT builtin, created_at",
    )
    .fetch_all(&st.db)
    .await?;
    Ok(ids
        .into_iter()
        .filter_map(|id| st.fleet.client(id).map(|c| (id, c)))
        .collect())
}

/// Makes a complete backup; returns its directory.
pub async fn run(st: &AppState, o: &BackupOptions) -> Result<PathBuf> {
    private_dir(&o.out)?;
    let name = format!(
        "queue-backup-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
    );
    let partial = o.out.join(format!("{name}.partial"));
    private_dir(&partial)?;
    let key = o.key.as_ref();
    let mut files = vec![];

    // 1. Database.
    let mut child = pg_command("pg_dump", &st.cfg.database_url)?
        .args(["--format=custom", "--no-owner", "--no-privileges"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("start pg_dump")?;
    let stdout = child.stdout.take().context("pg_dump stdout")?;
    let mut stderr = child.stderr.take().context("pg_dump stderr")?;
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let stream = tokio_util::io::ReaderStream::new(stdout).map(|r| r.map_err(anyhow::Error::from));
    let db = write_stream(&partial, "db.dump", key, stream).await?;
    let status = child.wait().await?;
    let err = err_task.await.unwrap_or_default();
    if !status.success() {
        bail!("pg_dump failed: {}", err.trim());
    }
    files.push(db);

    // 2. Workspaces of every runner.
    let mut workspaces = vec![];
    for (rid, client) in runner_clients(st).await? {
        let list = client
            .workspaces()
            .await
            .with_context(|| format!("runner {rid}: list workspaces (backup incomplete)"))?;
        for ws in list {
            let (head, resp) = client
                .bundle(&ws)
                .await
                .with_context(|| format!("runner {rid}: bundle of {ws}"))?;
            let rel = format!("workspaces/{rid}/{ws}.bundle");
            let s = resp.bytes_stream().map(|r| r.map_err(anyhow::Error::from));
            let e = write_stream(&partial, &rel, key, s).await?;
            workspaces.push(WorkspaceEntry {
                runner: rid,
                id: ws,
                head,
                file: rel,
            });
            files.push(e);
        }
    }

    // 3. App keys (never MASTER_KEY).
    let vapid = st.cfg.data_dir.join("vapid_private.key");
    if vapid.exists() {
        let data = tokio::fs::read(&vapid).await?;
        let s = futures::stream::iter([Ok(bytes::Bytes::from(data))]);
        files.push(write_stream(&partial, "app/vapid_private.key", key, s).await?);
    }

    // 4. Manifest + MAC, then the directory becomes final.
    let m = Manifest {
        format: FORMAT_VERSION,
        created_at: chrono::Utc::now(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        encrypted: key.is_some(),
        key_id: key.map(key_id),
        files,
        workspaces,
    };
    let json = serde_json::to_vec_pretty(&m)?;
    let mut f = private_file(&partial.join("manifest.json")).await?;
    f.write_all(&json).await?;
    f.sync_all().await?;
    if let Some(k) = key {
        let mut f = private_file(&partial.join("manifest.mac")).await?;
        f.write_all(mac(k, &json).as_bytes()).await?;
        f.sync_all().await?;
    }
    let done = o.out.join(&name);
    tokio::fs::rename(&partial, &done).await?;
    let bytes: u64 = m.files.iter().map(|f| f.bytes).sum();
    crate::audit::system(
        st,
        "admin.backup_created",
        "backup",
        &name,
        None,
        serde_json::json!({"workspaces": m.workspaces.len(), "bytes": bytes, "encrypted": m.encrypted}),
    )
    .await;
    if o.keep > 0 {
        prune(&o.out, o.keep)?;
    }
    Ok(done)
}

/// Keeps the newest `keep` complete backups (and removes stale `.partial`
/// directories older than a day).
pub fn prune(out: &Path, keep: usize) -> Result<Vec<PathBuf>> {
    let mut complete = vec![];
    let mut removed = vec![];
    for e in std::fs::read_dir(out)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("queue-backup-") || !e.file_type()?.is_dir() {
            continue;
        }
        if name.ends_with(".partial") {
            let old = e
                .metadata()?
                .modified()?
                .elapsed()
                .is_ok_and(|d| d > std::time::Duration::from_secs(86_400));
            if old {
                std::fs::remove_dir_all(e.path())?;
                removed.push(e.path());
            }
            continue;
        }
        if e.path().join("manifest.json").exists() {
            complete.push(e.path());
        }
    }
    complete.sort();
    while complete.len() > keep {
        let p = complete.remove(0);
        std::fs::remove_dir_all(&p)?;
        removed.push(p);
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------
// Restore
// ---------------------------------------------------------------------------

/// Reads and authenticates the manifest of a backup.
pub fn read_manifest(dir: &Path, key: Option<&[u8; 32]>) -> Result<Manifest> {
    let json = std::fs::read(dir.join("manifest.json")).context("manifest.json")?;
    let m: Manifest = serde_json::from_slice(&json).context("manifest.json")?;
    if m.format != FORMAT_VERSION {
        bail!("unsupported backup format {}", m.format);
    }
    match (m.encrypted, key) {
        (true, None) => bail!("the backup is encrypted: set BACKUP_KEY or BACKUP_KEY_FILE"),
        (true, Some(k)) => {
            if m.key_id.as_deref() != Some(key_id(k).as_str()) {
                bail!("BACKUP_KEY does not belong to this backup");
            }
            let want = std::fs::read_to_string(dir.join("manifest.mac")).context("manifest.mac")?;
            if want.trim() != mac(k, &json) {
                bail!("manifest.mac does not match: the manifest was changed");
            }
        }
        (false, _) => {}
    }
    for f in &m.files {
        if f.path.contains("..") || f.path.starts_with('/') {
            bail!("invalid path in manifest: {}", f.path);
        }
    }
    Ok(m)
}

pub struct RestoreOptions {
    pub from: PathBuf,
    pub key: Option<[u8; 32]>,
    /// Restore every workspace to this runner and bind all projects to it
    /// (the original runners are gone). `None` = to the recorded runners.
    pub target_runner: Option<Uuid>,
}

#[derive(Debug, Default)]
pub struct RestoreReport {
    pub workspaces: usize,
    pub bytes: u64,
}

/// Restores a backup into an **empty** database and empty runners:
/// verifies every file first, then `pg_restore --single-transaction`, then
/// the workspaces (each runner refuses to overwrite an existing one).
/// `builtin` is the client of `RUNNER_URL`; registered runners are built
/// from the restored database (their tokens need the same `MASTER_KEY`).
pub async fn restore(
    cfg: &crate::config::Config,
    builtin: RunnerClient,
    o: &RestoreOptions,
) -> Result<RestoreReport> {
    let m = read_manifest(&o.from, o.key.as_ref())?;
    let db_entry = m
        .files
        .iter()
        .find(|f| f.path == "db.dump")
        .cloned()
        .ok_or_else(|| anyhow!("backup has no database dump"))?;
    // 1. Every file intact before anything changes.
    for f in &m.files {
        verify_file(&o.from, f, o.key).await?;
    }
    // 2. Empty database only.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&cfg.database_url)
        .await
        .context("connect to postgres")?;
    let tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'",
    )
    .fetch_one(&pool)
    .await?;
    if tables > 0 {
        bail!(
            "the database is not empty ({tables} tables): restore only into a new, empty database"
        );
    }
    // 3. pg_restore from the decrypted stream, all or nothing.
    let mut child = pg_command("pg_restore", &cfg.database_url)?
        .args([
            "--no-owner",
            "--no-privileges",
            "--single-transaction",
            "--exit-on-error",
            "--dbname",
            &pg_env(&cfg.database_url)?
                .iter()
                .find(|(k, _)| *k == "PGDATABASE")
                .map(|(_, v)| v.clone())
                .unwrap_or_default(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("start pg_restore")?;
    let mut stdin = child.stdin.take().context("pg_restore stdin")?;
    let mut stderr = child.stderr.take().context("pg_restore stderr")?;
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let mut s = Box::pin(read_stream(o.from.clone(), db_entry, o.key));
    let mut feed_err = None;
    while let Some(c) = s.next().await {
        match c {
            Ok(b) => {
                if let Err(e) = stdin.write_all(&b).await {
                    feed_err = Some(anyhow!(e));
                    break;
                }
            }
            Err(e) => {
                feed_err = Some(e);
                break;
            }
        }
    }
    drop(stdin);
    if feed_err.is_some() {
        let _ = child.kill().await;
    }
    let status = child.wait().await?;
    let err = err_task.await.unwrap_or_default();
    if let Some(e) = feed_err {
        return Err(e.context(format!("pg_restore aborted: {}", err.trim())));
    }
    if !status.success() {
        bail!("pg_restore failed: {}", err.trim());
    }

    // 4. Workspaces.
    let mut clients: std::collections::HashMap<Uuid, RunnerClient> = Default::default();
    clients.insert(fleet::BUILTIN, builtin);
    let rows: Vec<(Uuid, Option<String>, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT id, url, token_enc FROM runners WHERE NOT builtin AND status <> 'revoked'",
    )
    .fetch_all(&pool)
    .await?;
    for (id, url, enc) in rows {
        if let (Some(url), Some(enc)) = (url, enc) {
            let token = String::from_utf8(crate::crypto::decrypt(&cfg.master_key, &enc).context(
                "runner token of the restored database: is MASTER_KEY the same as before?",
            )?)?;
            clients.insert(id, RunnerClient::new(&url, &token));
        }
    }
    let mut report = RestoreReport::default();
    for w in &m.workspaces {
        let target = o.target_runner.unwrap_or(w.runner);
        let client = clients.get(&target).ok_or_else(|| {
            anyhow!(
                "runner {target} of workspace {} is unknown or revoked; use --target-runner",
                w.id
            )
        })?;
        let entry = m
            .files
            .iter()
            .find(|f| f.path == w.file)
            .cloned()
            .ok_or_else(|| anyhow!("{} missing in manifest", w.file))?;
        report.bytes += entry.bytes;
        let body = reqwest::Body::wrap_stream(
            read_stream(o.from.clone(), entry, o.key).map(|r| r.map_err(std::io::Error::other)),
        );
        client
            .restore(&w.id, &w.head, body)
            .await
            .with_context(|| format!("restore of workspace {} on runner {target}", w.id))?;
        report.workspaces += 1;
    }
    if let Some(t) = o.target_runner {
        sqlx::query("UPDATE projects SET runner_id=$1")
            .bind(t)
            .execute(&pool)
            .await?;
    }

    // 5. App keys: only where none exist yet.
    if let Some(e) = m.files.iter().find(|f| f.path == "app/vapid_private.key") {
        let target = cfg.data_dir.join("vapid_private.key");
        if target.exists() {
            tracing::warn!(
                "{} exists, Web Push key of the backup not restored",
                target.display()
            );
        } else {
            let mut data = vec![];
            let mut s = Box::pin(read_stream(o.from.clone(), e.clone(), o.key));
            while let Some(c) = s.next().await {
                data.extend_from_slice(&c?);
            }
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&target)?;
            f.write_all(&data)?;
        }
    }
    crate::audit::record(
        &pool,
        crate::audit::Entry {
            actor: None,
            action: "admin.restore_completed",
            target_type: "backup",
            target_id: &o.from.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            project: None,
            details: serde_json::json!({"workspaces": report.workspaces, "created_at": m.created_at}),
        },
    )
    .await?;
    Ok(report)
}

/// Reads a whole plaintext file of a backup (tests, small files).
pub fn read_plain(path: &Path) -> Result<Vec<u8>> {
    let mut v = vec![];
    std::fs::File::open(path)?.read_to_end(&mut v)?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(key: &[u8; 32], path: &str, data: &[u8], piece: usize) -> Vec<u8> {
        let mut e = Encryptor::new(key, path);
        let mut out = vec![];
        for c in data.chunks(piece.max(1)) {
            out.extend(e.update(c).unwrap());
        }
        out.extend(e.finish().unwrap());
        out
    }

    fn dec(key: &[u8; 32], path: &str, data: &[u8], piece: usize) -> Result<Vec<u8>> {
        let mut d = Decryptor::new(key, path);
        let mut out = vec![];
        for c in data.chunks(piece.max(1)) {
            out.extend(d.update(c)?);
        }
        out.extend(d.finish()?);
        Ok(out)
    }

    #[test]
    fn roundtrip_for_all_sizes_and_feeds() {
        let k = [3u8; 32];
        for size in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 17] {
            let data: Vec<u8> = (0..size).map(|i| (i * 7 % 251) as u8).collect();
            for piece in [1000, CHUNK, 7 * CHUNK] {
                let c = enc(&k, "a/b", &data, piece);
                assert_eq!(
                    dec(&k, "a/b", &c, 4096).unwrap(),
                    data,
                    "size {size} piece {piece}"
                );
            }
        }
        // Same plaintext, different ciphertext (random nonce prefix).
        assert_ne!(enc(&k, "x", b"same", 10), enc(&k, "x", b"same", 10));
    }

    #[test]
    fn every_manipulation_is_detected() {
        let k = [3u8; 32];
        let data: Vec<u8> = (0..(2 * CHUNK + 100)).map(|i| i as u8).collect();
        let c = enc(&k, "db.dump", &data, CHUNK);
        assert!(dec(&[4u8; 32], "db.dump", &c, CHUNK).is_err(), "wrong key");
        assert!(
            dec(&k, "other", &c, CHUNK).is_err(),
            "file swapped (path is AAD)"
        );
        let mut flipped = c.clone();
        flipped[20] ^= 1;
        assert!(dec(&k, "db.dump", &flipped, CHUNK).is_err(), "changed byte");
        let full = 11 + CHUNK + TAG;
        assert!(
            dec(&k, "db.dump", &c[..full], CHUNK).is_err(),
            "cut after a full chunk"
        );
        assert!(
            dec(&k, "db.dump", &c[..c.len() - 1], CHUNK).is_err(),
            "cut tail"
        );
        assert!(dec(&k, "db.dump", &c[..5], CHUNK).is_err(), "only a header");
        let sealed = CHUNK + TAG;
        let (h, ch0, ch1, last) = (
            &c[..11],
            &c[11..11 + sealed],
            &c[11 + sealed..11 + 2 * sealed],
            &c[11 + 2 * sealed..],
        );
        let swapped = [h, ch1, ch0, last].concat();
        assert!(
            dec(&k, "db.dump", &swapped, CHUNK).is_err(),
            "chunks reordered"
        );
        let mut longer = c.clone();
        longer.extend_from_slice(&c[11..]);
        assert!(
            dec(&k, "db.dump", &longer, CHUNK).is_err(),
            "appended chunks"
        );
    }

    #[test]
    fn keys_and_pg_env() {
        let k = parse_key(&"ab".repeat(32)).unwrap();
        assert_eq!(k, [0xab; 32]);
        assert_eq!(parse_key(&STANDARD.encode([5u8; 32])).unwrap(), [5u8; 32]);
        assert!(parse_key("short").is_err());
        assert_eq!(key_id(&k).len(), 16);
        assert_ne!(key_id(&k), key_id(&[0u8; 32]));
        let env =
            pg_env("postgres://us%40er:p%40ss%2Fw@db.local:6543/queued?sslmode=require").unwrap();
        let get = |n: &str| env.iter().find(|(k, _)| *k == n).map(|(_, v)| v.clone());
        assert_eq!(get("PGUSER").as_deref(), Some("us@er"));
        assert_eq!(get("PGPASSWORD").as_deref(), Some("p@ss/w"));
        assert_eq!(get("PGHOST").as_deref(), Some("db.local"));
        assert_eq!(get("PGPORT").as_deref(), Some("6543"));
        assert_eq!(get("PGDATABASE").as_deref(), Some("queued"));
        assert_eq!(get("PGSSLMODE").as_deref(), Some("require"));
    }

    #[test]
    fn key_file_must_be_private() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("k");
        generate_key_file(&p).unwrap();
        assert!(generate_key_file(&p).is_err(), "never overwritten");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let ps = p.to_string_lossy().to_string();
        let env = |k: &str| (k == "BACKUP_KEY_FILE").then(|| ps.clone());
        assert!(load_key(&env).unwrap().is_some());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_key(&env).is_err(), "world-readable key file refused");
    }

    #[test]
    fn prune_keeps_the_newest() {
        let t = tempfile::tempdir().unwrap();
        for n in [
            "queue-backup-20260101T000000.000Z",
            "queue-backup-20260102T000000.000Z",
            "queue-backup-20260103T000000.000Z",
        ] {
            std::fs::create_dir(t.path().join(n)).unwrap();
            std::fs::write(t.path().join(n).join("manifest.json"), "{}").unwrap();
        }
        std::fs::create_dir(t.path().join("queue-backup-x.partial")).unwrap();
        let removed = prune(t.path(), 2).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!t.path().join("queue-backup-20260101T000000.000Z").exists());
        assert!(
            t.path().join("queue-backup-x.partial").exists(),
            "fresh partial kept"
        );
    }
}
