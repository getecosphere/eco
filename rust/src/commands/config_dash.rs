//! `eco config` — local estate configuration dashboard (dev).
//!
//! Serves a web UI on http://127.0.0.1:8765 that lets a developer configure an
//! estate without hand-editing YAML. It reads the composed LXS schemas
//! (contract v2 `fields`) + the estate's current `services.<name>.config` and
//! renders each service's fields as a form:
//!
//!   - `managed` fields are greyed out ("managed by eco — <generator>")
//!   - non-secret values are written back to `ecompose.yml` (config block)
//!   - secret values are written to the service's local `.env` (never the
//!     manifest, per the secrets guardrail contract)
//!
//! The UI is read-only against the running estate; changes take effect on the
//! next `eco up`. See eco-server/docs/lxs-config-schema-v2.md.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tiny_http::{Header, Response, Server};

use crate::{ecompose, util};
use crate::commands::lxs::{self, LxsField, LxsManifest};

const HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>eco config · {{ESTATE}}</title>
<style>
  * { box-sizing:border-box; }
  body { margin:0; min-height:100vh; font:400 15px/1.55 "Manrope", ui-sans-serif, system-ui, sans-serif;
    background:#f7f6f2; color:#17141d; -webkit-font-smoothing:antialiased; }
  header { padding:1.6rem 2rem 1rem; border-bottom:1px solid #ded9e1; background:#fff; }
  header h1 { margin:0; font-size:1.15rem; letter-spacing:-.02em; }
  header p { margin:.3rem 0 0; color:#665f6e; font-size:.85rem; }
  main { max-width:860px; margin:0 auto; padding:1.5rem 1rem 4rem; }
  .service { background:#fff; border:1px solid #ded9e1; border-radius:12px; margin:1rem 0; overflow:hidden; }
  .service > h2 { margin:0; padding:.9rem 1.1rem; font-size:.95rem; letter-spacing:-.01em;
    background:#f1eef6; border-bottom:1px solid #ded9e1; }
  .service > h2 code { font-weight:600; color:#5b3fd6; }
  .group { padding:.6rem 1.1rem 0; }
  .group > h3 { margin:.8rem 0 .2rem; font-size:.72rem; letter-spacing:.08em; text-transform:uppercase; color:#8d8493; }
  .field { display:grid; grid-template-columns:minmax(180px,1fr) minmax(220px,1.4fr); gap:.6rem 1rem;
    align-items:start; padding:.65rem 0; border-top:1px solid #f0edf2; }
  .field .meta label { font-weight:700; font-size:.85rem; display:block; }
  .field .meta small { color:#8d8493; font-size:.78rem; display:block; margin-top:.15rem; }
  .field .meta .req { color:#c0392b; }
  .field .meta .mgr { color:#b07f2a; }
  .field input[type=text], .field input[type=password], .field input[type=number], .field select, .field textarea {
    width:100%; padding:.45rem .6rem; border:1px solid #cfc9d5; border-radius:8px; font:inherit; background:#fff; }
  .field input:disabled { background:#f4f2f5; color:#8d8493; }
  .field .bool { display:flex; gap:.5rem; align-items:center; padding-top:.25rem; }
  .field .chips { display:flex; flex-wrap:wrap; gap:.35rem; }
  .field .chip { background:#e9e4f2; color:#5b3fd6; border-radius:999px; padding:.15rem .6rem; font-size:.78rem; }
  .actions { padding:1rem 1.1rem 1.2rem; border-top:1px solid #f0edf2; display:flex; gap:.7rem; align-items:center; }
  button { padding:.55rem 1.3rem; border:0; border-radius:9px; font-weight:700; cursor:pointer; font-size:.85rem; }
  button.save { background:#5b3fd6; color:#fff; }
  button.save:hover { background:#482bbd; }
  button.save:disabled { background:#b9b0c8; cursor:default; }
  .status { font-size:.82rem; color:#665f6e; }
  .status.ok { color:#1e7f4f; }
  .status.err { color:#c0392b; }
  .hint { padding:1rem 1.1rem; color:#8d8493; font-size:.8rem; }
  .hint code { background:#f1eef6; padding:.05rem .35rem; border-radius:4px; }
</style>
</head>
<body>
<header>
  <h1>eco config · <span id="estate">…</span></h1>
  <p>Konfigurasi LXS estate (dev). Managed fields diisi eco; nilai non-secret tersimpan di <code>ecompose.yml</code>, secret di <code>.env</code>. Berlaku setelah <code>eco up</code>.</p>
</header>
<main id="app"><p>Memuat…</p></main>
<script>
const $ = s => document.querySelector(s);
async function load() {
  const r = await fetch('/api/estate');
  if (!r.ok) return renderError(await r.text());
  const data = await r.json();
  $('#estate').textContent = data.project;
  render(data);
}
function renderError(msg) {
  $('#app').innerHTML = `<p style="color:#c0392b">${escapeHtml(msg)}</p>`;
}
function escapeHtml(s) {
  return s.replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
}
function inputFor(f, val, dirty) {
  if (f.managed) return `<input type="text" value="" disabled placeholder="managed by eco — ${escapeHtml(f.managed)}">`;
  if (f.type === 'bool') {
    const on = val === 'true' || val === '1';
    return `<div class="bool"><label><input type="radio" name="${f.key}" value="true" ${on?'checked':''}> Ya</label>
      <label><input type="radio" name="${f.key}" value="false" ${!on?'checked':''}> Tidak</label></div>`;
  }
  if (f.type === 'enum') {
    const opts = f.choices.map(c => `<option value="${escapeHtml(c)}" ${c===val?'selected':''}>${escapeHtml(c)}</option>`).join('');
    return `<select name="${f.key}">${opts}</select>`;
  }
  if (f.type === 'int' || f.type === 'float') {
    return `<input type="number" step="${f.type==='float'?'any':'1'}" name="${f.key}" value="${escapeHtml(val)}">`;
  }
  if (f.type === 'csv' || f.type === 'csv-url') {
    return `<input type="text" name="${f.key}" value="${escapeHtml(val)}" placeholder="koma-pisah: a,b,c">`;
  }
  if (f.type === 'json') {
    return `<textarea name="${f.key}" rows="3">${escapeHtml(val)}</textarea>`;
  }
  return `<input type="${f.secret?'password':'text'}" name="${f.key}" value="${f.secret?'':escapeHtml(val)}" placeholder="${f.secret?'tidak ditampilkan — kosongkan biarkan':''}">`;
}
function render(data) {
  const app = $('#app'); app.innerHTML = '';
  const newValues = {};
  for (const svc of data.services) {
    const el = document.createElement('div'); el.className = 'service';
    const h = document.createElement('h2');
    h.innerHTML = `<code>${escapeHtml(svc.name)}</code> <span style="font-weight:400;color:#8d8493;font-size:.8rem">${escapeHtml(svc.lxs)}</span>`;
    el.appendChild(h);
    if (!svc.fields.length) {
      const hint = document.createElement('div'); hint.className = 'hint';
      hint.innerHTML = 'LXS ini belum menyatakan schema konfigurasi (contract v2 <code>fields</code>).';
      el.appendChild(hint); app.appendChild(el); continue;
    }
    const groups = {};
    for (const f of svc.fields) (groups[f.group || 'umum'] = groups[f.group || 'umum'] || []).push(f);
    for (const [gname, fields] of Object.entries(groups)) {
      const g = document.createElement('div'); g.className = 'group';
      const gh = document.createElement('h3'); gh.textContent = gname; g.appendChild(gh);
      for (const f of fields) {
        const val = svc.config[f.key] != null ? svc.config[f.key] : (f.default || '');
        const row = document.createElement('div'); row.className = 'field';
        const meta = document.createElement('div'); meta.className = 'meta';
        let label = `<label>${escapeHtml(f.key)}${f.required?' <span class="req">*</span>':''}${f.managed?' <span class="mgr">(eco)</span>':''}${f.secret?' <span class="mgr">(secret)</span>':''}</label>`;
        meta.innerHTML = label + (f.description ? `<small>${escapeHtml(f.description)}</small>` : '');
        row.appendChild(meta);
        const ctl = document.createElement('div');
        ctl.innerHTML = inputFor(f, val, false);
        row.appendChild(ctl);
        g.appendChild(row);
      }
      el.appendChild(g);
    }
    const actions = document.createElement('div'); actions.className = 'actions';
    const btn = document.createElement('button'); btn.className = 'save'; btn.type = 'button'; btn.textContent = 'Simpan';
    btn.addEventListener('click', async () => {
      btn.disabled = true; const st = statusEl(); st.className = 'status'; st.textContent = 'Menyimpan…';
      const config = {}; const secrets = {};
      for (const f of svc.fields) {
        if (f.managed) continue;
        const input = el.querySelector(`[name="${CSS.escape(f.key)}"]`);
        if (!input) continue;
        const v = input.value;
        if (f.secret) { if (v) secrets[f.key] = v; }
        else { if (v) config[f.key] = v; }
      }
      const r = await fetch('/api/apply', { method:'POST', headers:{'Content-Type':'application/json'},
        body: JSON.stringify({ service: svc.name, config, secrets }) });
      const body = await r.text();
      if (r.ok) { st.className = 'status ok'; st.textContent = 'Tersimpan. Jalankan eco up untuk menerapkan.'; }
      else { st.className = 'status err'; st.textContent = body; }
      btn.disabled = false;
    });
    const statusEl = () => { const s = document.createElement('span'); actions.appendChild(s); return s; };
    actions.appendChild(btn);
    actions.appendChild(statusEl());
    el.appendChild(actions);
    app.appendChild(el);
  }
}
load();
</script>
</body>
</html>"#;

fn find_ecompose_file(start_dir: &Path) -> Result<PathBuf, String> {
    let mut dir = std::fs::canonicalize(start_dir).unwrap_or_else(|_| start_dir.to_path_buf());
    loop {
        let direct = dir.join("ecompose.yml");
        if direct.is_file() {
            return Ok(direct);
        }
        let parent = match dir.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
        if parent == dir {
            break;
        }
        dir = parent;
    }
    Err("No ecompose.yml found. Run `eco config` from inside an estate directory.".to_string())
}

fn registry_address(estate_root: &Path) -> Option<String> {
    if let Some(reg) = lxs::read_estate_state(estate_root).map(|s| s.registry).filter(|r| !r.is_empty()) {
        return Some(reg);
    }
    if let Ok(reg) = std::env::var("ECO_LXS_REGISTRY") {
        if !reg.is_empty() {
            return Some(reg);
        }
    }
    None
}

/// Load the composed LXS manifest for a service from the estate's registry.
fn load_lxs_manifest(service: &ecompose::Service, address: Option<&str>) -> Result<Option<LxsManifest>, String> {
    let (manifest, _version) = lxs::fetch_lxs_manifest(&service.lxs, address)?;
    Ok(Some(manifest))
}

/// Synthesize a v1 (no-fields) contract into schema fields so the UI still has
/// something to render.
fn fields_for(manifest: &LxsManifest) -> Vec<(String, LxsField)> {
    if !manifest.contract.env.fields.is_empty() {
        let mut keys: Vec<&String> = manifest.contract.env.fields.keys().collect();
        keys.sort();
        return keys
            .into_iter()
            .map(|k| (k.clone(), manifest.contract.env.fields[k].clone()))
            .collect();
    }
    let mut out = Vec::new();
    for key in manifest.contract.env.required.iter() {
        out.push((
            key.clone(),
            LxsField {
                required: true,
                ..Default::default()
            },
        ));
    }
    for key in manifest.contract.env.optional.iter() {
        out.push((key.clone(), LxsField::default()));
    }
    out
}

fn build_estate_json(estate_root: &Path, content: &str) -> Result<serde_json::Value, String> {
    let services = ecompose::parse_services(content);
    let project = ecompose::parse_project_name(content);
    let registry = registry_address(estate_root);

    let mut svc_values = Vec::new();
    for svc in services {
        let mut entry = serde_json::Map::new();
        entry.insert("name".into(), serde_json::Value::String(svc.name.clone()));
        entry.insert("lxs".into(), serde_json::Value::String(svc.lxs.clone()));
        let mut config_map = serde_json::Map::new();
        for (k, v) in &svc.config {
            config_map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        entry.insert("config".into(), serde_json::Value::Object(config_map));
        if svc.lxs.is_empty() {
            entry.insert("fields".into(), serde_json::Value::Array(vec![]));
            svc_values.push(serde_json::Value::Object(entry));
            continue;
        }
        let manifest = match load_lxs_manifest(&svc, registry.as_deref()) {
            Ok(Some(m)) => m,
            Ok(None) => {
                entry.insert("fields".into(), serde_json::Value::Array(vec![]));
                svc_values.push(serde_json::Value::Object(entry));
                continue;
            }
            Err(e) => {
                eprintln!("[eco config] cannot load schema for {}: {e}", svc.name);
                entry.insert("fields".into(), serde_json::Value::Array(vec![]));
                svc_values.push(serde_json::Value::Object(entry));
                continue;
            }
        };
        let fields = fields_for(&manifest);
        let mut arr = Vec::new();
        for (key, f) in fields {
            let mut fm = serde_json::Map::new();
            fm.insert("key".into(), serde_json::Value::String(key));
            fm.insert("type".into(), serde_json::Value::String(if f.r#type.is_empty() { "string".to_string() } else { f.r#type.clone() }));
            fm.insert("default".into(), serde_json::Value::String(f.default.clone()));
            fm.insert("description".into(), serde_json::Value::String(f.description.clone()));
            fm.insert("group".into(), serde_json::Value::String(f.group.clone()));
            fm.insert("secret".into(), serde_json::Value::Bool(f.secret || f.r#type == "secret"));
            fm.insert("managed".into(), serde_json::Value::String(f.managed.clone()));
            fm.insert("required".into(), serde_json::Value::Bool(f.required));
            fm.insert("choices".into(), serde_json::Value::Array(f.choices.iter().cloned().map(serde_json::Value::String).collect()));
            arr.push(serde_json::Value::Object(fm));
        }
        entry.insert("fields".into(), serde_json::Value::Array(arr));
        svc_values.push(serde_json::Value::Object(entry));
    }
    Ok(serde_json::json!({
        "project": project,
        "services": svc_values,
    }))
}

/// Remove any existing `config:` block under the service and insert a fresh one.
fn apply_config_to_manifest(content: &str, service: &str, config: &HashMap<String, String>) -> Result<String, String> {
    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut in_services = false;
    let mut svc_start: Option<usize> = None;
    let mut svc_end = lines.len();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_end_matches('\r');
        if trimmed == "services:" {
            in_services = true;
            continue;
        }
        if !in_services {
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(' ') {
            break;
        }
        if line.starts_with("  ") && !line.starts_with("    ") && trimmed.ends_with(':') {
            let name = trimmed.trim().trim_end_matches(':').trim();
            if svc_start.is_some() {
                svc_end = idx;
                break;
            }
            if name == service {
                svc_start = Some(idx);
            }
        }
    }
    let Some(start) = svc_start else {
        return Err(format!("service '{service}' not found in ecompose.yml"));
    };

    // Build the new block lines.
    let mut block: Vec<String> = Vec::new();
    if !config.is_empty() {
        block.push("    config:".to_string());
        let mut keys: Vec<&String> = config.keys().collect();
        keys.sort();
        for k in keys {
            let v = config[k].trim();
            let rendered = if v.contains(':') || v.contains('#') || v.chars().any(|c| !c.is_ascii_graphic() || c == '"' || c == '\'') {
                format!("      {k}: {:?}", v)
            } else {
                format!("      {k}: \"{v}\"")
            };
            block.push(rendered);
        }
    }

    // Determine the span to drop (existing config block, if any).
    let mut drop_start: Option<usize> = None;
    let mut drop_end = start + 1;
    for idx in start..svc_end {
        let line = &lines[idx];
        if line.trim_end() == "    config:" {
            drop_start = Some(idx);
            drop_end = idx;
            continue;
        }
        if drop_start.is_some() {
            if line.starts_with("      ") {
                drop_end = idx + 1;
                continue;
            }
            break;
        }
    }

    // Insertion point: before the first indent-4 block after the header (so
    // scalar props like lxs:/port: stay at the top of the service block).
    let mut insert_at = start + 1;
    for idx in (start + 1)..svc_end {
        if drop_start.is_some() && idx >= drop_start.unwrap() && idx < drop_end {
            continue;
        }
        if line_starts_indent(&lines[idx], 4) {
            insert_at = idx;
            break;
        }
        insert_at = idx + 1;
    }

    let mut out: Vec<String> = Vec::with_capacity(lines.len() + block.len());
    for (idx, line) in lines.iter().enumerate() {
        if let Some(ds) = drop_start {
            if idx == ds {
                out.extend(block.iter().cloned());
                continue;
            }
            if idx >= ds && idx < drop_end {
                continue;
            }
        }
        if !block.is_empty() && !drop_start.is_some() && idx == insert_at {
            out.extend(block.iter().cloned());
        }
        out.push(line.clone());
    }
    if !block.is_empty() && !drop_start.is_some() && insert_at >= lines.len() {
        out.extend(block.iter().cloned());
    }
    if block.is_empty() && drop_start.is_some() {
        // config block removed entirely; nothing else to do
    }
    Ok(out.join("\n") + "\n")
}

fn line_starts_indent(line: &str, indent: usize) -> bool {
    line.starts_with(&" ".repeat(indent)) && !line.starts_with(&" ".repeat(indent + 1))
}

fn apply_secret(estate_root: &Path, service: &str, key: &str, value: &str) -> Result<(), String> {
    let dir = estate_root.join(service);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {service}: {e}"))?;
    let env_path = dir.join(".env");
    let mut lines: Vec<String> = if env_path.exists() {
        std::fs::read_to_string(&env_path)
            .map(|c| c.lines().map(|l| l.to_string()).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    lines.retain(|l| !l.starts_with(&format!("{key}=")));
    lines.push(format!("{key}={value}"));
    std::fs::write(&env_path, lines.join("\n") + "\n").map_err(|e| format!("write {}: {e}", env_path.display()))
}

pub fn run_config(args: &[String]) -> Result<(), String> {
    let mut port: u16 = 8765;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                port = args.get(i + 1).and_then(|p| p.parse().ok()).unwrap_or(8765);
                i += 2;
            }
            other => return Err(format!("Unknown eco config option: {other} (usage: eco config [--port <n>])")),
        }
    }

    let cwd = util::current_dir();
    let file_path = find_ecompose_file(&cwd)?;
    let estate_root = file_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let content = std::fs::read_to_string(&file_path).map_err(|e| format!("read {}: {e}", file_path.display()))?;

    let server = Server::http(format!("127.0.0.1:{port}")).map_err(|e| format!("cannot bind 127.0.0.1:{port}: {e}"))?;
    println!();
    println!("  eco config — estate: {}", util::bold(&ecompose::parse_project_name(&content)));
    println!("  {}", util::cyan(&format!("  http://127.0.0.1:{port}")));
    println!("  {}", util::dim("Ctrl-C untuk berhenti. Perubahan berlaku setelah `eco up`."));
    println!();

    for request in server.incoming_requests() {
        let mut request = request;
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or(&url).to_string();
        let method = request.method().to_string();

        let (status, body, ctype) = match (method.as_str(), path.as_str()) {
            ("GET", "/") => (
                200,
                HTML.replace("{{ESTATE}}", &ecompose::parse_project_name(&content)),
                "text/html; charset=utf-8",
            ),
            ("GET", "/api/estate") => {
                let content = std::fs::read_to_string(&file_path).map_err(|e| format!("read {}: {e}", file_path.display()));
                match content.and_then(|c| build_estate_json(&estate_root, &c)) {
                    Ok(json) => (200, json.to_string(), "application/json"),
                    Err(e) => (500, format!("{{\"error\":\"{}\"}}", e.replace('"', "\\\"")), "application/json"),
                }
            }
            ("POST", "/api/apply") => {
                let mut buf = Vec::new();
                if let Err(e) = request.as_reader().read_to_end(&mut buf) {
                    (500, format!("{{\"error\":\"{e}\"}}"), "application/json")
                } else {
                    match apply_request(&estate_root, &file_path, &String::from_utf8_lossy(&buf)) {
                        Ok(_) => (200, "{\"ok\":true}".to_string(), "application/json"),
                        Err(e) => (500, format!("{{\"error\":\"{}\"}}", e.replace('"', "\\\"")), "application/json"),
                    }
                }
            }
            _ => (404, "not found".to_string(), "text/plain"),
        };

        let _ = request.respond(
            Response::from_string(body)
                .with_status_code(status)
                .with_header(Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes()).unwrap_or_else(|_| Header::from_bytes(&b"Content-Type"[..], &b"text/plain"[..]).unwrap())),
        );
    }
    Ok(())
}

fn apply_request(estate_root: &Path, file_path: &Path, body: &str) -> Result<(), String> {
    use std::io::Read as _;
    let req: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("bad json: {e}"))?;
    let service = req["service"].as_str().ok_or("missing service")?;
    let config: HashMap<String, String> = req["config"]
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect())
        .unwrap_or_default();
    let secrets: HashMap<String, String> = req["secrets"]
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect())
        .unwrap_or_default();

    let content = std::fs::read_to_string(file_path).map_err(|e| format!("read {}: {e}", file_path.display()))?;
    let next = apply_config_to_manifest(&content, service, &config)?;
    std::fs::write(file_path, next).map_err(|e| format!("write {}: {e}", file_path.display()))?;

    for (k, v) in &secrets {
        apply_secret(estate_root, service, k, v)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_config_writes_and_replaces_the_service_config_block() {
        let content = "project: x\nservices:\n  a:\n    lxs: auth@1.0.0\n    port: 4200\n    access:\n      routes:\n        - path: /\n          level: public\n  b:\n    lxs: other@1.0.0\n";
        let mut cfg = HashMap::new();
        cfg.insert("EMAIL_VERIFICATION_REQUIRED".to_string(), "false".to_string());
        cfg.insert("RATE_LIMIT_AUTH_BURST".to_string(), "7".to_string());
        let out = apply_config_to_manifest(content, "a", &cfg).unwrap();
        assert!(out.contains("    config:\n      EMAIL_VERIFICATION_REQUIRED: \"false\"\n      RATE_LIMIT_AUTH_BURST: \"7\"\n"));
        assert!(out.contains("    lxs: auth@1.0.0"));
        assert!(out.contains("    port: 4200"));
        assert!(out.contains("    access:"), "access block must survive: {out}");
        // Second apply replaces the block, not duplicates it.
        let mut cfg2 = HashMap::new();
        cfg2.insert("RATE_LIMIT_AUTH_BURST".to_string(), "10".to_string());
        let out2 = apply_config_to_manifest(&out, "a", &cfg2).unwrap();
        assert_eq!(out2.matches("config:").count(), 1, "must not duplicate config blocks: {out2}");
        assert!(out2.contains("RATE_LIMIT_AUTH_BURST: \"10\""));
        assert!(!out2.contains("EMAIL_VERIFICATION_REQUIRED"));
        assert!(out2.contains("    access:"));
    }

    #[test]
    fn apply_config_removes_block_when_config_empty() {
        let content = "project: x\nservices:\n  a:\n    lxs: auth@1.0.0\n    config:\n      X: \"1\"\n    port: 4200\n";
        let out = apply_config_to_manifest(content, "a", &HashMap::new()).unwrap();
        assert!(!out.contains("config:"), "empty config should drop the block: {out}");
        assert!(out.contains("    port: 4200"));
    }

    #[test]
    fn apply_config_skips_blank_lines_between_services() {
        let content = "project: x\nservices:\n  a:\n    lxs: auth@1.0.0\n\n  b:\n    lxs: other@1.0.0\n";
        let mut cfg = HashMap::new();
        cfg.insert("K".to_string(), "v".to_string());
        let out = apply_config_to_manifest(content, "b", &cfg).unwrap();
        assert!(out.contains("config:\n      K: \"v\"\n"));
    }
}
