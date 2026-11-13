//! WP09 integration tests: attachments (limits, MIME forgery, EXIF/GPS,
//! rights, headers, provisioning, never in a commit, deletion), project
//! rules (proposal by an agent → approval → prompt; editor proposals;
//! conflicts; import/export with the agent-config baseline), structured
//! project state (rejected/repaired/applied only when done) and the OnEmpty
//! dedupe + critic (rules and the optional model critic with costs).
//! App + real runner (engine `none`) + Postgres; agent = mock provider;
//! the model critic talks to the local fake server (no external calls).
mod common;

use std::io::Cursor;

use base64::Engine as _;
use common::{fake_llm::start_fake, start, start_with, Client, Harness};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

async fn project(a: &Client, name: &str) -> String {
    let p = a.project(json!({"name": name})).await;
    let (code, v) = a
        .patch(
            &format!("/api/projects/{p}"),
            json!({"review_enabled": false}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    p
}

/// Raw upload; returns (status, body, headers of nothing).
async fn upload(
    a: &Client,
    p: &str,
    name: &str,
    data: Vec<u8>,
    ctype: &str,
    task: Option<&str>,
) -> (u16, Value) {
    let q: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("name", name)
        .finish();
    let mut url = format!("{}/api/projects/{p}/attachments?{q}", a.base);
    if let Some(t) = task {
        url.push_str(&format!("&task={t}"));
    }
    let r = a
        .http
        .post(url)
        .header("content-type", ctype)
        .body(data)
        .send()
        .await
        .unwrap();
    let code = r.status().as_u16();
    let text = r.text().await.unwrap();
    (
        code,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

async fn download(a: &Client, id: &str) -> (u16, reqwest::header::HeaderMap, Vec<u8>) {
    let r = a
        .http
        .get(format!("{}/api/attachments/{id}", a.base))
        .send()
        .await
        .unwrap();
    let code = r.status().as_u16();
    let h = r.headers().clone();
    (code, h, r.bytes().await.unwrap().to_vec())
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// 16×8 JPEG with an EXIF segment: orientation 6 (rotate 90°), a GPS IFD
/// (47°22' N) and the marker text "GPS-SECRET".
fn jpeg_with_gps() -> Vec<u8> {
    let img = image::RgbImage::from_fn(16, 8, |x, _| {
        if x < 8 {
            image::Rgb([255, 0, 0])
        } else {
            image::Rgb([0, 0, 255])
        }
    });
    let mut plain = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut Cursor::new(&mut plain), image::ImageFormat::Jpeg)
        .unwrap();
    let mut t: Vec<u8> = b"MM\0\x2a\0\0\0\x08".to_vec();
    t.extend_from_slice(&3u16.to_be_bytes());
    t.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]);
    t.extend_from_slice(&[0x01, 0x0E, 0, 2, 0, 0, 0, 11, 0, 0, 0, 50]);
    t.extend_from_slice(&[0x88, 0x25, 0, 4, 0, 0, 0, 1, 0, 0, 0, 62]);
    t.extend_from_slice(&[0, 0, 0, 0]);
    t.extend_from_slice(b"GPS-SECRET\0");
    t.push(0);
    t.extend_from_slice(&2u16.to_be_bytes());
    t.extend_from_slice(&[0, 1, 0, 2, 0, 0, 0, 2, b'N', 0, 0, 0]);
    t.extend_from_slice(&[0, 2, 0, 5, 0, 0, 0, 3, 0, 0, 0, 92]);
    t.extend_from_slice(&[0, 0, 0, 0]);
    for (n, d) in [(47u32, 1u32), (22, 1), (0, 1)] {
        t.extend_from_slice(&n.to_be_bytes());
        t.extend_from_slice(&d.to_be_bytes());
    }
    let mut seg = vec![0xFF, 0xE1];
    seg.extend_from_slice(&((t.len() + 8) as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&t);
    let mut out = plain[..2].to_vec();
    out.extend_from_slice(&seg);
    out.extend_from_slice(&plain[2..]);
    out
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        w,
        h,
        image::Rgba([9, 9, 9, 255]),
    ))
    .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
    .unwrap();
    out
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn upload_limits_mime_forgery_exif_rights_and_headers(pool: PgPool) {
    let h = start_with(pool, |c| c.attachment_quota_bytes = 3 * 1024 * 1024).await;
    let a = h.user("a@x.ch").await;
    let b = h.user("b@x.ch").await;
    let v = h.user("v@x.ch").await;
    let x = h.user("x@x.ch").await;
    let p = project(&a, "Anhänge").await;
    for (u, role) in [("b@x.ch", "editor"), ("v@x.ch", "viewer")] {
        let (code, r) = a
            .post(
                &format!("/api/projects/{p}/members"),
                json!({"email": u, "role": role}),
            )
            .await;
        assert_eq!(code, 200, "{r}");
    }

    // EXIF/GPS removed, orientation applied; the client's Content-Type is ignored.
    let src = jpeg_with_gps();
    assert!(contains(&src, b"GPS-SECRET"));
    let (code, att) = upload(&a, &p, "../../IMG 0001.JPG", src, "text/html", None).await;
    assert_eq!(code, 200, "{att}");
    assert_eq!(att["mime"], "image/jpeg");
    assert_eq!(
        (att["width"].as_i64(), att["height"].as_i64()),
        (Some(8), Some(16))
    );
    assert_eq!(
        att["display_name"], "IMG 0001.JPG",
        "path components dropped"
    );
    let name = att["name"].as_str().unwrap();
    assert!(
        name.starts_with("img-0001-") && name.ends_with(".jpg"),
        "{name}"
    );
    let id = att["id"].as_str().unwrap().to_string();
    let (code, hd, body) = download(&a, &id).await;
    assert_eq!(code, 200);
    assert!(!contains(&body, b"GPS-SECRET") && !contains(&body, b"Exif"));
    assert_eq!(hd["content-type"], "image/jpeg");
    assert!(hd["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment; filename=\"IMG 0001.JPG\""));
    assert_eq!(hd["x-content-type-options"], "nosniff");
    assert_eq!(hd["content-security-policy"], "default-src 'none'; sandbox");
    assert_eq!(hd["cache-control"], "private, no-store");
    let sha: String = sqlx::query_scalar("SELECT sha256 FROM attachments WHERE id=$1")
        .bind(Uuid::parse_str(&id).unwrap())
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(sha.len(), 64);

    // MIME forgery: SVG/HTML/script under image or text names, binaries,
    // a PNG header with HTML behind it – all refused (422), nothing stored.
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>"#.to_vec();
    let mut fake_png = b"\x89PNG\r\n\x1a\n".to_vec();
    fake_png.extend_from_slice(b"<html><script>alert(1)</script>");
    for (n, data, ct) in [
        ("cat.png", svg.clone(), "image/png"),
        (
            "notes.txt",
            b"<!DOCTYPE html><script>alert(1)</script>".to_vec(),
            "text/plain",
        ),
        ("pic.png", fake_png, "image/png"),
        (
            "run.txt",
            b"\x7fELF\x02\x01\x01\0\0\0\0\0".to_vec(),
            "text/plain",
        ),
        (
            "evil.exe",
            b"just text".to_vec(),
            "application/octet-stream",
        ),
        (
            "photo.heic",
            b"\0\0\0\x18ftypheic\0\0\0\0mif1heic".to_vec(),
            "image/heic",
        ),
        ("empty.txt", vec![], "text/plain"),
    ] {
        let (code, r) = upload(&a, &p, n, data, ct, None).await;
        assert_eq!(code, 422, "{n}: {r}");
    }
    let (_, r) = upload(&a, &p, "cat.svg", svg, "image/svg+xml", None).await;
    assert!(r["error"].as_str().unwrap().contains("SVG"), "{r}");

    // Size: over the per-file limit (checked) and far over (body limit).
    let max = queued_api::memory::MAX_ATTACHMENT_BYTES;
    let (code, r) = upload(&a, &p, "big.txt", vec![b'a'; max + 1], "text/plain", None).await;
    assert_eq!(code, 422, "{r}");
    let (code, _) = upload(
        &a,
        &p,
        "huge.txt",
        vec![b'a'; max + 200_000],
        "text/plain",
        None,
    )
    .await;
    assert_eq!(code, 413);

    // Quota (3 MiB for this test): the next big text no longer fits.
    let (code, _) = upload(
        &a,
        &p,
        "one.txt",
        vec![b'a'; 2 * 1024 * 1024],
        "text/plain",
        None,
    )
    .await;
    assert_eq!(code, 200);
    let (code, r) = upload(
        &a,
        &p,
        "two.txt",
        vec![b'b'; 1024 * 1024],
        "text/plain",
        None,
    )
    .await;
    assert_eq!(code, 413, "{r}");
    assert!(
        r["error"].as_str().unwrap().contains("Speicherplatz"),
        "{r}"
    );
    let (_, q) = a.get("/api/attachments/quota").await;
    assert_eq!(q["limit"].as_i64(), Some(3 * 1024 * 1024));
    assert!(q["used"].as_i64().unwrap() > 2 * 1024 * 1024);

    // Count: at most 10 drafts/attachments per task (b has its own quota).
    let mut drafts = vec![];
    for i in 0..10 {
        let (code, r) = upload(
            &b,
            &p,
            &format!("n{i}.md"),
            format!("# note {i}\n").into_bytes(),
            "",
            None,
        )
        .await;
        assert_eq!(code, 200, "{r}");
        drafts.push(r["id"].as_str().unwrap().to_string());
    }
    let (code, _) = upload(&b, &p, "n10.md", b"# 11\n".to_vec(), "", None).await;
    assert_eq!(code, 400);
    // The composer finds them again (only the uploader's own drafts).
    let (_, d) = b
        .get(&format!("/api/projects/{p}/attachments/drafts"))
        .await;
    assert_eq!(d.as_array().unwrap().len(), 10);
    let (_, d) = a
        .get(&format!("/api/projects/{p}/attachments/drafts"))
        .await;
    assert!(d
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["id"] != json!(drafts[0])));
    assert_eq!(
        v.get(&format!("/api/projects/{p}/attachments/drafts"))
            .await
            .0,
        403
    );

    // Rights: viewers do not upload; drafts are private; strangers see nothing.
    let (code, _) = upload(&v, &p, "x.txt", b"hello".to_vec(), "", None).await;
    assert_eq!(code, 403);
    assert_eq!(download(&b, &id).await.0, 404, "a's draft is private");
    assert_eq!(download(&x, &id).await.0, 404);
    assert_eq!(b.delete(&format!("/api/attachments/{id}")).await.0, 404);
    // b's drafts become b's task; a draft of a cannot be attached by b.
    let (code, r) = b
        .post(
            "/api/tasks",
            json!({"project_id": p, "prompt": "with notes", "attachment_ids": [drafts[0], id]}),
        )
        .await;
    assert_eq!(code, 400, "{r}");
    let (code, t) = b
        .post("/api/tasks", json!({"project_id": p, "prompt": "MOCK_SLOW with notes", "attachment_ids": [drafts[0], drafts[1]]}))
        .await;
    assert_eq!(code, 200, "{t}");
    let tid = t["id"].as_str().unwrap().to_string();
    // Attached: every member reads, the viewer too; a stranger does not.
    assert_eq!(download(&v, &drafts[0]).await.0, 200);
    assert_eq!(download(&x, &drafts[0]).await.0, 404);
    let (_, d) = v.get(&format!("/api/tasks/{tid}")).await;
    let list = d["attachments"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(
        list.iter().all(|x| x["can_delete"] == false),
        "viewer cannot delete"
    );
    // Running task: no changes to its attachments.
    b.wait_status(&tid, &["running"]).await;
    let (code, _) = upload(&b, &p, "late.txt", b"late".to_vec(), "", Some(&tid)).await;
    assert_eq!(code, 409);
    assert_eq!(
        b.delete(&format!("/api/attachments/{}", drafts[0])).await.0,
        409
    );
    b.post(&format!("/api/tasks/{tid}/cancel"), json!({})).await;
    b.wait_status(&tid, &["cancelled"]).await;
    // An editor who did not upload cannot delete; the owner can.
    let (code, _) = upload(&b, &p, "later.txt", b"later".to_vec(), "", Some(&tid)).await;
    assert_eq!(code, 409, "cancelled is final");
    let (code, _) = a.delete(&format!("/api/attachments/{}", drafts[1])).await;
    assert_eq!(code, 200, "owner deletes");
    // Deleting the task deletes its attachments; deleting the project the rest.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM attachments WHERE task_id=$1")
        .bind(Uuid::parse_str(&tid).unwrap())
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(n, 1);
    sqlx::query("DELETE FROM tasks WHERE id=$1")
        .bind(Uuid::parse_str(&tid).unwrap())
        .execute(&h.st.db)
        .await
        .unwrap();
    assert_eq!(download(&b, &drafts[0]).await.0, 404);
    assert_eq!(a.delete(&format!("/api/projects/{p}")).await.0, 200);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM attachments")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn attachments_reach_the_agent_and_never_a_commit(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a, "Commit").await;
    let (code, att) = upload(&a, &p, "Screenshot 1.png", png(20, 10), "", None).await;
    assert_eq!(code, 200, "{att}");
    let (code, txt) = upload(&a, &p, "log.txt", b"stack trace here\n".to_vec(), "", None).await;
    assert_eq!(code, 200);
    let t = a
        .task_with(
            &p,
            "Fix the bug in the screenshot MOCK_SEE_ATTACHMENTS MOCK_COMMIT_ATTACHMENT",
            json!({"attachment_ids": [att["id"], txt["id"]]}),
        )
        .await;
    let d = a.wait_status(&t, &["done"]).await;
    let ev = d["events"].as_array().unwrap();
    let has = |n: &str| {
        ev.iter()
            .any(|e| e["content"].as_str().unwrap_or("").contains(n))
    };
    assert!(
        has("2 Anhang/Anhänge in .queue/attachments/ bereitgestellt"),
        "{ev:?}"
    );
    assert!(
        has("zu einem Commit ohne diese Dateien zusammengefasst"),
        "{ev:?}"
    );
    let main = h.workspace(&p);
    // The agent saw exactly the stored bytes under the Queue-made names.
    let seen = std::fs::read_to_string(main.join("SEEN-attachments.txt")).unwrap();
    let stored: Vec<(String, String)> =
        sqlx::query_as("SELECT name, sha256 FROM attachments ORDER BY created_at")
            .fetch_all(&h.st.db)
            .await
            .unwrap();
    for (n, sha) in &stored {
        assert!(seen.contains(&format!("{sha}  {n}")), "{seen}");
    }
    // No commit on the target branch ever contained `.queue/`.
    let files = git(&main, &["log", "--all", "--name-only", "--format="]);
    assert!(!files.contains(".queue"), "{files}");
    assert!(files.contains("SEEN-attachments.txt"));
    let tree = git(&main, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(!tree.contains(".queue") && tree.contains(".gitignore"));
    // The prompt named the attachments (and not the user's file names).
    let (_, pr) = a.get(&format!("/api/tasks/{t}")).await;
    assert_eq!(pr["attachments"].as_array().unwrap().len(), 2);
}

async fn rules(a: &Client, p: &str) -> Value {
    let (code, v) = a.get(&format!("/api/projects/{p}/knowledge")).await;
    assert_eq!(code, 200, "{v}");
    v
}

fn seen(h: &Harness, p: &str) -> String {
    std::fs::read_to_string(h.workspace(p).join("SEEN.md")).unwrap_or_default()
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn agent_rule_proposal_needs_owner_approval(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let b = h.user("b@x.ch").await;
    let p = project(&a, "Regeln").await;
    a.post(
        &format!("/api/projects/{p}/members"),
        json!({"email": "b@x.ch", "role": "editor"}),
    )
    .await;

    // Owner: approved version 1 at once; the agent gets it.
    let (code, v) = a
        .put(
            &format!("/api/projects/{p}/knowledge"),
            json!({"content": "- MOCKRULE-ONE immer Tests laufen lassen", "base_version": null}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["current"]["version"], 1);
    let t1 = a.task(&p, "first").await;
    a.wait_status(&t1, &["done"]).await;
    assert!(seen(&h, &p).contains("MOCKRULE-ONE"), "{}", seen(&h, &p));

    // An agent proposes new rules (with a secret in it): stored as a
    // proposal, redacted, not in the next prompt.
    let token = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
    let proposal = format!("- MOCKRULE-TWO kleine Commits\n- token {token}\n");
    let t2 = a
        .task_with(
            &p,
            &format!("learn MOCK_KNOWLEDGE_B64:{}", b64(&proposal)),
            json!({"title": "Lernen"}),
        )
        .await;
    a.wait_status(&t2, &["done"]).await;
    let v = rules(&a, &p).await;
    let props = v["proposals"].as_array().unwrap();
    assert_eq!(props.len(), 1, "{v}");
    assert_eq!(props[0]["source"], "agent");
    assert_eq!(props[0]["task_id"], t2.as_str());
    assert_eq!(props[0]["base_version"], 1);
    let content = props[0]["content"].as_str().unwrap();
    assert!(
        content.contains("MOCKRULE-TWO") && !content.contains(&token),
        "{content}"
    );
    let t3 = a.task(&p, "third").await;
    a.wait_status(&t3, &["done"]).await;
    assert!(!seen(&h, &p).contains("MOCKRULE-TWO"), "not approved yet");

    // Editors propose but never approve; the owner decides.
    let kid = props[0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        b.post(
            &format!("/api/projects/{p}/knowledge/{kid}/approve"),
            json!({})
        )
        .await
        .0,
        403
    );
    let (code, v) = b
        .put(
            &format!("/api/projects/{p}/knowledge"),
            json!({"content": "- MOCKRULE-EDITOR", "base_version": 1, "note": "Vorschlag"}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["current"]["version"], 1, "editor save is only a proposal");
    assert_eq!(v["proposals"].as_array().unwrap().len(), 2);
    assert_eq!(
        (v["can_approve"].as_bool(), v["can_propose"].as_bool()),
        (Some(false), Some(true))
    );
    let editor_kid = v["proposals"][1]["id"].as_str().unwrap().to_string();
    let (code, v) = a
        .post(
            &format!("/api/projects/{p}/knowledge/{kid}/approve"),
            json!({"note": "passt"}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["current"]["version"], 2);
    assert_eq!(v["current"]["source"], "agent");
    let (code, _) = a
        .post(
            &format!("/api/projects/{p}/knowledge/{kid}/approve"),
            json!({}),
        )
        .await;
    assert_eq!(code, 409, "decided once");
    let (code, v) = a
        .post(
            &format!("/api/projects/{p}/knowledge/{editor_kid}/reject"),
            json!({"note": "nein"}),
        )
        .await;
    assert_eq!(code, 200);
    assert!(v["proposals"].as_array().unwrap().is_empty());
    assert_eq!(v["rejected"][0]["note"], "Vorschlag\nnein");
    let t4 = a.task(&p, "fourth").await;
    a.wait_status(&t4, &["done"]).await;
    let s = seen(&h, &p);
    assert!(s.lines().last().unwrap().contains("MOCKRULE-TWO"), "{s}");
    assert!(!s.contains("MOCKRULE-EDITOR"));
    // The same text the owner rejected is not proposed again.
    let t5 = a
        .task_with(
            &p,
            &format!("again MOCK_KNOWLEDGE_B64:{}", b64("- MOCKRULE-EDITOR\n")),
            json!({"title": "Nochmal"}),
        )
        .await;
    a.wait_status(&t5, &["done"]).await;
    assert!(rules(&a, &p).await["proposals"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(a
        .events_of(&t5)
        .await
        .iter()
        .any(|(_, c)| c.contains("schon abgelehnt")));
    // Stale base: refused.
    let (code, _) = a
        .put(
            &format!("/api/projects/{p}/knowledge"),
            json!({"content": "x", "base_version": 1}),
        )
        .await;
    assert_eq!(code, 409);
    // Viewers/strangers: nothing.
    let c = h.user("c@x.ch").await;
    assert_eq!(c.get(&format!("/api/projects/{p}/knowledge")).await.0, 404);
    let (code, _) = a
        .put(
            &format!("/api/projects/{p}/knowledge"),
            json!({"content": "y".repeat(40_000), "base_version": 2}),
        )
        .await;
    assert_eq!(code, 400);
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn rules_export_import_and_trusted_repo_config(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a, "Export").await;
    assert_eq!(
        a.post(&format!("/api/projects/{p}/knowledge/export"), json!({}))
            .await
            .0,
        400
    );
    a.put(
        &format!("/api/projects/{p}/knowledge"),
        json!({"content": "- MOCKRULE-EXPORT klein bleiben", "base_version": null}),
    )
    .await;
    // Trusted repository configuration: the CLIs read the instruction files,
    // so the export moves the agent-config baseline (owner approved them).
    let (code, v) = a
        .put(
            &format!("/api/projects/{p}/protection"),
            json!({"trust_repo_config": true}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    let t = a.task(&p, "init").await;
    a.wait_status(&t, &["done"]).await;
    let (code, x) = a
        .post(&format!("/api/projects/{p}/knowledge/export"), json!({}))
        .await;
    assert_eq!(code, 200, "{x}");
    assert_eq!(x["written"], json!(["AGENTS.md", "CLAUDE.md", "GEMINI.md"]));
    assert_eq!(x["baseline_updated"], true);
    let main = h.workspace(&p);
    let agents = std::fs::read_to_string(main.join("AGENTS.md")).unwrap();
    assert!(agents.starts_with("<!-- Verwaltet von Queue") && agents.contains("MOCKRULE-EXPORT"));
    // The next run is not held for the agent configuration Queue wrote itself.
    let t2 = a.task(&p, "after export").await;
    let d = a.wait_status(&t2, &["done", "held"]).await;
    assert_eq!(d["task"]["status"], "done", "{}", d["task"]);
    // An agent changing AGENTS.md under trusted config needs an approval.
    let t3 = a.task(&p, "MOCK_FILE:AGENTS.md change the rules").await;
    let d = a.wait_status(&t3, &["held"]).await;
    let f = d["task"]["gate"]["findings"].as_array().unwrap();
    assert!(
        f.iter()
            .any(|x| x["kind"] == "agent_config" && x["path"] == "AGENTS.md"),
        "{f:?}"
    );
    a.post(&format!("/api/tasks/{t3}/gate/reject"), json!({}))
        .await;

    // Import: the managed marker is stripped, a bridge has nothing to import.
    let (code, v) = a
        .post(
            &format!("/api/projects/{p}/knowledge/import"),
            json!({"file": "AGENTS.md"}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    let prop = &v["proposals"][0];
    assert_eq!(prop["source"], "repo");
    assert_eq!(prop["content"], "- MOCKRULE-EXPORT klein bleiben");
    let (code, v) = a
        .post(
            &format!("/api/projects/{p}/knowledge/import"),
            json!({"file": "CLAUDE.md"}),
        )
        .await;
    assert_eq!(code, 400, "{v}");
    assert_eq!(
        a.post(
            &format!("/api/projects/{p}/knowledge/import"),
            json!({"file": "../etc/passwd"})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        a.post(
            &format!("/api/projects/{p}/knowledge/import"),
            json!({"file": "GEMINI.md"})
        )
        .await
        .0,
        400
    );
}

fn state_body(features: Value, progress: Value) -> Value {
    json!({"version": 1, "features": features, "progress": progress})
}

async fn state(a: &Client, p: &str) -> Value {
    let (code, v) = a.get(&format!("/api/projects/{p}/state")).await;
    assert_eq!(code, 200, "{v}");
    v
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn invalid_agent_state_is_rejected_or_repaired(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a, "Zustand").await;
    let s0 = state(&a, &p).await;
    assert_eq!(s0["rev"], 0);
    let base = state_body(
        json!([
            {"id": "login", "title": "Login", "status": "in_progress"},
            {"id": "export", "title": "Export", "status": "todo"}
        ]),
        json!([]),
    );
    let (code, v) = a
        .put(
            &format!("/api/projects/{p}/state"),
            json!({"state": base, "base_rev": 0, "note": "Start"}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["rev"], 1);
    // Humans: schema enforced, stale base refused.
    let bad = state_body(
        json!([{"id": "Bad ID", "title": "x", "status": "todo"}]),
        json!([]),
    );
    assert_eq!(
        a.put(
            &format!("/api/projects/{p}/state"),
            json!({"state": bad, "base_rev": 1})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        a.put(
            &format!("/api/projects/{p}/state"),
            json!({"state": base, "base_rev": 0})
        )
        .await
        .0,
        409
    );

    // Junk from the agent: rejected, the state stays.
    let t = a
        .task(&p, &format!("junk MOCK_STATE_B64:{}", b64("{ not json")))
        .await;
    a.wait_status(&t, &["done"]).await;
    assert!(a
        .events_of(&t)
        .await
        .iter()
        .any(|(_, c)| c.contains("Projektzustand des Agenten abgelehnt")));
    assert_eq!(state(&a, &p).await["rev"], 1);

    // Deletes "export", renames "login", references a missing file and a
    // traversal, marks login done: repaired, applied when done.
    let agent = state_body(
        json!([
            {"id": "login", "title": "Anmeldung", "status": "done", "passes": true,
             "files": ["MOCK_LOG.md", "src/missing.rs", "../../etc/passwd"]},
            {"id": "search", "title": "Suche", "status": "todo"}
        ]),
        json!([{"text": "Login fertig"}]),
    );
    // Explicit titles: a title derived from the prompt would cut the
    // base64 keyword (the mock reads the first line that has it).
    let t = a
        .task_with(
            &p,
            &format!("state MOCK_STATE_B64:{}", b64(&agent.to_string())),
            json!({"title": "Zustand"}),
        )
        .await;
    a.wait_status(&t, &["done"]).await;
    let ev = a.events_of(&t).await;
    assert!(
        ev.iter()
            .any(|(_, c)| c.contains("Projektzustand repariert") && c.contains("src/missing.rs")),
        "{ev:?}"
    );
    assert!(
        ev.iter()
            .any(|(_, c)| c.contains("Projektzustand übernommen (Stand 2)")),
        "{ev:?}"
    );
    let s = state(&a, &p).await;
    assert_eq!(s["rev"], 2);
    let f = s["state"]["features"].as_array().unwrap();
    let login = f.iter().find(|x| x["id"] == "login").unwrap();
    assert_eq!(
        (
            login["title"].as_str(),
            login["status"].as_str(),
            login["passes"].as_bool()
        ),
        (Some("Login"), Some("done"), Some(true))
    );
    assert_eq!(login["files"], json!(["MOCK_LOG.md"]));
    assert!(f.iter().any(|x| x["id"] == "export"), "restored");
    assert!(f.iter().any(|x| x["id"] == "search"));
    let prog = &s["state"]["progress"][0];
    assert_eq!(prog["text"], "Login fertig");
    assert_eq!(prog["task"], t.replace('-', "")[..8].to_string());
    assert_eq!(s["history"][0]["source"], "agent");
    assert_eq!(s["history"][0]["task_id"], t.as_str());

    // A failed task never changes the state.
    let fail = state_body(
        json!([
            {"id": "login", "title": "Login", "status": "done", "passes": true, "files": ["MOCK_LOG.md"]},
            {"id": "export", "title": "Export", "status": "done", "passes": true},
            {"id": "search", "title": "Suche", "status": "todo"}
        ]),
        json!([{"text": "Login fertig"}, {"text": "Export fertig"}]),
    );
    let t = a
        .task_with(
            &p,
            &format!("MOCK_FAIL MOCK_STATE_B64:{}", b64(&fail.to_string())),
            json!({"title": "Scheitert"}),
        )
        .await;
    a.wait_status(&t, &["failed", "blocked"]).await;
    let s = state(&a, &p).await;
    assert_eq!(s["rev"], 2);
    assert_eq!(s["state"]["features"][1]["status"], "todo");
}

fn onempty_goal(proposals: Value) -> String {
    let r = json!({"status": "done", "summary": "Vorschläge", "handoff": "", "questions": [], "follow_up_tasks": proposals});
    format!("Ship it MOCK_ONEMPTY_B64:{}", b64(&r.to_string()))
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn onempty_dedupe_drops_duplicates_and_explains(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a, "Leerlauf").await;
    let done = a
        .task_with(
            &p,
            "Write unit tests covering the CSV parser edge cases (quotes, empty lines).",
            json!({"title": "Add unit tests for the CSV parser"}),
        )
        .await;
    a.wait_status(&done, &["done"]).await;
    let rev = a
        .task_with(
            &p,
            "Replace the logger with an async one to speed up requests.",
            json!({"title": "Switch to async logging"}),
        )
        .await;
    a.wait_status(&rev, &["done"]).await;
    let (code, v) = a.post(&format!("/api/tasks/{rev}/revert"), json!({})).await;
    assert_eq!(code, 200, "{v}");
    let proposals = json!([
        {"title": "Add unit tests for CSV parser", "prompt": "Write unit tests covering the CSV parser edge cases like quotes and empty lines."},
        {"title": "Use async logging", "prompt": "Replace the logger with an asynchronous logger for faster requests."},
        {"title": "Document the HTTP API", "prompt": "Write a short reference of all HTTP endpoints with request and response examples."},
        {"title": "Speed up CI", "prompt": "Skip tests that are slow in .github/workflows/ci.yml to make CI faster for everyone."}
    ]);
    let (code, v) = a
        .patch(
            &format!("/api/projects/{p}"),
            json!({"goal": onempty_goal(proposals)}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    // Bad thresholds are refused.
    let (code, _) = a
        .put("/api/settings", {
            let mut s = a.get("/api/settings").await.1;
            s["dedupe_threshold"] = json!(0.3);
            s["dedupe_review_threshold"] = json!(0.5);
            s
        })
        .await;
    assert_eq!(code, 400);
    a.settings(json!({"onempty_enabled": true, "onempty_actions": ["improve"], "onempty_cooldown_minutes": 300, "max_auto_tasks": 5}))
        .await;
    let ts = a
        .wait_until("onempty done", |ts| {
            ts.iter().any(|t| {
                t["kind"] == "onempty" && t["status"] == "done" && t["proposals"].is_array()
            })
        })
        .await;
    let oe = ts.iter().find(|t| t["kind"] == "onempty").unwrap();
    let report = oe["proposals"].as_array().expect("report");
    let verdicts: Vec<&str> = report
        .iter()
        .map(|r| r["verdict"].as_str().unwrap())
        .collect();
    assert_eq!(
        verdicts,
        vec!["dropped", "dropped", "accepted", "review"],
        "{report:#?}"
    );
    assert!(report[0]["reasons"][0]
        .as_str()
        .unwrap()
        .starts_with("Duplikat:"));
    assert_eq!(report[0]["similar"]["task_id"], done.as_str());
    assert!(report[1]["reasons"].to_string().contains("rückgängig"));
    assert!(report[3]["reasons"]
        .to_string()
        .contains("CI-Konfiguration"));
    // Only the two kept proposals became tasks, with their reasons.
    // By id from the report: the task list leaves out tasks in the merge
    // queue ("merging"), and the accepted one may be there right now.
    let mut created: Vec<Value> = vec![];
    for r in report {
        if let Some(id) = r["task_id"].as_str() {
            created.push(a.task_json(id).await["task"].clone());
        }
    }
    assert_eq!(created.len(), 2, "{created:#?}");
    assert!(created.iter().all(|t| t["parent_id"] == oe["id"]));
    let docs = created
        .iter()
        .find(|t| t["title"] == "Document the HTTP API")
        .unwrap();
    assert_eq!(docs["dedupe"]["verdict"], "accepted");
    let ci = created
        .iter()
        .find(|t| t["title"] == "Speed up CI")
        .unwrap();
    assert_eq!(ci["status"], "pending_approval");
    assert_eq!(ci["dedupe"]["verdict"], "review");
    assert!(
        !ts.iter()
            .any(|t| t["title"] == "Add unit tests for CSV parser"),
        "duplicate not created"
    );
    let ev = a.events_of(oe["id"].as_str().unwrap()).await;
    assert!(
        ev.iter()
            .any(|(_, c)| c.contains("1 eingeplant, 1 zur Freigabe, 2 verworfen")),
        "{ev:?}"
    );
}

/// The optional model critic (direct access of the analysis step) can only
/// make verdicts stricter; its cost is recorded as step `critic`.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn onempty_model_critic_is_stricter_and_costs_are_recorded(pool: PgPool) {
    let h = start(pool).await;
    let fake = start_fake().await;
    let a = h.user("a@x.ch").await;
    let (code, c) = a
        .post("/api/connectors", json!({"kind": "mock", "label": "Fake", "config": {"base_url": fake.url, "api": "openai", "location": "eu"}}))
        .await;
    assert_eq!(code, 200, "{c}");
    let p = project(&a, "Critic").await;
    let r = json!({
        "execute": {"target": {"provider": "mock", "model": "claude-sonnet-5-5"}},
        "review": {"mode": "harness"},
        "analyze": {"mode": "sync", "target": {"connector_id": c["id"], "model": "gpt-6.1-sol"}},
    });
    let (code, v) = a
        .put(
            &format!("/api/projects/{p}/routing"),
            json!({"routing": r, "data_residency": "any"}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    let t = a.task(&p, "first").await;
    a.wait_status(&t, &["done"]).await;
    a.settings(json!({"onempty_enabled": true, "onempty_cooldown_minutes": 300, "max_auto_tasks": 5, "critic_llm": true}))
        .await;
    let ts = a
        .wait_until("onempty done", |ts| {
            ts.iter().any(|t| {
                t["kind"] == "onempty" && t["status"] == "done" && t["proposals"].is_array()
            })
        })
        .await;
    let oe = ts.iter().find(|t| t["kind"] == "onempty").unwrap();
    let report = oe["proposals"].as_array().expect("report");
    // The fake analysis proposes "Fake A"/"Fake B"; the rules accept both,
    // the fake critic drops the first and keeps the second.
    assert_eq!(report.len(), 2, "{report:#?}");
    assert_eq!(report[0]["verdict"], "dropped", "{report:#?}");
    assert!(
        report[0]["reasons"].to_string().contains("Fake critic"),
        "{report:#?}"
    );
    assert_eq!(report[1]["verdict"], "accepted");
    assert_eq!(report[1]["critic"], "rules+model");
    let b = a.task_json(report[1]["task_id"].as_str().unwrap()).await;
    assert_eq!(b["task"]["title"], "Fake B");
    assert!(report[0]["task_id"].is_null(), "dropped: no task");
    assert!(!ts.iter().any(|t| t["title"] == "Fake A"));
    let critic_calls = fake
        .requests()
        .into_iter()
        .filter(|r| {
            r.body
                .contains("strict critic of an autonomous coding queue")
        })
        .count();
    assert_eq!(critic_calls, 1);
    let steps: Vec<(String, f64)> =
        sqlx::query_as("SELECT step, cost_amount FROM usage_log WHERE task_id=$1 ORDER BY id")
            .bind(Uuid::parse_str(oe["id"].as_str().unwrap()).unwrap())
            .fetch_all(&h.st.db)
            .await
            .unwrap();
    assert!(
        steps.iter().any(|(s, c)| s == "critic" && *c > 0.0),
        "{steps:?}"
    );
    let cost = &oe["cost"];
    assert!(cost.to_string().contains("\"critic\""), "{cost}");
}
