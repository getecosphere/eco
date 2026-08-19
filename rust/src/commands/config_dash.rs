//! `eco config` — local estate configuration dashboard (dev).
//!
//! Serves a web UI on http://127.0.0.1:8765 that lets a developer configure
//! one or more estates without hand-editing YAML:
//!
//!   - left-side tree navigation: estates → services → groups → fields
//!   - LXS configuration per service, rendered from the composed LXS schema
//!     (contract v2 `fields`), sections collapsible + searchable
//!   - `managed` fields are greyed out ("managed by eco — <generator>")
//!   - non-secret values write back to `ecompose.yml` config blocks
//!   - secret values write to the service's local `.env` (never the manifest)
//!   - a raw `ecompose.yml` editor (validated) for the whole manifest, e.g.
//!     project name, hostname, access routes
//!   - read-only prod env per service, fetched from the host agent
//!     (`GET /v1/estates/<project>/services/<service>/env`). The agent only
//!     returns `PUBLIC_*/VITE_*/NEXT_PUBLIC_*` keys by design — prod secrets
//!     never leave the host (stricter than Heroku's masked-but-revealable
//!     config vars), so the dashboard cannot leak them even if it tried.
//!
//! The UI is read-only against the running estate; config changes take effect
//! on the next `eco up`. See eco-server/docs/lxs-config-schema-v2.md.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tiny_http::{Header, Response, Server};

use crate::{ecompose, util};
use crate::commands::lxs::{self, LxsField, LxsManifest};

const HTML: &str = r#"<!doctype html>
<html lang="id">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>eco config</title>
<style>
  * { box-sizing:border-box; }
  :root { --bg:#f7f6f2; --card:#fff; --line:#ded9e1; --line2:#f0edf2; --text:#17141d; --muted:#665f6e;
          --accent:#5b3fd6; --accent2:#482bbd; --chipbg:#e9e4f2; --err:#c0392b; --ok:#1e7f4f; }
  html,body { height:100%; }
  body { margin:0; font:400 14px/1.55 "Manrope", ui-sans-serif, system-ui, sans-serif;
    background:var(--bg); color:var(--text); -webkit-font-smoothing:antialiased; display:flex; }
  /* ---------- sidebar ---------- */
  aside { width:300px; min-width:300px; background:#fff; border-right:1px solid var(--line);
    display:flex; flex-direction:column; height:100vh; position:sticky; top:0; }
  .brand { padding:1rem 1rem .6rem; border-bottom:1px solid var(--line); }
  .brand h1 { margin:0; font-size:1.05rem; letter-spacing:-.02em; }
  .brand small { color:var(--muted); }
  #searchwrap { padding:.7rem 1rem; }
  #search { width:100%; padding:.5rem .7rem; border:1px solid var(--line); border-radius:9px; font:inherit; }
  #tree { flex:1; overflow:auto; padding:.4rem .6rem 1rem; }
  .estate { margin:.15rem 0; }
  .estate > .row { display:flex; align-items:center; gap:.5rem; padding:.42rem .55rem; border-radius:8px; cursor:pointer; }
  .estate > .row:hover { background:#f1eef6; }
  .estate.active > .row { background:var(--chipbg); color:var(--accent); font-weight:700; }
  .caret { width:14px; color:var(--muted); font-size:.7rem; display:inline-block; }
  .estate-children { margin-left:1.05rem; border-left:1px solid var(--line); padding-left:.35rem; }
  .svc { padding:.3rem .5rem; border-radius:7px; color:var(--muted); font-size:.86rem; cursor:pointer; }
  .svc:hover { background:#f4f2f5; color:var(--text); }
  .svc .b { font-family:"DM Mono",ui-monospace,monospace; color:var(--text); }
  .hint { padding:.6rem 1rem .8rem; color:var(--muted); font-size:.78rem; border-top:1px solid var(--line); }
  .hint code { background:var(--chipbg); padding:.02rem .3rem; border-radius:4px; }
  /* ---------- main ---------- */
  main { flex:1; overflow:auto; height:100vh; }
  header.main { padding:1.1rem 1.6rem .8rem; border-bottom:1px solid var(--line); background:#fff;
    position:sticky; top:0; z-index:5; }
  header.main h2 { margin:0; font-size:1.15rem; letter-spacing:-.02em; }
  header.main .path { color:var(--muted); font-size:.8rem; font-family:"DM Mono",ui-monospace,monospace; }
  .tabs { display:flex; gap:.25rem; margin-top:.6rem; }
  .tab { padding:.4rem .9rem; border-radius:8px 8px 0 0; cursor:pointer; color:var(--muted); font-weight:600; font-size:.85rem; border:1px solid transparent; border-bottom:0; }
  .tab.active { background:var(--card); color:var(--accent); border-color:var(--line); }
  .content { max-width:1000px; margin:0 auto; padding:1.2rem 1.6rem 5rem; }
  .panel { display:none; } .panel.active { display:block; }
  /* ---------- general ---------- */
  .general { background:var(--card); border:1px solid var(--line); border-radius:12px; padding:1rem 1.2rem; margin-bottom:1rem; }
  .general h3 { margin:0 0 .6rem; font-size:.72rem; letter-spacing:.08em; text-transform:uppercase; color:#8d8493; }
  .grow { display:grid; grid-template-columns:repeat(3,1fr); gap:.7rem; }
  .grow label { display:block; font-size:.78rem; color:var(--muted); margin-bottom:.2rem; }
  .grow input { width:100%; padding:.45rem .6rem; border:1px solid var(--line); border-radius:8px; font:inherit; }
  /* ---------- services ---------- */
  details.service { background:var(--card); border:1px solid var(--line); border-radius:12px; margin:1rem 0; }
  details.service > summary { cursor:pointer; padding:.85rem 1.1rem; font-weight:700; font-size:.93rem;
    list-style:none; display:flex; align-items:center; gap:.5rem; background:#f1eef6; border-radius:12px 12px 0 0; }
  details.service > summary::-webkit-details-marker { display:none; }
  details.service > summary .caret { transition:transform .12s; }
  details.service[open] > summary { border-bottom:1px solid var(--line); }
  details.service[open] > summary .caret { transform:rotate(90deg); }
  details.service > summary .lxs { font-weight:400; color:var(--muted); font-size:.8rem; font-family:"DM Mono",ui-monospace,monospace; }
  .group { padding:.2rem 1.1rem 0; }
  .group > h4 { margin:1rem 0 .2rem; font-size:.72rem; letter-spacing:.08em; text-transform:uppercase; color:#8d8493; }
  .field { display:grid; grid-template-columns:minmax(200px,1fr) minmax(240px,1.4fr); gap:.6rem 1rem;
    align-items:start; padding:.6rem 0; border-top:1px solid var(--line2); }
  .field.hl { background:#fff8e1; border-radius:8px; box-shadow:0 0 0 2px #f0d45a; }
  .field .meta label { font-weight:700; font-size:.85rem; display:block; word-break:break-all; }
  .field .meta small { color:var(--muted); font-size:.78rem; display:block; margin-top:.15rem; }
  .req { color:var(--err); } .mgr { color:#b07f2a; } .sec { color:var(--accent); }
  .field input[type=text], .field input[type=password], .field input[type=number], .field select, .field textarea {
    width:100%; padding:.45rem .6rem; border:1px solid #cfc9d5; border-radius:8px; font:inherit; background:#fff; }
  .field input:disabled { background:#f4f2f5; color:var(--muted); }
  .bool { display:flex; gap:.5rem; align-items:center; padding-top:.25rem; }
  .actions { padding:1rem 1.1rem 1.2rem; border-top:1px solid var(--line2); display:flex; gap:.7rem; align-items:center; }
  button { padding:.5rem 1.2rem; border:0; border-radius:9px; font-weight:700; cursor:pointer; font-size:.84rem; }
  button.save { background:var(--accent); color:#fff; } button.save:hover { background:var(--accent2); }
  button.save:disabled { background:#b9b0c8; cursor:default; }
  button.ghost { background:transparent; color:var(--accent); border:1px solid var(--accent); }
  .status { font-size:.82rem; color:var(--muted); } .status.ok { color:var(--ok); } .status.err { color:var(--err); }
  .empty { padding:1rem 1.1rem; color:var(--muted); font-size:.85rem; }
  /* ---------- raw editor ---------- */
  textarea#raw { width:100%; min-height:60vh; font:12.5px/1.5 "DM Mono",ui-monospace,monospace; padding:.9rem;
    border:1px solid var(--line); border-radius:10px; background:#fff; color:var(--text); white-space:pre; }
  /* ---------- prod env ---------- */
  .envbar { display:flex; gap:.7rem; align-items:center; margin-bottom:.8rem; }
  .envbar select { padding:.45rem .6rem; border:1px solid var(--line); border-radius:8px; font:inherit; }
  table.env { width:100%; border-collapse:collapse; background:#fff; border:1px solid var(--line); border-radius:10px; overflow:hidden; }
  table.env th, table.env td { text-align:left; padding:.5rem .8rem; border-bottom:1px solid var(--line2); font-size:.84rem; }
  table.env th { background:#f1eef6; font-size:.72rem; text-transform:uppercase; letter-spacing:.06em; color:var(--muted); }
  table.env td code { font-family:"DM Mono",ui-monospace,monospace; word-break:break-all; }
  .mask { font-family:"DM Mono",ui-monospace,monospace; color:var(--muted); }
  .reveal { font-size:.75rem; padding:.25rem .6rem; }
  .envinfo { color:var(--muted); font-size:.82rem; margin-bottom:.6rem; }
  .warn { background:#fff8e1; border:1px solid #f0d45a; color:#6b5800; border-radius:9px; padding:.7rem .9rem; font-size:.84rem; margin-bottom:.8rem; }
</style>
</head>
<body>
<aside>
  <div class="brand"><h1>eco config</h1><small>konfigurasi estate (dev)</small></div>
  <div id="searchwrap"><input id="search" type="search" placeholder="Cari key / service / estate…"></div>
  <div id="tree"><p class="hint">Memuat estate…</p></div>
  <div class="hint">Simpan → berlaku setelah <code>eco up</code>. Prod env read-only & tersembunyi default.</div>
</aside>
<main>
  <header class="main">
    <h2 id="estateTitle">Pilih estate di sidebar</h2>
    <div class="path" id="estatePath"></div>
    <nav class="tabs" id="tabs">
      <div class="tab active" data-tab="lxs">LXS Config</div>
      <div class="tab" data-tab="raw">ecompose.yml</div>
      <div class="tab" data-tab="env">Prod env</div>
    </nav>
  </header>
  <div class="content">
    <section class="panel active" id="p-lxs"><p class="empty">Pilih estate di sidebar untuk mulai.</p></section>
    <section class="panel" id="p-raw"><p class="empty">—</p></section>
    <section class="panel" id="p-env"><p class="empty">—</p></section>
  </div>
</main>
<script>
const $ = s => document.querySelector(s);
const esc = s => String(s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const q = params => '?' + new URLSearchParams(params).toString();
const qs = new URLSearchParams(location.search);
let ESTATES = [], CURRENT = null;

/* ---------------- tree ---------------- */
async function loadEstates() {
  const r = await fetch('/api/estates');
  ESTATES = await r.json();
  renderTree();
  const pref = qs.get('dir');
  if (pref) { const e = ESTATES.find(x => x.path === pref); if (e) selectEstate(e); }
  else if (ESTATES.length) selectEstate(ESTATES[0]);
}
function renderTree() {
  const tree = $('#tree'); tree.innerHTML = '';
  for (const e of ESTATES) {
    const wrap = document.createElement('div'); wrap.className = 'estate';
    const row = document.createElement('div'); row.className = 'row';
    const caret = document.createElement('span'); caret.className = 'caret'; caret.textContent = '▸';
    const name = document.createElement('span'); name.textContent = e.project;
    row.append(caret, name);
    // Click toggles expand/collapse; expanding also loads the estate.
    row.onclick = () => {
      const isOpen = children.style.display === 'block';
      if (isOpen) {
        children.style.display = 'none';
        caret.textContent = '▸';
      } else {
        selectEstate(e, wrap);
      }
    };
    const children = document.createElement('div'); children.className = 'estate-children'; children.style.display = 'none';
    for (const s of e.services) {
      const svc = document.createElement('div'); svc.className = 'svc';
      svc.innerHTML = `<span class="b">${esc(s)}</span>`;
      svc.onclick = (ev) => { ev.stopPropagation(); selectEstate(e, wrap).then(() => jumpToService(s)); };
      children.appendChild(svc);
    }
    wrap.append(row, children); tree.appendChild(wrap);
  }
}
async function selectEstate(e, wrapNode) {
  document.querySelectorAll('.estate').forEach(n => n.classList.remove('active'));
  document.querySelectorAll('.estate-children').forEach(n => n.style.display = 'none');
  if (wrapNode) { wrapNode.classList.add('active'); wrapNode.querySelector('.estate-children').style.display = 'block';
    wrapNode.querySelector('.caret').textContent = '▾'; }
  const r = await fetch('/api/estate' + q({ dir: e.path }));
  if (!r.ok) return setStatus(await r.text(), 'err');
  CURRENT = await r.json();
  CURRENT.path = e.path;
  $('#estateTitle').textContent = CURRENT.project;
  $('#estatePath').textContent = e.path;
  renderGeneral(); renderLXS();
  history.replaceState(null, '', '/?dir=' + encodeURIComponent(e.path));
}
/* ---------------- tabs ---------------- */
document.querySelectorAll('.tab').forEach(t => t.onclick = () => {
  document.querySelectorAll('.tab').forEach(x => x.classList.remove('active'));
  document.querySelectorAll('.panel').forEach(x => x.classList.remove('active'));
  t.classList.add('active');
  $('#p-' + t.dataset.tab).classList.add('active');
  if (t.dataset.tab === 'raw') renderRaw();
  if (t.dataset.tab === 'env') renderEnv();
});
/* ---------------- LXS config ---------------- */
function renderGeneral() {
  const sec = $('#p-lxs'); sec.innerHTML = '';
  const g = document.createElement('div'); g.className = 'general';
  const h = document.createElement('h3'); h.textContent = 'Umum'; g.appendChild(h);
  const grow = document.createElement('div'); grow.className = 'grow';
  const mk = (label, key, val) => {
    const d = document.createElement('div');
    d.innerHTML = `<label>${label}</label><input id="gen-${key}" value="${esc(val||'')}">`;
    grow.appendChild(d);
  };
  mk('Project name', 'project', CURRENT.project);
  mk('Main estate', 'main', CURRENT.main);
  mk('Hostname', 'hostname', CURRENT.hostname);
  g.appendChild(grow);
  const btn = document.createElement('button'); btn.className = 'save'; btn.textContent = 'Simpan Umum'; btn.type = 'button';
  const st = document.createElement('span'); st.className = 'status';
  btn.onclick = async () => {
    btn.disabled = true; st.className = 'status'; st.textContent = 'Menyimpan…';
    const r = await fetch('/api/general' + q({ dir: CURRENT.path }), { method:'POST', headers:{'Content-Type':'application/json'},
      body: JSON.stringify({ project: $('#gen-project').value.trim(), main: $('#gen-main').value.trim(), hostname: $('#gen-hostname').value.trim() }) });
    const t = await r.text();
    if (r.ok) { st.className = 'status ok'; st.textContent = 'Tersimpan.'; loadEstates(); }
    else { st.className = 'status err'; st.textContent = t; }
    btn.disabled = false;
  };
  g.append(btn, st); sec.appendChild(g);
}
function renderLXS() {
  const sec = $('#p-lxs');
  for (const svc of CURRENT.services) {
    const el = document.createElement('details'); el.className = 'service'; el.id = 'svc-' + cssId(svc.name);
    const sum = document.createElement('summary');
    sum.innerHTML = `<span class="caret">▸</span> <code>${esc(svc.name)}</code> <span class="lxs">${esc(svc.lxs)}</span>`;
    el.appendChild(sum);
    if (!svc.fields.length) {
      const hint = document.createElement('div'); hint.className = 'empty';
      hint.textContent = svc.lxs ? 'LXS ini belum menyatakan schema konfigurasi (contract v2 fields).' : 'Service sumber (path:) — konfigurasi lewat ecompose.yml.';
      el.appendChild(hint);
    } else {
      const groups = {};
      for (const f of svc.fields) (groups[f.group || 'umum'] = groups[f.group || 'umum'] || []).push(f);
      for (const [gname, fields] of Object.entries(groups)) {
        const g = document.createElement('div'); g.className = 'group';
        const gh = document.createElement('h4'); gh.textContent = gname; g.appendChild(gh);
        for (const f of fields) {
          const val = svc.config[f.key] != null ? svc.config[f.key] : (f.default || '');
          const row = document.createElement('div'); row.className = 'field'; row.dataset.key = f.key;
          row.dataset.service = svc.name; row.dataset.grp = gname;
          const meta = document.createElement('div'); meta.className = 'meta';
          let tags = f.required ? ' <span class="req">*</span>' : '';
          if (f.managed) tags += ' <span class="mgr">(eco)</span>';
          if (f.secret) tags += ' <span class="sec">(secret)</span>';
          meta.innerHTML = `<label>${esc(f.key)}${tags}</label>` + (f.description ? `<small>${esc(f.description)}</small>` : '');
          row.appendChild(meta);
          const ctl = document.createElement('div'); ctl.innerHTML = inputFor(f, val); row.appendChild(ctl);
          g.appendChild(row);
        }
        el.appendChild(g);
      }
      const act = document.createElement('div'); act.className = 'actions';
      const btn = document.createElement('button'); btn.className = 'save'; btn.textContent = 'Simpan ' + svc.name; btn.type = 'button';
      const st = document.createElement('span'); st.className = 'status';
      btn.onclick = async () => {
        btn.disabled = true; st.className = 'status'; st.textContent = 'Menyimpan…';
        const config = {}; const secrets = {};
        for (const f of svc.fields) {
          if (f.managed) continue;
          const input = el.querySelector(`[name="${cssId(f.key)}"]`);
          if (!input) continue;
          const v = input.value;
          if (f.secret) { if (v) secrets[f.key] = v; }
          else { config[f.key] = v; }
        }
        const r = await fetch('/api/apply' + q({ dir: CURRENT.path }), { method:'POST', headers:{'Content-Type':'application/json'},
          body: JSON.stringify({ service: svc.name, config, secrets }) });
        const t = await r.text();
        if (r.ok) { st.className = 'status ok'; st.textContent = 'Tersimpan. Jalankan eco up untuk menerapkan.'; loadEstates(); }
        else { st.className = 'status err'; st.textContent = t; }
        btn.disabled = false;
      };
      act.append(btn, st); el.appendChild(act);
    }
    sec.appendChild(el);
  }
}
function inputFor(f, val) {
  if (f.managed) return `<input type="text" value="" disabled placeholder="managed by eco — ${esc(f.managed)}">`;
  if (f.type === 'bool') {
    const on = val === 'true' || val === '1';
    return `<div class="bool"><label><input type="radio" name="${cssId(f.key)}" value="true" ${on?'checked':''}> Ya</label>
      <label><input type="radio" name="${cssId(f.key)}" value="false" ${!on?'checked':''}> Tidak</label></div>`;
  }
  if (f.type === 'enum') {
    const opts = f.choices.map(c => `<option value="${esc(c)}" ${c===val?'selected':''}>${esc(c)}</option>`).join('');
    return `<select name="${cssId(f.key)}">${opts}</select>`;
  }
  if (f.type === 'int' || f.type === 'float') return `<input type="number" step="${f.type==='float'?'any':'1'}" name="${cssId(f.key)}" value="${esc(val)}">`;
  if (f.type === 'csv' || f.type === 'csv-url') return `<input type="text" name="${cssId(f.key)}" value="${esc(val)}" placeholder="koma-pisah: a,b,c">`;
  if (f.type === 'json') return `<textarea name="${cssId(f.key)}" rows="3">${esc(val)}</textarea>`;
  return `<input type="${f.secret?'password':'text'}" name="${cssId(f.key)}" value="${f.secret?'':esc(val)}" placeholder="${f.secret?'tidak ditampilkan — kosongkan biarkan':''}">`;
}
function cssId(s) { return 'i_' + s.replace(/[^a-zA-Z0-9]/g, '_'); }
function jumpToService(name) { const el = document.getElementById('svc-' + cssId(name)); if (el) { el.open = true; el.scrollIntoView({behavior:'smooth', block:'start'}); } }
/* ---------------- raw ecompose ---------------- */
let RAW = '';
async function renderRaw() {
  const sec = $('#p-raw');
  const r = await fetch('/api/ecompose' + q({ dir: CURRENT.path }));
  RAW = await r.text();
  sec.innerHTML = '';
  const info = document.createElement('div'); info.className = 'envinfo';
  info.textContent = 'Edit seluruh ecompose.yml (nama project, hostname, access routes, dsb.). Validasi saat simpan.';
  sec.appendChild(info);
  const ta = document.createElement('textarea'); ta.id = 'raw'; ta.value = RAW; sec.appendChild(ta);
  const bar = document.createElement('div'); bar.style.cssText = 'display:flex;gap:.7rem;align-items:center;margin-top:.7rem;';
  const btn = document.createElement('button'); btn.className = 'save'; btn.textContent = 'Simpan ecompose.yml'; btn.type = 'button';
  const st = document.createElement('span'); st.className = 'status';
  btn.onclick = async () => {
    btn.disabled = true; st.className = 'status'; st.textContent = 'Menyimpan…';
    const r2 = await fetch('/api/ecompose-save' + q({ dir: CURRENT.path }), { method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({ content: ta.value }) });
    const t = await r2.text();
    if (r2.ok) { st.className = 'status ok'; st.textContent = 'Tersimpan.'; loadEstates(); }
    else { st.className = 'status err'; st.textContent = t; }
    btn.disabled = false;
  };
  bar.append(btn, st); sec.appendChild(bar);
}
/* ---------------- prod env ---------------- */
async function renderEnv() {
  const sec = $('#p-env'); sec.innerHTML = '';
  const warn = document.createElement('div'); warn.className = 'warn';
  warn.innerHTML = 'Agent host <b>tidak mengekspos secret</b> — endpoint prod-env hanya mengembalikan key <code>PUBLIC_*</code>/<code>VITE_*</code>/<code>NEXT_PUBLIC_*</code> (desain keamanan platform, lebih ketat daripada Heroku yang masih bisa reveal). Secret prod tidak pernah meninggalkan host.';
  sec.appendChild(warn);
  const bar = document.createElement('div'); bar.className = 'envbar';
  const sel = document.createElement('select');
  for (const s of CURRENT.services) { if (s.lxs) { const o = document.createElement('option'); o.value = s.name; o.textContent = s.name; sel.appendChild(o); } }
  bar.appendChild(sel);
  const info = document.createElement('span'); info.className = 'envinfo'; bar.appendChild(info);
  sec.appendChild(bar);
  const table = document.createElement('table'); table.className = 'env';
  table.innerHTML = '<thead><tr><th style="width:36%">Key</th><th>Nilai (prod, public-only)</th></tr></thead><tbody></tbody>';
  sec.appendChild(table);
  const tbody = table.querySelector('tbody');
  async function load() {
    tbody.innerHTML = '<tr><td colspan="2">Memuat…</td></tr>';
    info.textContent = '';
    const r = await fetch('/api/prod-env' + q({ dir: CURRENT.path, service: sel.value }));
    const d = await r.json();
    if (!d.available) { tbody.innerHTML = `<tr><td colspan="2">${esc(d.error || 'prod env tidak tersedia')}</td></tr>`; return; }
    info.textContent = d.source;
    tbody.innerHTML = '';
    for (const item of d.env) {
      const tr = document.createElement('tr');
      tr.innerHTML = `<td><code>${esc(item.key)}</code></td><td><code>${esc(item.value)}</code></td>`;
      tbody.appendChild(tr);
    }
    if (!d.env.length) tbody.innerHTML = '<tr><td colspan="2">Tidak ada key public (PUBLIC_*/VITE_*/NEXT_PUBLIC_*) di service ini.</td></tr>';
  }
  sel.onchange = load;
  await load();
}
/* ---------------- search ---------------- */
let lastQuery = '';
$('#search').addEventListener('input', (e) => {
  const q0 = e.target.value.trim().toLowerCase(); lastQuery = q0;
  if (!CURRENT) return;
  if (!q0) { document.querySelectorAll('#p-lxs .field').forEach(f => f.classList.remove('hl')); return; }
  document.querySelectorAll('#p-lxs .field').forEach(f => {
    const hit = (f.dataset.key + ' ' + f.dataset.grp + ' ' + f.dataset.service).toLowerCase().includes(q0);
    f.classList.toggle('hl', hit);
    if (hit) f.scrollIntoView({ behavior:'smooth', block:'center' });
  });
  const first = document.querySelector('#p-lxs .field.hl');
  if (first && !document.querySelector('#p-lxs .field.hl').offsetParent) first.scrollIntoView({ behavior:'smooth', block:'center' });
});
/* ---------------- boot ---------------- */
loadEstates();
</script>
</body>
</html>"#;

/* ================= Rust side ================= */

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

fn default_estates_root() -> PathBuf {
    if let Ok(p) = std::env::var("ECO_CONFIG_ESTATES") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    for candidate in [
        format!("{}/ar-rahman/estates", util::home_dir()),
        format!("{}/projects", util::home_dir()),
        format!("{}/superapp", util::home_dir()),
    ] {
        let p = PathBuf::from(&candidate);
        if p.is_dir() {
            return p;
        }
    }
    PathBuf::from(format!("{}/ar-rahman/estates", util::home_dir()))
}

/// Discover estates: subdirectories of the estates root that carry ecompose.yml.
fn discover_estates(root: &Path) -> Vec<(String, String, Vec<String>)> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let ecompose = dir.join("ecompose.yml");
            if ecompose.is_file() {
                if let Ok(content) = std::fs::read_to_string(&ecompose) {
                    let project = ecompose::parse_project_name(&content);
                    let services = ecompose::parse_services(&content)
                        .iter()
                        .map(|s| s.name.clone())
                        .collect();
                    out.push((project, dir.display().to_string(), services));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Validate that a requested estate dir is one of the discovered estates.
fn resolve_estate_dir(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let requested = PathBuf::from(raw);
    for (_name, path, _services) in discover_estates(root) {
        if PathBuf::from(&path) == requested {
            return Ok(requested);
        }
    }
    Err("unknown estate dir (not under the discovered estates root)".to_string())
}

/// Resolve a registry address for the dashboard: estate state wins, then the
/// local mirror (ECO_LXS_REGISTRY or the default path), then the GitHub default.
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

fn load_lxs_manifest(service: &ecompose::Service, address: Option<&str>) -> Result<Option<LxsManifest>, String> {
    let (manifest, _version) = lxs::fetch_lxs_manifest(&service.lxs, address)?;
    Ok(Some(manifest))
}

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
        "main": ecompose::parse_main(&content),
        "hostname": ecompose::parse_estates(&content).first().map(|e| e.hostname.clone()).unwrap_or_default(),
        "services": svc_values,
    }))
}

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
        if !block.is_empty() && drop_start.is_none() && idx == insert_at {
            out.extend(block.iter().cloned());
        }
        out.push(line.clone());
    }
    if !block.is_empty() && drop_start.is_none() && insert_at >= lines.len() {
        out.extend(block.iter().cloned());
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

/// Replace top-level `key: value` lines (project, main) and the first
/// `hostname:` under the estates block. Leaves everything else untouched.
fn apply_general(content: &str, project: &str, main: &str, hostname: &str) -> Result<String, String> {
    if project.trim().is_empty() {
        return Err("project name cannot be empty".to_string());
    }
    let mut out: Vec<String> = Vec::new();
    let mut in_estates = false;
    let mut hostname_done = false;
    let mut project_done = false;
    let mut main_done = false;
    for line in content.lines() {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.starts_with("project:") && !line.starts_with(' ') {
            out.push(format!("project: {}", project.trim()));
            project_done = true;
            continue;
        }
        if trimmed.starts_with("main:") && !line.starts_with(' ') {
            out.push(format!("main: {}", main.trim()));
            main_done = true;
            continue;
        }
        if trimmed == "estates:" && !line.starts_with(' ') {
            in_estates = true;
            out.push(line.to_string());
            continue;
        }
        if in_estates {
            if !line.starts_with(' ') {
                in_estates = false;
            } else if !hostname_done && line.trim_start().starts_with("hostname:") {
                out.push(format!("    hostname: {}", hostname.trim()));
                hostname_done = true;
                continue;
            }
        }
        out.push(line.to_string());
    }
    if !project_done {
        out.insert(0, format!("project: {}", project.trim()));
    }
    if !main_done && !main.trim().is_empty() {
        out.insert(1, format!("main: {}", main.trim()));
    }
    Ok(out.join("\n") + "\n")
}

/// Fetch a service's generated prod env from the host agent, masked.
fn fetch_prod_env(estate_root: &Path, content: &str, service: &str) -> Result<serde_json::Value, String> {
    let project = ecompose::parse_project_name(content);
    let api_url = std::env::var("ECO_API_URL").unwrap_or_default();
    let api_key = std::env::var("ECO_API_KEY").unwrap_or_default();
    if api_url.is_empty() || api_key.is_empty() {
        return Ok(serde_json::json!({ "available": false, "error": "ECO_API_URL / ECO_API_KEY belum di-set (sumber dari ~/.zshrc)" }));
    }
    let url = format!("{}/v1/estates/{}/services/{}/env", api_url.trim_end_matches('/'), project, service);
    let response = ureq::get(&url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .call()
        .map_err(|e| format!("agent request gagal: {e}"))?;
    let status = response.status();
    let text = response.into_string().map_err(|e| e.to_string())?;
    if !(200..300).contains(&status) {
        return Ok(serde_json::json!({ "available": false, "error": format!("agent {status}: {}", text.lines().next().unwrap_or("")) }));
    }
    let mut env = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            env.push(serde_json::json!({
                "key": k.trim(),
                "value": v.trim().trim_matches('"'),
            }));
        }
    }
    Ok(serde_json::json!({
        "available": true,
        "source": format!("host agent · /opt/eco/{project}/current/.env/{service}.env (hanya PUBLIC_*/VITE_*/NEXT_PUBLIC_* — secret tidak diekspos oleh agent)"),
        "env": env,
    }))
}

fn parse_query(url: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(q) = url.split('?').nth(1) {
        for pair in q.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                let decoded = urlencoding_decode(v);
                out.insert(k.to_string(), decoded);
            }
        }
    }
    out
}

fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn json_error(status: u16, msg: &str) -> (u16, String, &'static str) {
    (status, format!("{{\"error\":\"{}\"}}", msg.replace('"', "\\\"")), "application/json")
}

pub fn run_config(args: &[String]) -> Result<(), String> {
    let mut port: u16 = 8765;
    let mut estates_root: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                port = args.get(i + 1).and_then(|p| p.parse().ok()).unwrap_or(8765);
                i += 2;
            }
            "--estates" => {
                estates_root = Some(PathBuf::from(args.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
            other => return Err(format!("Unknown eco config option: {other} (usage: eco config [--port <n>] [--estates <dir>])")),
        }
    }

    let root = estates_root.unwrap_or_else(default_estates_root);
    let cwd = util::current_dir();
    let cwd_estate = find_ecompose_file(&cwd).ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));

    let server = Server::http(format!("127.0.0.1:{port}")).map_err(|e| format!("cannot bind 127.0.0.1:{port}: {e}"))?;
    println!();
    println!("  eco config");
    println!("  estates root: {}", util::bold(&root.display().to_string()));
    println!("  {}", util::cyan(&format!("  http://127.0.0.1:{port}")));
    println!("  {}", util::dim("Ctrl-C untuk berhenti. Perubahan berlaku setelah `eco up`."));
    println!();

    let estates = discover_estates(&root);
    if estates.is_empty() {
        return Err(format!("no estates (ecompose.yml) found under {}", root.display()));
    }

    for request in server.incoming_requests() {
        let mut request = request;
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or(&url).to_string();
        let query = parse_query(&url);
        let method = request.method().to_string();

        let (status, body, ctype) = match (method.as_str(), path.as_str()) {
            ("GET", "/") => (
                200,
                HTML.replace("{{ESTATE}}", ""),
                "text/html; charset=utf-8",
            ),
            ("GET", "/api/estates") => {
                let list: Vec<serde_json::Value> = estates
                    .iter()
                    .map(|(project, path, services)| {
                        serde_json::json!({ "project": project, "path": path, "services": services })
                    })
                    .collect();
                (200, serde_json::json!(list).to_string(), "application/json")
            }
            ("GET", "/api/estate") => {
                let dir = match query.get("dir") {
                    Some(d) => d.clone(),
                    None => cwd_estate
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned())
                        .ok_or_else(|| "no estate dir".to_string())?,
                };
                match resolve_estate_dir(&root, &dir).and_then(|d| {
                    std::fs::read_to_string(d.join("ecompose.yml"))
                        .map_err(|e| format!("read ecompose.yml: {e}"))
                        .and_then(|c| build_estate_json(&d, &c))
                }) {
                    Ok(json) => (200, json.to_string(), "application/json"),
                    Err(e) => json_error(400, &e),
                }
            }
            ("POST", "/api/apply") => {
                let mut buf = Vec::new();
                if let Err(e) = request.as_reader().read_to_end(&mut buf) {
                    json_error(500, &format!("read body: {e}"))
                } else {
                    let dir = query.get("dir").cloned().unwrap_or_default();
                    match resolve_estate_dir(&root, &dir).and_then(|d| {
                        apply_request(&d, &d.join("ecompose.yml"), &String::from_utf8_lossy(&buf))
                    }) {
                        Ok(_) => (200, "{\"ok\":true}".to_string(), "application/json"),
                        Err(e) => json_error(500, &e),
                    }
                }
            }
            ("POST", "/api/general") => {
                let mut buf = Vec::new();
                if let Err(e) = request.as_reader().read_to_end(&mut buf) {
                    json_error(500, &format!("read body: {e}"))
                } else {
                    let dir = query.get("dir").cloned().unwrap_or_default();
                    match resolve_estate_dir(&root, &dir).and_then(|d| {
                        let file = d.join("ecompose.yml");
                        let content = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
                        let req: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&buf)).map_err(|e| format!("bad json: {e}"))?;
                        let project = req["project"].as_str().unwrap_or("").to_string();
                        let main = req["main"].as_str().unwrap_or("").to_string();
                        let hostname = req["hostname"].as_str().unwrap_or("").to_string();
                        let next = apply_general(&content, &project, &main, &hostname)?;
                        std::fs::write(&file, next).map_err(|e| format!("write {}: {e}", file.display()))
                    }) {
                        Ok(_) => (200, "{\"ok\":true}".to_string(), "application/json"),
                        Err(e) => json_error(500, &e),
                    }
                }
            }
            ("GET", "/api/ecompose") => {
                let dir = query.get("dir").cloned().unwrap_or_default();
                match resolve_estate_dir(&root, &dir).and_then(|d| {
                    std::fs::read_to_string(d.join("ecompose.yml")).map_err(|e| format!("read ecompose.yml: {e}"))
                }) {
                    Ok(text) => (200, text, "text/plain"),
                    Err(e) => json_error(400, &e),
                }
            }
            ("POST", "/api/ecompose-save") => {
                let mut buf = Vec::new();
                if let Err(e) = request.as_reader().read_to_end(&mut buf) {
                    json_error(500, &format!("read body: {e}"))
                } else {
                    let dir = query.get("dir").cloned().unwrap_or_default();
                    match resolve_estate_dir(&root, &dir).and_then(|d| {
                        let file = d.join("ecompose.yml");
                        let req: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&buf)).map_err(|e| format!("bad json: {e}"))?;
                        let content = req["content"].as_str().ok_or("missing content")?;
                        if ecompose::parse_project_name(content).is_empty() {
                            return Err("validasi gagal: `project:` tidak ditemukan atau kosong".to_string());
                        }
                        if ecompose::parse_services(content).is_empty() {
                            return Err("validasi gagal: `services:` kosong — pastikan YAML tetap utuh".to_string());
                        }
                        std::fs::write(&file, content).map_err(|e| format!("write {}: {e}", file.display()))
                    }) {
                        Ok(_) => (200, "{\"ok\":true}".to_string(), "application/json"),
                        Err(e) => json_error(400, &e),
                    }
                }
            }
            ("GET", "/api/prod-env") => {
                let dir = query.get("dir").cloned().unwrap_or_default();
                let service = query.get("service").cloned().unwrap_or_default();
                match resolve_estate_dir(&root, &dir).and_then(|d| {
                    let content = std::fs::read_to_string(d.join("ecompose.yml")).map_err(|e| e.to_string())?;
                    fetch_prod_env(&d, &content, &service)
                }) {
                    Ok(json) => (200, json.to_string(), "application/json"),
                    Err(e) => json_error(500, &e),
                }
            }
            _ => (404, "not found".to_string(), "text/plain"),
        };

        let _ = request.respond(
            Response::from_string(body)
                .with_status_code(status)
                .with_header(
                    Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes())
                        .unwrap_or_else(|_| Header::from_bytes(&b"Content-Type"[..], &b"text/plain"[..]).unwrap()),
                ),
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

    #[test]
    fn apply_general_updates_project_main_hostname() {
        let content = "project: old\nmain: old\n\nestates:\n  old:\n    hostname: old.example.com\n    services: []\n\nservices:\n  a:\n    lxs: x@1.0.0\n";
        let out = apply_general(content, "new", "new", "new.example.com").unwrap();
        assert!(out.starts_with("project: new\nmain: new\n"));
        assert!(out.contains("    hostname: new.example.com"));
        assert!(out.contains("services:\n  a:\n    lxs: x@1.0.0"), "services must survive: {out}");
    }
}
