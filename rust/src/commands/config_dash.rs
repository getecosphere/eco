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
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use tiny_http::{Header, Response, Server};

use crate::{ecompose, util};
use crate::commands::lxs::{self, LxsField, LxsManifest};

/// Ecosphere favicons (same as the getecosphere.com estate frontend), embedded
/// so the dashboard is self-contained. PNG — the estate's favicon.svg is
/// broken (it references a missing ../master/ecosphere-original-1254.png), so
/// we use the actual rendered PNGs the estate serves.
const FAVICON_16: &[u8] = include_bytes!("../../../assets/favicon-16.png");
const FAVICON_32: &[u8] = include_bytes!("../../../assets/favicon-32.png");

/// The running `eco up dev` session (one at a time), so the UI can start a
/// dev environment and poll until a local URL appears.
struct DevSession {
    child: Child,
    dir: String,
    log_path: PathBuf,
}
static DEV_SESSION: Mutex<Option<DevSession>> = Mutex::new(None);

const HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Ecosphere Genie</title>
<link rel="icon" type="image/png" sizes="32x32" href="/favicon-32.png">
<link rel="icon" type="image/png" sizes="16x16" href="/favicon-16.png">
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
  .brand { padding:1rem 1rem .5rem; border-bottom:1px solid var(--line); display:flex; align-items:center; gap:.6rem; }
  .brand img { width:26px; height:26px; border-radius:6px; }
  .brand h1 { margin:0; font-size:1.05rem; letter-spacing:-.02em; }
  .brand small { color:var(--muted); display:block; }
  .langbtn { margin-left:auto; display:flex; gap:.15rem; }
  .langbtn button { padding:.15rem .4rem; font-size:.72rem; font-weight:700; border:1px solid var(--line);
    background:#fff; color:var(--muted); border-radius:6px; cursor:pointer; }
  .langbtn button.on { background:var(--chipbg); color:var(--accent); border-color:var(--accent); }
  #searchwrap { padding:.7rem 1rem; }
  #search { width:100%; padding:.5rem .7rem; border:1px solid var(--line); border-radius:9px; font:inherit; }
  #tree { flex:1; overflow:auto; padding:.4rem .6rem 1rem; }
  .estate { margin:.15rem 0; }
  .estate > .row { display:flex; align-items:center; gap:.5rem; padding:.42rem .55rem; border-radius:8px; cursor:pointer; }
  .estate > .row:hover { background:#f1eef6; }
  .estate.active > .row { background:var(--chipbg); color:var(--accent); font-weight:700; }
  .rowlinks { margin-left:auto; display:flex; gap:2px; opacity:0; transition:opacity .12s; }
  .row:hover .rowlinks { opacity:1; }
  .rowlink { width:22px; height:22px; padding:0; display:inline-flex; align-items:center; justify-content:center;
    border:0; background:transparent; border-radius:6px; cursor:pointer; color:var(--accent); }
  .rowlink:hover { background:var(--chipbg); }
  .rowlink svg { width:14px; height:14px; }
  .rowlink.off { color:#c3bcc9; cursor:default; }
  .rowlink.off:hover { background:transparent; }
  .caret { width:18px; flex:none; color:var(--muted); font-size:1rem; display:inline-block; text-align:center; transition:transform .12s; }
  .estate-children { margin-left:1.05rem; border-left:1px solid var(--line); padding-left:.35rem; }
  .svc { display:flex; align-items:center; gap:.4rem; padding:.32rem .5rem; border-radius:7px; color:var(--muted); font-size:.86rem; cursor:pointer; }
  .svc:hover { background:#f4f2f5; color:var(--text); }
  .svc .b { font-family:"DM Mono",ui-monospace,monospace; color:var(--text); }
  .svcimg { width:15px; height:15px; border-radius:3px; object-fit:contain; flex:none; }
  .svcdot { width:15px; flex:none; text-align:center; font-size:13px; color:#1a5fb4; }
  .favicon { width:16px; height:16px; border-radius:4px; object-fit:contain; flex:none; }
  .ctxmenu { position:fixed; z-index:100; background:#fff; border:1px solid var(--line); border-radius:9px;
    box-shadow:0 10px 28px rgba(0,0,0,.14); padding:.3rem; min-width:190px; }
  .ctxmenu button { display:block; width:100%; text-align:left; padding:.5rem .75rem; border:0; background:none;
    font:inherit; font-size:.84rem; border-radius:6px; cursor:pointer; color:var(--text); }
  .ctxmenu button:hover { background:#f1eef6; }
  .ctxmenu button.disabled { color:var(--muted); cursor:default; }
  .ctxmenu button.disabled:hover { background:none; }
  .ctxmenu .sep { height:1px; background:var(--line); margin:.3rem .4rem; }
  .ctxmenu .head { font-size:.68rem; letter-spacing:.07em; text-transform:uppercase; color:#8d8493; padding:.3rem .75rem .2rem; }
  .hint { padding:.6rem 1rem .8rem; color:var(--muted); font-size:.78rem; border-top:1px solid var(--line); }
  .hint code { background:var(--chipbg); padding:.02rem .3rem; border-radius:4px; }
  /* ---------- main ---------- */
  main { flex:1; overflow:auto; height:100vh; }
  header.main { padding:1.1rem 1.6rem .8rem; border-bottom:1px solid var(--line); background:#fff;
    position:sticky; top:0; z-index:5; }
  header.main h2 { margin:0; font-size:1.15rem; letter-spacing:-.02em; }
  header.main .path { color:var(--muted); font-size:.8rem; font-family:"DM Mono",ui-monospace,monospace; }
  header.main .desc { color:#8d8493; font-size:.85rem; margin-top:.15rem; }
  .openlinks { display:flex; gap:.5rem; margin-top:.55rem; }
  .openlink { display:inline-flex; align-items:center; gap:.3rem; padding:.38rem .8rem; border-radius:8px;
    background:#fff; border:1px solid var(--line); color:var(--accent); font-weight:700; font-size:.8rem; text-decoration:none; }
  .openlink:hover { background:var(--chipbg); }
  .openlink[hidden] { display:none; }
  .tabs { display:flex; gap:.25rem; margin-top:.6rem; }
  .tab { padding:.4rem .9rem; border-radius:8px 8px 0 0; cursor:pointer; color:var(--muted); font-weight:600; font-size:.85rem; border:1px solid transparent; border-bottom:0; }
  .tab.active { background:var(--card); color:var(--accent); border-color:var(--line); }
  .content { max-width:1000px; margin:0 auto; padding:1.2rem 1.6rem 5rem; }
  .panel { display:none; } .panel.active { display:block; }
  /* ---------- general ---------- */
  .general { background:var(--card); border:1px solid var(--line); border-radius:12px; padding:1rem 1.2rem; margin-bottom:1rem; }
  .general h3 { margin:0 0 .6rem; font-size:.72rem; letter-spacing:.08em; text-transform:uppercase; color:#8d8493; }
  .general .acts { display:flex; gap:.7rem; align-items:center; margin-top:1rem; }
  .general .acts .status { margin:0; }
  .grow { display:grid; grid-template-columns:repeat(3,1fr); gap:.7rem; }
  .grow label { display:block; font-size:.78rem; color:var(--muted); margin-bottom:.2rem; }
  .grow input { width:100%; padding:.45rem .6rem; border:1px solid var(--line); border-radius:8px; font:inherit; }
  /* ---------- sections & services ---------- */
  .sect { display:flex; align-items:center; gap:.5rem; margin:1.4rem 0 .3rem; font-size:.78rem; letter-spacing:.08em;
    text-transform:uppercase; color:#8d8493; }
  .sect::after { content:""; flex:1; height:1px; background:var(--line); }
  .badge { font-size:.58rem; padding:.13rem .45rem; border-radius:999px; font-weight:800; letter-spacing:.06em; white-space:nowrap; }
  .badge.core { background:#e5f0ff; color:#1a5fb4; }
  .badge.lxs { background:var(--chipbg); color:var(--accent); }
  details.service { background:var(--card); border:1px solid var(--line); border-radius:12px; margin:1rem 0; }
  details.service > summary { cursor:pointer; padding:.8rem 1.1rem; font-weight:700; font-size:.93rem;
    list-style:none; display:flex; align-items:center; gap:.55rem; background:#f1eef6; border-radius:12px 12px 0 0; }
  details.service > summary::-webkit-details-marker { display:none; }
  details.service[open] > summary { border-bottom:1px solid var(--line); }
  details.service > summary .lxs { font-weight:400; color:var(--muted); font-size:.8rem; font-family:"DM Mono",ui-monospace,monospace; }
  details.service > summary .pub { font-weight:500; color:#8d8493; font-size:.74rem; }
  .svcicon { width:20px; height:20px; border-radius:5px; object-fit:contain; flex:none; }
  .svcicon.core { display:inline-flex; align-items:center; justify-content:center; font-size:15px; color:#1a5fb4; background:#e5f0ff; }
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
  .envinfo { color:var(--muted); font-size:.82rem; margin-bottom:.6rem; }
  .warn { background:#fff8e1; border:1px solid #f0d45a; color:#6b5800; border-radius:9px; padding:.7rem .9rem; font-size:.84rem; margin-bottom:.8rem; }
</style>
</head>
<body>
<aside>
  <div class="brand">
    <img src="/favicon-32.png" alt="">
    <div><h1>Ecosphere Genie</h1><small data-i18n="subtitle">estate configuration (dev)</small></div>
    <div class="langbtn"><button id="langEn" class="on">EN</button><button id="langId">ID</button></div>
  </div>
  <div id="searchwrap"><input id="search" type="search" data-i18n-ph="search" placeholder="Search estates, services, keys…"></div>
  <div id="tree"><p class="hint" data-i18n="treeLoading">Loading estates…</p></div>
  <div class="hint"><span data-i18n="sidebarHint">Save → applies after</span> <code>eco up</code><span data-i18n="sidebarHint2">. Prod env read-only &amp; hidden by default.</span></div>
</aside>
<main>
  <header class="main">
    <h2 id="estateTitle" data-i18n="selectEstate">Select an estate on the left</h2>
    <div class="path" id="estatePath"></div>
    <div class="desc" id="estateDesc"></div>
    <div class="openlinks">
      <a id="openProd" class="openlink" target="_blank" rel="noopener" data-i18n="openProd">Open prod ↗</a>
      <a id="openLocal" class="openlink" target="_blank" rel="noopener" data-i18n="openLocal">Open local ↗</a>
    </div>
    <nav class="tabs" id="tabs">
      <div class="tab active" data-tab="lxs" data-i18n="tabLxs">LXS Config</div>
      <div class="tab" data-tab="raw" data-i18n="tabRaw">ecompose.yml</div>
      <div class="tab" data-tab="env" data-i18n="tabEnv">Prod env</div>
    </nav>
  </header>
  <div class="content">
    <section class="panel active" id="p-lxs"><p class="empty" data-i18n="selectEstate">Select an estate on the left to start.</p></section>
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

/* ---------------- i18n ---------------- */
const I18N = {
  en: {
    subtitle:'estate configuration (dev)', search:'Search estates, services, keys…',
    treeLoading:'Loading estates…', sidebarHint:'Save → applies after', sidebarHint2:'. Prod env read-only &amp; hidden by default.',
    selectEstate:'Select an estate on the left', openProd:'Open prod ↗', openLocal:'Open local ↗',
    tabLxs:'LXS Config', tabRaw:'ecompose.yml', tabEnv:'Prod env',
    general:'General', projectName:'Project name', mainEstate:'Main estate', hostname:'Hostname', description:'Description', saveGeneral:'Save General',
    saving:'Saving…', saved:'Saved.', runUp:'Saved. Run `eco up` to apply.',
    coreDomain:'Core domain (source)', reusableLxs:'Reusable LXS (registry)', badgeCore:'CORE', badgeLxs:'LXS',
    lxsNoSchema:'This LXS has no config schema yet (contract v2 fields).',
    sourceService:'Source (path:) service — configure via ecompose.yml.',
    saveService:'Save', rawInfo:'Edit the whole ecompose.yml (project name, hostname, access routes…). Validated on save.',
    saveEcompose:'Save ecompose.yml', envWarn:'The host agent never exposes secrets — prod-env returns only <code>PUBLIC_*</code>/<code>VITE_*</code>/<code>NEXT_PUBLIC_*</code> keys (platform security design, stricter than Heroku’s reveal).',
    envLoading:'Loading…', envEmpty:'No public keys (PUBLIC_*/VITE_*/NEXT_PUBLIC_*) in this service.', envNotAvail:'prod env not available',
    copyPath:'Copy path', copyEcompose:'Copy ecompose.yml path', reload:'Reload estate', copied:'Path copied',
    startDev:'Start dev (eco up dev)', devStarting:'Starting dev…', devRunning:'Dev running.', devDone:'Dev ready.',
    devFail:'Dev failed (see log).', devAlready:'eco up dev already running.', devLog:'log', stopHint:'or stop it in a terminal', localDev:'Local dev',
    pm2Pause:'Pause', pm2Stop:'Stop', pm2Delete:'Delete',
  },
  id: {
    subtitle:'konfigurasi estate (dev)', search:'Cari estate, service, key…',
    treeLoading:'Memuat estate…', sidebarHint:'Simpan → berlaku setelah', sidebarHint2:'. Prod env read-only &amp; tersembunyi default.',
    selectEstate:'Pilih estate di sidebar', openProd:'Buka di prod ↗', openLocal:'Buka lokal ↗',
    tabLxs:'LXS Config', tabRaw:'ecompose.yml', tabEnv:'Prod env',
    general:'Umum', projectName:'Nama project', mainEstate:'Estate utama', hostname:'Hostname', description:'Deskripsi', saveGeneral:'Simpan Umum',
    saving:'Menyimpan…', saved:'Tersimpan.', runUp:'Tersimpan. Jalankan `eco up` untuk menerapkan.',
    coreDomain:'Core domain (sumber)', reusableLxs:'Reusable LXS (registry)', badgeCore:'CORE', badgeLxs:'LXS',
    lxsNoSchema:'LXS ini belum menyatakan schema konfigurasi (contract v2 fields).',
    sourceService:'Service sumber (path:) — konfigurasi lewat ecompose.yml.',
    saveService:'Simpan', rawInfo:'Edit seluruh ecompose.yml (nama project, hostname, access routes…). Validasi saat simpan.',
    saveEcompose:'Simpan ecompose.yml', envWarn:'Agent host tidak mengekspos secret — prod-env hanya mengembalikan key <code>PUBLIC_*</code>/<code>VITE_*</code>/<code>NEXT_PUBLIC_*</code> (desain keamanan platform).',
    envLoading:'Memuat…', envEmpty:'Tidak ada key public (PUBLIC_*/VITE_*/NEXT_PUBLIC_*) di service ini.', envNotAvail:'prod env tidak tersedia',
    copyPath:'Salin path', copyEcompose:'Salin path ecompose.yml', reload:'Muat ulang estate', copied:'Path tersalin',
    startDev:'Jalankan dev (eco up dev)', devStarting:'Memulai dev…', devRunning:'Dev berjalan.', devDone:'Dev siap.',
    devFail:'Dev gagal (lihat log).', devAlready:'eco up dev sedang berjalan.', devLog:'log', stopHint:'atau hentikan di terminal', localDev:'Local dev',
    pm2Pause:'Jeda', pm2Stop:'Hentikan', pm2Delete:'Hapus',
  }
};
let LANG = localStorage.getItem('ecoGenieLang') || 'en';
const t = k => (I18N[LANG] && I18N[LANG][k]) || I18N.en[k] || k;
function setLang(l) {
  LANG = l; localStorage.setItem('ecoGenieLang', l);
  $('#langEn').classList.toggle('on', l==='en'); $('#langId').classList.toggle('on', l==='id');
  document.querySelectorAll('[data-i18n]').forEach(el => el.textContent = t(el.dataset.i18n));
  document.querySelectorAll('[data-i18n-ph]').forEach(el => el.placeholder = t(el.dataset.i18nPh));
  renderTree(); if (CURRENT) { renderGeneral(); renderLXS(); }
}
$('#langEn').onclick = () => setLang('en');
$('#langId').onclick = () => setLang('id');

/* ---------------- context menu ---------------- */
let ctx = null;
function closeCtx() { if (ctx) { ctx.remove(); ctx = null; } }
function showCtx(x, y, head, items) {
  closeCtx();
  ctx = document.createElement('div'); ctx.className = 'ctxmenu';
  if (head) { const h = document.createElement('div'); h.className = 'head'; h.textContent = head; ctx.appendChild(h); }
  for (const it of items) {
    if (it === '-') { const s = document.createElement('div'); s.className = 'sep'; ctx.appendChild(s); continue; }
    const b = document.createElement('button');
    b.textContent = it.label;
    if (it.disabled) b.classList.add('disabled');
    else b.onclick = () => { closeCtx(); it.action(); };
    ctx.appendChild(b);
  }
  document.body.appendChild(ctx);
  const w = ctx.offsetWidth, h = ctx.offsetHeight;
  ctx.style.left = Math.min(x, window.innerWidth - w - 10) + 'px';
  ctx.style.top = Math.min(y, window.innerHeight - h - 10) + 'px';
}
document.addEventListener('click', closeCtx);
document.addEventListener('scroll', closeCtx, true);
document.addEventListener('resize', closeCtx);
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') closeCtx(); });
function copyText(txt) {
  const done = () => {};
  (navigator.clipboard ? navigator.clipboard.writeText(txt) : Promise.reject())
    .then(() => { const s = $('#estatePath'); const old = s.textContent; s.textContent = t('copied'); setTimeout(() => s.textContent = old, 1200); })
    .catch(() => {});
}
function estateCtxItems(e) {
  return [
    { label: t('openProd'), disabled: !e.prodUrl, action: () => window.open(e.prodUrl, '_blank') },
    { label: t('openLocal'), disabled: !e.localUrl, action: () => window.open(e.localUrl, '_blank') },
    '-',
    { label: t('copyPath'), action: () => copyText(e.path) },
    { label: t('copyEcompose'), action: () => copyText(e.path + '/ecompose.yml') },
    { label: t('reload'), action: () => { const w = document.querySelector('.estate[data-path="' + cssId(e.path) + '"]'); selectEstate(e, w); } },
  ];
}
/* ---------------- tree ---------------- */
const ICONS = {
  globe: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/></svg>',
  monitor: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="3" width="20" height="14" rx="2"/><path d="M8 21h8M12 17v4"/></svg>',
};
function rowLink(icon, url, title) {
  const b = document.createElement('button');
  b.className = 'rowlink' + (url ? '' : ' off');
  b.title = url ? title : title + ' (—)';
  b.innerHTML = ICONS[icon];
  b.onclick = (ev) => { ev.stopPropagation(); if (url) window.open(url, '_blank'); };
  return b;
}
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
    const wrap = document.createElement('div'); wrap.className = 'estate'; wrap.dataset.path = cssId(e.path);
    const row = document.createElement('div'); row.className = 'row';
    const caret = document.createElement('span'); caret.className = 'caret'; caret.textContent = '▸';
    const img = document.createElement('img');
    img.className = 'favicon'; img.width = 16; img.height = 16; img.alt = '';
    img.src = '/api/estate-favicon' + q({ dir: e.path });
    img.onerror = () => img.remove();
    const name = document.createElement('span'); name.textContent = e.project;
    const links = document.createElement('span'); links.className = 'rowlinks';
    links.appendChild(rowLink('globe', e.prodUrl, t('openProd')));
    links.appendChild(rowLink('monitor', e.localUrl, t('openLocal')));
    row.append(caret, img, name, links);
    row.onclick = () => {
      const isOpen = children.style.display === 'block';
      if (isOpen) { children.style.display = 'none'; caret.textContent = '▸'; }
      else selectEstate(e, wrap);
    };
    row.addEventListener('contextmenu', (ev) => {
      ev.preventDefault();
      showCtx(ev.clientX, ev.clientY, e.project, estateCtxItems(e));
    });
    const children = document.createElement('div'); children.className = 'estate-children'; children.style.display = 'none';
    for (const s of e.services) {
      const svc = document.createElement('div'); svc.className = 'svc';
      svc.innerHTML = `<span class="svcimg"></span><span class="b">${esc(s)}</span>`;
      svc.onclick = (ev) => { ev.stopPropagation(); selectEstate(e, wrap).then(() => jumpToService(s)); };
      svc.addEventListener('contextmenu', (ev) => {
        ev.preventDefault(); ev.stopPropagation();
        showCtx(ev.clientX, ev.clientY, s, [
          { label: t('openProd'), disabled: !e.prodUrl, action: () => window.open(e.prodUrl, '_blank') },
          { label: t('openLocal'), disabled: !e.localUrl, action: () => window.open(e.localUrl, '_blank') },
          '-',
          { label: t('reload'), action: () => selectEstate(e, wrap) },
        ]);
      });
      children.appendChild(svc);
    }
    wrap.append(row, children); tree.appendChild(wrap);
  }
}
async function selectEstate(e, wrapNode) {
  document.querySelectorAll('.estate').forEach(n => n.classList.remove('active'));
  document.querySelectorAll('.estate-children').forEach(n => { n.style.display = 'none'; n.parentElement.querySelector('.caret').textContent = '▸'; });
  if (wrapNode) { wrapNode.classList.add('active'); wrapNode.querySelector('.estate-children').style.display = 'block';
    wrapNode.querySelector('.caret').textContent = '▾'; }
  const r = await fetch('/api/estate' + q({ dir: e.path }));
  if (!r.ok) return setStatus(await r.text(), 'err');
  CURRENT = await r.json();
  CURRENT.path = e.path;
  $('#estateTitle').textContent = CURRENT.project;
  $('#estatePath').textContent = e.path;
  $('#estateDesc').textContent = CURRENT.description || '';
  $('#estateDesc').style.display = CURRENT.description ? '' : 'none';
  const prod = $('#openProd'), loc = $('#openLocal');
  if (CURRENT.prodUrl) { prod.href = CURRENT.prodUrl; prod.hidden = false; } else prod.hidden = true;
  if (CURRENT.localUrl) { loc.href = CURRENT.localUrl; loc.hidden = false; } else loc.hidden = true;
  renderGeneral(); renderLXS();
  history.replaceState(null, '', '/?dir=' + encodeURIComponent(e.path));
}
/* ---------------- tabs ---------------- */
document.querySelectorAll('.tab').forEach(tb => tb.onclick = () => {
  document.querySelectorAll('.tab').forEach(x => x.classList.remove('active'));
  document.querySelectorAll('.panel').forEach(x => x.classList.remove('active'));
  tb.classList.add('active');
  $('#p-' + tb.dataset.tab).classList.add('active');
  if (tb.dataset.tab === 'raw') renderRaw();
  if (tb.dataset.tab === 'env') renderEnv();
});
/* ---------------- LXS config ---------------- */
function renderGeneral() {
  const sec = $('#p-lxs'); sec.innerHTML = '';
  const g = document.createElement('div'); g.className = 'general';
  const h = document.createElement('h3'); h.textContent = t('general'); g.appendChild(h);
  const grow = document.createElement('div'); grow.className = 'grow';
  const mk = (label, key, val) => {
    const d = document.createElement('div');
    d.innerHTML = `<label>${label}</label><input id="gen-${key}" value="${esc(val||'')}">`;
    grow.appendChild(d);
  };
  mk(t('projectName'), 'project', CURRENT.project);
  mk(t('mainEstate'), 'main', CURRENT.main);
  mk(t('hostname'), 'hostname', CURRENT.hostname);
  const ddesc = document.createElement('div'); ddesc.style.cssText = 'grid-column:1/-1';
  ddesc.innerHTML = `<label>${t('description')}</label><input id="gen-description" value="${esc(CURRENT.description||'')}" style="width:100%">`;
  grow.appendChild(ddesc);
  g.append(grow);
  const acts = document.createElement('div'); acts.className = 'acts';
  const btn = document.createElement('button'); btn.className = 'save'; btn.textContent = t('saveGeneral'); btn.type = 'button';
  const st = document.createElement('span'); st.className = 'status';
  btn.onclick = async () => {
    btn.disabled = true; st.className = 'status'; st.textContent = t('saving');
    const r = await fetch('/api/general' + q({ dir: CURRENT.path }), { method:'POST', headers:{'Content-Type':'application/json'},
      body: JSON.stringify({ project: $('#gen-project').value.trim(), main: $('#gen-main').value.trim(), hostname: $('#gen-hostname').value.trim(), description: $('#gen-description').value.trim() }) });
    const res = await r.text();
    if (r.ok) { st.className = 'status ok'; st.textContent = t('saved'); loadEstates(); selectEstate(ESTATES.find(x => x.path === CURRENT.path)); }
    else { st.className = 'status err'; st.textContent = res; }
    btn.disabled = false;
  };
  acts.append(btn, st); g.append(acts); sec.appendChild(g);

  /* --- local dev: start `eco up dev`, poll until the local URL appears --- */
  const dev = document.createElement('div'); dev.className = 'general';
  const dh = document.createElement('h3'); dh.textContent = t('localDev'); dev.appendChild(dh);
  const dacts = document.createElement('div'); dacts.className = 'acts';
  const devBtn = document.createElement('button'); devBtn.className = 'save'; devBtn.textContent = t('startDev'); devBtn.type = 'button';
  const devSt = document.createElement('span'); devSt.className = 'status';
  dacts.append(devBtn, devSt); dev.appendChild(dacts);
  const devLog = document.createElement('details'); devLog.style.display = 'none'; devLog.open = true;
  const devLogSum = document.createElement('summary'); devLogSum.textContent = t('devLog'); devLog.appendChild(devLogSum);
  const devPre = document.createElement('pre'); devPre.style.cssText = 'font:11px/1.4 "DM Mono",ui-monospace,monospace; max-height:300px; overflow:auto; background:#141018; color:#c9d3dd; border-radius:8px; padding:.6rem; margin:.5rem 0 0; white-space:pre-wrap;';
  devLog.appendChild(devPre); dev.appendChild(devLog);
  sec.appendChild(dev);

  let pollTimer = null;
  const showLog = () => { devLog.style.display = 'block'; };
  const appendLog = (lines) => {
    if (lines && lines.length) { showLog(); devPre.textContent = lines.join('\n'); devPre.scrollTop = devPre.scrollHeight; }
  };
  const refreshLocal = (url) => {
    const loc = $('#openLocal');
    if (url) { CURRENT.localUrl = url; loc.href = url; loc.hidden = false; }
  };
  const mkBtn = (label, cls, fn) => { const b = document.createElement('button'); b.className = cls || 'save'; b.type = 'button'; b.textContent = label; b.onclick = fn; return b; };
  async function pm2(action) {
    const r = await fetch('/api/dev-pm2' + q({ dir: CURRENT.path, action }), { method:'POST' });
    const d = await r.json();
    if (!r.ok) { devSt.className = 'status err'; devSt.textContent = d.error || action; }
    pollDev();
  }
  function renderDevButtons(d) {
    dacts.innerHTML = '';
    const running = d.pm2 && d.pm2.running;
    if (running) {
      dacts.appendChild(mkBtn(t('pm2Pause'), 'ghost', () => pm2('pause')));
      dacts.appendChild(mkBtn(t('pm2Stop'), 'ghost', () => pm2('stop')));
      dacts.appendChild(mkBtn(t('pm2Delete'), 'ghost', () => pm2('delete')));
      devSt.className = 'status ok'; devSt.textContent = t('devRunning');
      const apps = (d.pm2.apps || []).map(a => a.name + ' · ' + a.status).join(', ');
      if (apps) devSt.textContent += ' — ' + apps;
    } else if (d.running) {
      const b = mkBtn(t('startDev'), 'save', () => {}); b.disabled = true; dacts.appendChild(b);
      devSt.className = 'status'; devSt.textContent = t('devStarting');
    } else {
      dacts.appendChild(mkBtn(t('startDev'), 'save', startDev));
      devSt.className = 'status'; devSt.textContent = '';
    }
  }
  async function pollDev() {
    const r = await fetch('/api/dev-status' + q({ dir: CURRENT.path }));
    if (!r.ok) return;
    const d = await r.json();
    refreshLocal(d.localUrl);
    appendLog(d.log);
    renderDevButtons(d);
    if (d.running) {
      if (pollTimer) clearTimeout(pollTimer); pollTimer = setTimeout(pollDev, 2000);
    } else {
      if (pollTimer) clearTimeout(pollTimer); pollTimer = null;
    }
  }
  async function startDev() {
    devSt.className = 'status'; devSt.textContent = t('devStarting');
    showLog(); appendLog([t('devStarting')]);
    const r = await fetch('/api/dev-up' + q({ dir: CURRENT.path }), { method:'POST' });
    const res = await r.text();
    if (!r.ok) { devSt.className = 'status err'; devSt.textContent = res.includes('already running') ? t('devAlready') : res; }
    pollDev();
  }
  pollDev();
}
function renderLXS() {
  const sec = $('#p-lxs');
  const core = CURRENT.services.filter(s => s.kind === 'core');
  const lxs = CURRENT.services.filter(s => s.kind !== 'core');
  const renderGroup = (title, badgeCls, badgeLabel) => {
    const hd = document.createElement('div'); hd.className = 'sect'; hd.textContent = title; sec.appendChild(hd);
  };
  renderGroup(t('coreDomain'));
  for (const svc of core) sec.appendChild(serviceBlock(svc, 'core', t('badgeCore')));
  renderGroup(t('reusableLxs'));
  for (const svc of lxs) sec.appendChild(serviceBlock(svc, 'lxs', t('badgeLxs')));
}
function serviceBlock(svc, kind, badgeLabel) {
  const el = document.createElement('details'); el.className = 'service'; el.id = 'svc-' + cssId(svc.name);
  const sum = document.createElement('summary');
  const icon = kind === 'lxs'
    ? `<img class="svcicon" src="/favicon-32.png" alt="">`
    : `<span class="svcicon core">◈</span>`;
  const pub = svc.publisher ? ` <span class="pub">· ${esc(svc.publisher)}</span>` : '';
  sum.innerHTML = `<span class="caret">▸</span>${icon}<code>${esc(svc.name)}</code> <span class="badge ${kind}">${esc(badgeLabel)}</span> <span class="lxs">${esc(svc.lxs || '')}</span>${pub}`;
  el.appendChild(sum);
  if (!svc.fields.length) {
    const hint = document.createElement('div'); hint.className = 'empty';
    hint.textContent = svc.lxs ? t('lxsNoSchema') : t('sourceService');
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
    const btn = document.createElement('button'); btn.className = 'save'; btn.textContent = t('saveService') + ' ' + svc.name; btn.type = 'button';
    const st = document.createElement('span'); st.className = 'status';
    btn.onclick = async () => {
      btn.disabled = true; st.className = 'status'; st.textContent = t('saving');
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
      const res = await r.text();
      if (r.ok) { st.className = 'status ok'; st.textContent = t('runUp'); loadEstates(); }
      else { st.className = 'status err'; st.textContent = res; }
      btn.disabled = false;
    };
    act.append(btn, st); el.appendChild(act);
  }
  return el;
}
function inputFor(f, val) {
  if (f.managed) return `<input type="text" value="" disabled placeholder="managed by eco — ${esc(f.managed)}">`;
  if (f.type === 'bool') {
    const on = val === 'true' || val === '1';
    return `<div class="bool"><label><input type="radio" name="${cssId(f.key)}" value="true" ${on?'checked':''}> Yes</label>
      <label><input type="radio" name="${cssId(f.key)}" value="false" ${!on?'checked':''}> No</label></div>`;
  }
  if (f.type === 'enum') {
    const opts = f.choices.map(c => `<option value="${esc(c)}" ${c===val?'selected':''}>${esc(c)}</option>`).join('');
    return `<select name="${cssId(f.key)}">${opts}</select>`;
  }
  if (f.type === 'int' || f.type === 'float') return `<input type="number" step="${f.type==='float'?'any':'1'}" name="${cssId(f.key)}" value="${esc(val)}">`;
  if (f.type === 'csv' || f.type === 'csv-url') return `<input type="text" name="${cssId(f.key)}" value="${esc(val)}" placeholder="comma-separated: a,b,c">`;
  if (f.type === 'json') return `<textarea name="${cssId(f.key)}" rows="3">${esc(val)}</textarea>`;
  return `<input type="${f.secret?'password':'text'}" name="${cssId(f.key)}" value="${f.secret?'':esc(val)}" placeholder="${f.secret?'hidden — leave blank to keep':''}">`;
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
  info.textContent = t('rawInfo');
  sec.appendChild(info);
  const ta = document.createElement('textarea'); ta.id = 'raw'; ta.value = RAW; sec.appendChild(ta);
  const bar = document.createElement('div'); bar.style.cssText = 'display:flex;gap:.7rem;align-items:center;margin-top:.7rem;';
  const btn = document.createElement('button'); btn.className = 'save'; btn.textContent = t('saveEcompose'); btn.type = 'button';
  const st = document.createElement('span'); st.className = 'status';
  btn.onclick = async () => {
    btn.disabled = true; st.className = 'status'; st.textContent = t('saving');
    const r2 = await fetch('/api/ecompose-save' + q({ dir: CURRENT.path }), { method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({ content: ta.value }) });
    const res = await r2.text();
    if (r2.ok) { st.className = 'status ok'; st.textContent = t('saved'); loadEstates(); }
    else { st.className = 'status err'; st.textContent = res; }
    btn.disabled = false;
  };
  bar.append(btn, st); sec.appendChild(bar);
}
/* ---------------- prod env ---------------- */
async function renderEnv() {
  const sec = $('#p-env'); sec.innerHTML = '';
  const warn = document.createElement('div'); warn.className = 'warn'; warn.innerHTML = t('envWarn');
  sec.appendChild(warn);
  const bar = document.createElement('div'); bar.className = 'envbar';
  const sel = document.createElement('select');
  for (const s of CURRENT.services) { if (s.lxs) { const o = document.createElement('option'); o.value = s.name; o.textContent = s.name; sel.appendChild(o); } }
  bar.appendChild(sel);
  const info = document.createElement('span'); info.className = 'envinfo'; bar.appendChild(info);
  sec.appendChild(bar);
  const table = document.createElement('table'); table.className = 'env';
  table.innerHTML = '<thead><tr><th style="width:36%">Key</th><th>Value (prod, public-only)</th></tr></thead><tbody></tbody>';
  sec.appendChild(table);
  const tbody = table.querySelector('tbody');
  async function load() {
    tbody.innerHTML = `<tr><td colspan="2">${t('envLoading')}</td></tr>`;
    info.textContent = '';
    const r = await fetch('/api/prod-env' + q({ dir: CURRENT.path, service: sel.value }));
    const d = await r.json();
    if (!d.available) { tbody.innerHTML = `<tr><td colspan="2">${esc(d.error || t('envNotAvail'))}</td></tr>`; return; }
    info.textContent = d.source;
    tbody.innerHTML = '';
    for (const item of d.env) {
      const tr = document.createElement('tr');
      tr.innerHTML = `<td><code>${esc(item.key)}</code></td><td><code>${esc(item.value)}</code></td>`;
      tbody.appendChild(tr);
    }
    if (!d.env.length) tbody.innerHTML = `<tr><td colspan="2">${t('envEmpty')}</td></tr>`;
  }
  sel.onchange = load;
  await load();
}
/* ---------------- search ---------------- */
let lastQuery = '';
$('#search').addEventListener('input', (e) => {
  const q0 = e.target.value.trim().toLowerCase(); lastQuery = q0;
  if (!CURRENT) return;
  document.querySelectorAll('#p-lxs .field').forEach(f => {
    const hit = !q0 || (f.dataset.key + ' ' + f.dataset.grp + ' ' + f.dataset.service).toLowerCase().includes(q0);
    f.classList.toggle('hl', hit);
  });
  if (q0) {
    const first = document.querySelector('#p-lxs .field.hl');
    if (first) first.scrollIntoView({ behavior:'smooth', block:'center' });
  }
});
/* ---------------- boot ---------------- */
setLang(LANG);
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

fn favicon_content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "application/octet-stream",
    }
}

/// An SVG favicon that references an external image (`<image href="...">` with
/// a path, not a `data:` URI) renders blank when served standalone — treat it
/// as broken so the default icon is used instead (see eco_docs/favicon.svg).
fn svg_renderable(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    if !text.contains("<image") {
        return true;
    }
    text.contains("data:image")
}

/// Read a favicon candidate's bytes, skipping broken SVGs.
fn read_favicon(path: &Path) -> Option<(Vec<u8>, &'static str)> {
    let bytes = std::fs::read(path).ok()?;
    if path.extension().and_then(|e| e.to_str()) == Some("svg") && !svg_renderable(&bytes) {
        return None;
    }
    Some((bytes, favicon_content_type(path)))
}

/// Resolve an estate's favicon. Checks known frontend layouts in preference
/// order (png > ico > svg — svg files sometimes reference a missing master
/// png and render empty), then falls back to a bounded walk for favicon.* .
fn estate_favicon(dir: &Path) -> Option<(Vec<u8>, &'static str)> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for p in [
        "frontend/images/favicon-32.png",
        "frontend/images/favicon-16.png",
        "frontend/images/favicon.ico",
        "frontend/static/favicon.ico",
        "frontend/static/favicon.png",
        "frontend/static/favicon.svg",
        "frontend/public/favicon.png",
        "frontend/public/favicon.ico",
        "frontend/public/favicon.svg",
        "frontend/app/public/favicon.png",
        "frontend/app/public/favicon.ico",
        "frontend/app/public/favicon.svg",
        "frontend/src/lib/assets/favicon.svg",
        "public/favicon.ico",
        "public/favicon.png",
        "public/favicon.svg",
        "static/favicon.ico",
        "static/favicon.png",
        "static/favicon.svg",
        "src/favicon.svg",
    ] {
        candidates.push(dir.join(p));
    }
    for c in &candidates {
        if let Some(fav) = read_favicon(c) {
            return Some(fav);
        }
    }
    // Bounded walk fallback: any favicon.* under the estate (depth ≤ 6),
    // skipping node_modules/build/target/.svelte-kit; png/ico preferred.
    let mut found: Vec<(u8, PathBuf)> = Vec::new(); // priority, path
    let mut stack: Vec<(PathBuf, usize)> = vec![(dir.to_path_buf(), 0)];
    while let Some((cur, depth)) = stack.pop() {
        if depth > 6 {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&cur) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                if name == "node_modules" || name == "build" || name == "target" || name == ".svelte-kit" || name == ".git" {
                    continue;
                }
                if p.is_dir() {
                    stack.push((p, depth + 1));
                } else if name.starts_with("favicon.") {
                    let prio = match p.extension().and_then(|e| e.to_str()) {
                        Some("png") => 0,
                        Some("ico") => 1,
                        Some("svg") => 2,
                        _ => 3,
                    };
                    found.push((prio, p));
                }
            }
        }
    }
    found.sort_by_key(|(prio, _)| *prio);
    for (_, p) in found {
        if let Some(fav) = read_favicon(&p) {
            return Some(fav);
        }
    }
    None
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
    let hostname = ecompose::parse_estates(content).first().map(|e| e.hostname.clone()).unwrap_or_default();
    let registry = registry_address(estate_root);
    let mut svc_values = Vec::new();
    for svc in &services {
        let mut entry = serde_json::Map::new();
        entry.insert("name".into(), serde_json::Value::String(svc.name.clone()));
        entry.insert("lxs".into(), serde_json::Value::String(svc.lxs.clone()));
        // core domain (source path:) vs reusable LXS (registry binary).
        entry.insert("kind".into(), serde_json::Value::String(if svc.lxs.is_empty() { "core".to_string() } else { "lxs".to_string() }));
        let mut config_map = serde_json::Map::new();
        for (k, v) in &svc.config {
            config_map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        entry.insert("config".into(), serde_json::Value::Object(config_map));
        if svc.lxs.is_empty() {
            entry.insert("fields".into(), serde_json::Value::Array(vec![]));
            entry.insert("publisher".into(), serde_json::Value::String(String::new()));
            svc_values.push(serde_json::Value::Object(entry));
            continue;
        }
        let manifest = match load_lxs_manifest(&svc, registry.as_deref()) {
            Ok(Some(m)) => m,
            Ok(None) => {
                entry.insert("fields".into(), serde_json::Value::Array(vec![]));
                entry.insert("publisher".into(), serde_json::Value::String(String::new()));
                svc_values.push(serde_json::Value::Object(entry));
                continue;
            }
            Err(e) => {
                eprintln!("[eco config] cannot load schema for {}: {e}", svc.name);
                entry.insert("fields".into(), serde_json::Value::Array(vec![]));
                entry.insert("publisher".into(), serde_json::Value::String(String::new()));
                svc_values.push(serde_json::Value::Object(entry));
                continue;
            }
        };
        entry.insert("publisher".into(), serde_json::Value::String(manifest.publisher.clone()));
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
        "hostname": hostname,
        "description": top_level_value(content, "description"),
        "prodUrl": if hostname.is_empty() { String::new() } else { format!("https://{}", hostname) },
        "localUrl": local_dev_url(estate_root, &project, &services),
        "services": svc_values,
    }))
}

/// Read a top-level scalar key (e.g. `description:`) from ecompose.yml.
fn top_level_value(content: &str, key: &str) -> String {
    let prefix = format!("{key}:");
    for raw in content.split('\n') {
        let line = raw.trim_end_matches('\r');
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(&prefix) {
            return crate::util::strip_quotes(line[prefix.len()..].trim());
        }
    }
    String::new()
}

/// Best-effort local dev URL for an estate, in order of reliability:
///
///   1. the eco dev port registry (`~/.eco/registry.db` — authoritative for
///      estates managed by `eco up dev`; the ecompose `port:` is a *requested*
///      port, not the dev port, so it is deliberately ignored),
///   2. PM2 `ecosystem.config.js` ports,
///   3. live listening ports whose process command mentions the project
///      (catches manually-run dev servers, e.g. `./target/release/…`),
///   4. otherwise "" (the local link is hidden).
fn local_dev_url(estate_root: &Path, project: &str, services: &[ecompose::Service]) -> String {
    let registry_path = crate::registry::default_registry_path();
    let scope = crate::registry::default_scope();
    let mut preferred: Vec<&ecompose::Service> = services.iter().collect();
    preferred.sort_by_key(|s| {
        if s.name.contains("frontend") {
            0
        } else if s.name.contains("-web") || s.name.contains("-app") || s.name == project {
            1
        } else {
            2
        }
    });

    // 1. Eco dev port registry (real allocated dev ports).
    for svc in &preferred {
        if let Ok(Some(port)) = crate::registry::lookup_port(&registry_path, &scope, project, &svc.name, "service") {
            return format!("http://localhost:{port}");
        }
    }

    // 2. PM2 ecosystem.config.js.
    let ecosystem = estate_root.join("ecosystem.config.js");
    let ports = crate::commands::show::read_ports_from_ecosystem(&ecosystem);
    let frontend_app = format!("{project}-frontend");
    let app_app = format!("{project}-app");
    if let Some(p) = ports.get(&frontend_app).or_else(|| ports.get(&app_app)) {
        return format!("http://localhost:{p}");
    }

    // 3. Live listening ports for this project (any running process whose
    //    command mentions the project name), preferring frontend-ish ones.
    let live = live_listening_ports();
    let proj_l = project.to_lowercase();
    let for_project: Vec<&(String, u16)> = live.iter().filter(|(cmd, _)| cmd.to_lowercase().contains(&proj_l)).collect();
    for svc in &preferred {
        let name_l = svc.name.to_lowercase();
        if let Some((_, port)) = for_project.iter().find(|(cmd, _)| cmd.to_lowercase().contains(&name_l)) {
            return format!("http://localhost:{port}");
        }
    }
    if let Some((_, port)) = for_project.first() {
        return format!("http://localhost:{port}");
    }

    // 4. Lowest PM2 port as a last resort.
    if let Some(p) = ports.values().min() {
        return format!("http://localhost:{p}");
    }
    String::new()
}

/// `lsof -nP -iTCP -sTCP:LISTEN -Fc -Fn` → (command, port) pairs.
fn live_listening_ports() -> Vec<(String, u16)> {
    let out = match std::process::Command::new("lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fc", "-Fn"]).output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let mut result = Vec::new();
    let mut cmd = String::new();
    for line in String::from_utf8_lossy(&out).lines() {
        if let Some(c) = line.strip_prefix('c') {
            cmd = c.to_string();
        } else if let Some(n) = line.strip_prefix('n') {
            if let Some(port) = n.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                if !cmd.is_empty() {
                    result.push((cmd.clone(), port));
                }
            }
        }
    }
    result
}

/// Start `eco up dev` for an estate in the background (one at a time).
fn dev_session_running() -> bool {
    let mut guard = DEV_SESSION.lock().unwrap();
    match guard.as_mut() {
        Some(s) => {
            match s.child.try_wait() {
                Ok(Some(_status)) => {
                    guard.take();
                    false
                }
                _ => true,
            }
        }
        None => false,
    }
}

fn start_dev_session(dir: &Path) -> Result<(), String> {
    if dev_session_running() {
        return Err("eco up dev already running — wait for it to finish (or stop it in a terminal)".to_string());
    }
    let project = std::fs::read_to_string(dir.join("ecompose.yml"))
        .map(|c| ecompose::parse_project_name(&c))
        .unwrap_or_default();
    let log_path = std::env::temp_dir().join(format!("eco-genie-dev-{}.log", if project.is_empty() { "estate" } else { &project }));
    let file = std::fs::File::create(&log_path).map_err(|e| format!("cannot write {}: {e}", log_path.display()))?;
    let child = Command::new(std::env::current_exe().map_err(|e| format!("current exe: {e}"))?)
        .args(["up", "dev", "--no-lxs-check"])
        .current_dir(dir)
        .env("ECO_NON_INTERACTIVE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(file.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(file))
        .spawn()
        .map_err(|e| format!("cannot spawn eco up dev: {e}"))?;
    let mut guard = DEV_SESSION.lock().unwrap();
    *guard = Some(DevSession {
        child,
        dir: dir.display().to_string(),
        log_path,
    });
    Ok(())
}

/// Locate the pm2 binary (PATH fallbacks for non-interactive launches).
fn pm2_bin() -> String {
    if let Ok(v) = std::env::var("ECO_GENIE_PM2") {
        if !v.is_empty() {
            return v;
        }
    }
    for c in [
        "/Users/eco/node/bin/pm2".to_string(),
        "/opt/homebrew/bin/pm2".to_string(),
        "/usr/local/bin/pm2".to_string(),
        format!("{}/.local/bin/pm2", util::home_dir()),
    ] {
        if Path::new(&c).is_file() {
            return c;
        }
    }
    "pm2".to_string()
}

/// The estate's PM2 apps (name, status) — apps named `<project>-*`.
fn pm2_estate_apps(project: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let output = match Command::new(pm2_bin()).arg("jlist").output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return out,
    };
    let text = match String::from_utf8(output) {
        Ok(t) => t,
        Err(_) => return out,
    };
    let arr: Vec<serde_json::Value> = match serde_json::from_str(&text) {
        Ok(a) => a,
        Err(_) => return out,
    };
    let prefix = format!("{project}-");
    for a in arr {
        if let Some(name) = a["name"].as_str() {
            if name.starts_with(&prefix) {
                let status = a["pm2_env"]["status"].as_str().unwrap_or("unknown").to_string();
                out.push((name.to_string(), status));
            }
        }
    }
    out
}

fn run_pm2_action(project: &str, action: &str) -> Result<Vec<String>, String> {
    if !["pause", "stop", "delete", "restart"].contains(&action) {
        return Err(format!("invalid pm2 action: {action}"));
    }
    let apps = pm2_estate_apps(project);
    if apps.is_empty() {
        return Err(format!("no PM2 apps running for project '{project}'"));
    }
    let names: Vec<&String> = apps.iter().map(|(n, _)| n).collect();
    let status = Command::new(pm2_bin())
        .arg(action)
        .args(&names)
        .status()
        .map_err(|e| format!("pm2 {action} failed: {e}"))?;
    if !status.success() {
        return Err(format!("pm2 {action} exited with {:?}", status.code()));
    }
    Ok(names.into_iter().cloned().collect())
}

/// Detect a running dev service by command-name needles (e.g. "auth-backend"),
/// returning its base URL. An env override wins when set.
fn detect_dev_url(env_key: &str, needles: &[&str]) -> Option<String> {
    if let Ok(v) = std::env::var(env_key) {
        if !v.trim().is_empty() {
            return Some(v.trim_end_matches('/').to_string());
        }
    }
    let live = live_listening_ports();
    for (cmd, port) in &live {
        let c = cmd.to_lowercase();
        if needles.iter().any(|n| c.contains(n)) {
            return Some(format!("http://localhost:{port}"));
        }
    }
    None
}

fn auth_base_url() -> Option<String> {
    detect_dev_url("ECO_GENIE_AUTH_URL", &["auth-backend", "auth_backend", "auth-back"])
}
fn profile_base_url() -> Option<String> {
    detect_dev_url("ECO_GENIE_PROFILE_URL", &["profile-backend", "profile_backend"])
}

/// Forward a request to a detected dev LXS and return its raw response.
fn forward(server: &str, path: &str, method: &str, bearer: Option<&str>, body: Option<&str>) -> Result<(u16, String), String> {
    let url = format!("{server}{path}");
    let mut req = ureq::request(method, &url);
    if let Some(t) = bearer {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    if let Some(b) = body {
        req = req.set("Content-Type", "application/json");
    }
    let resp = match req.send_string(body.unwrap_or("")) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => return Ok((code as u16, r.into_string().unwrap_or_default())),
        Err(e) => return Err(format!("auth upstream unreachable: {e}")),
    };
    let status = resp.status();
    let text = resp.into_string().unwrap_or_default();
    Ok((status as u16, text))
}

/// Authenticated identity + profile avatar for the dashboard header: auth LXS
/// `/auth/session`, then profile LXS avatar URL when a profile instance is up.
fn dashboard_me(bearer: Option<&str>) -> Result<(u16, String), String> {
    let auth = auth_base_url().ok_or_else(|| "no dev auth running".to_string())?;
    let (status, text) = forward(&auth, "/api/auth/session", "GET", bearer, None)?;
    if !(200..300).contains(&status) {
        return Ok((status, text));
    }
    let user: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("bad auth session: {e}"))?;
    let user_id = user["id"].as_str().unwrap_or("");
    let mut avatar_url = String::new();
    if let Some(profile) = profile_base_url() {
        if !user_id.is_empty() {
            if let Ok((_, ptext)) = forward(&profile, &format!("/api/users/{user_id}"), "GET", bearer, None) {
                if let Ok(pjson) = serde_json::from_str::<serde_json::Value>(&ptext) {
                    if let Some(a) = pjson["avatarUrl"].as_str() {
                        avatar_url = a.to_string();
                    }
                }
            }
        }
    }
    Ok((200, serde_json::json!({ "user": user, "avatarUrl": avatar_url }).to_string()))
}

fn dev_session_status(dir: &Path) -> serde_json::Value {    let mut running = false;
    let mut done = false;
    let mut exited_ok = false;
    let mut log_tail: Vec<String> = Vec::new();
    let mut take = false;
    {
        let mut guard = DEV_SESSION.lock().unwrap();
        if let Some(s) = guard.as_mut() {
            if s.dir != dir.display().to_string() {
                return serde_json::json!({ "running": false, "done": false, "log": [], "localUrl": "" });
            }
            match s.child.try_wait() {
                Ok(Some(status)) => {
                    done = true;
                    exited_ok = status.success();
                    take = true;
                }
                _ => running = true,
            }
            if let Ok(text) = std::fs::read_to_string(&s.log_path) {
                let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
                log_tail = lines.into_iter().rev().take(24).collect();
                log_tail.reverse();
            }
        }
        if take {
            guard.take();
        }
    }
    let local_url = {
        let content = std::fs::read_to_string(dir.join("ecompose.yml")).unwrap_or_default();
        let services = ecompose::parse_services(&content);
        local_dev_url(dir, &ecompose::parse_project_name(&content), &services)
    };
    let (pm2_apps, pm2_running) = {
        let content = std::fs::read_to_string(dir.join("ecompose.yml")).unwrap_or_default();
        let project = ecompose::parse_project_name(&content);
        let apps = pm2_estate_apps(&project);
        let running = apps.iter().any(|(_, s)| s == "online");
        (apps, running)
    };
    serde_json::json!({
        "running": running,
        "done": done,
        "ok": if done { exited_ok } else { false },
        "log": log_tail,
        "localUrl": local_url,
        "pm2": {
            "running": pm2_running,
            "apps": pm2_apps.iter().map(|(n, s)| serde_json::json!({ "name": n, "status": s })).collect::<Vec<_>>(),
        },
    })
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

/// Replace top-level scalar keys (project, main, description, hostname under
/// estates). Leaves everything else untouched.
fn apply_general(content: &str, project: &str, main: &str, hostname: &str, description: &str) -> Result<String, String> {
    if project.trim().is_empty() {
        return Err("project name cannot be empty".to_string());
    }
    let mut out: Vec<String> = Vec::new();
    let mut in_estates = false;
    let mut hostname_done = false;
    let mut project_done = false;
    let mut main_done = false;
    let mut desc_done = false;
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
        if trimmed.starts_with("description:") && !line.starts_with(' ') {
            out.push(format!("description: \"{}\"", description.trim()));
            desc_done = true;
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
    let mut insert_at = 0;
    if !project_done {
        out.insert(0, format!("project: {}", project.trim()));
        insert_at += 1;
    } else {
        insert_at = 1;
    }
    if !main_done && !main.trim().is_empty() {
        out.insert(insert_at, format!("main: {}", main.trim()));
        insert_at += 1;
    } else if main_done {
        insert_at += 1;
    }
    if !desc_done && !description.trim().is_empty() {
        out.insert(insert_at, format!("description: \"{}\"", description.trim()));
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

/// Neutral default favicon for estates without one: a rounded tile with the
/// project's initial — not the Ecosphere brand (that mark is reserved for
/// first-party LXS, where it identifies the publisher).
fn default_estate_favicon(project: &str) -> (Vec<u8>, &'static str) {
    let letter = project
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "E".to_string());
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="8" fill="#e9e4f2"/><text x="16" y="22" text-anchor="middle" font-family="sans-serif" font-size="16" font-weight="700" fill="#5b3fd6">{letter}</text></svg>"##
    );
    (svg.into_bytes(), "image/svg+xml")
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
            ("GET", "/favicon-32.png") => {
                let _ = request.respond(
                    Response::from_data(FAVICON_32.to_vec())
                        .with_status_code(200)
                        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"image/png"[..]).unwrap()),
                );
                continue;
            }
            ("GET", "/favicon-16.png") => {
                let _ = request.respond(
                    Response::from_data(FAVICON_16.to_vec())
                        .with_status_code(200)
                        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"image/png"[..]).unwrap()),
                );
                continue;
            }
            ("GET", "/api/estate-favicon") => {
                let dir = query.get("dir").cloned().unwrap_or_default();
                // Estate favicon; estates without one get a neutral initial
                // tile (not the Ecosphere brand).
                let (bytes, ctype) = match resolve_estate_dir(&root, &dir).ok().and_then(|d| estate_favicon(&d)) {
                    Some((b, c)) => (b, c),
                    None => {
                        let project = resolve_estate_dir(&root, &dir)
                            .ok()
                            .and_then(|d| std::fs::read_to_string(d.join("ecompose.yml")).ok())
                            .map(|c| ecompose::parse_project_name(&c))
                            .unwrap_or_default();
                        default_estate_favicon(&project)
                    }
                };
                let _ = request.respond(
                    Response::from_data(bytes)
                        .with_status_code(200)
                        .with_header(Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes()).unwrap()),
                );
                continue;
            }
            ("GET", "/api/estates") => {
                let list: Vec<serde_json::Value> = estates
                    .iter()
                    .map(|(project, path, _services)| {
                        let dir = PathBuf::from(path);
                        let content = std::fs::read_to_string(dir.join("ecompose.yml")).unwrap_or_default();
                        let hostname = ecompose::parse_estates(&content).first().map(|e| e.hostname.clone()).unwrap_or_default();
                        let services = ecompose::parse_services(&content);
                        serde_json::json!({
                            "project": project,
                            "path": path,
                            "description": top_level_value(&content, "description"),
                            "services": services.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                            "prodUrl": if hostname.is_empty() { String::new() } else { format!("https://{}", hostname) },
                            "localUrl": local_dev_url(&dir, project, &services),
                        })
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
                        let description = req["description"].as_str().unwrap_or("").to_string();
                        let next = apply_general(&content, &project, &main, &hostname, &description)?;
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
            ("POST", "/api/dev-up") => {
                let dir = query.get("dir").cloned().unwrap_or_default();
                match resolve_estate_dir(&root, &dir).and_then(|d| start_dev_session(&d)) {
                    Ok(_) => (200, "{\"ok\":true}".to_string(), "application/json"),
                    Err(e) => json_error(400, &e),
                }
            }
            ("GET", "/api/dev-status") => {
                let dir = query.get("dir").cloned().unwrap_or_default();
                match resolve_estate_dir(&root, &dir) {
                    Ok(d) => (200, dev_session_status(&d).to_string(), "application/json"),
                    Err(e) => json_error(400, &e),
                }
            }
            ("POST", "/api/dev-pm2") => {
                let dir = query.get("dir").cloned().unwrap_or_default();
                let action = query.get("action").cloned().unwrap_or_default();
                match resolve_estate_dir(&root, &dir) {
                    Ok(dir) => {
                        let project = std::fs::read_to_string(dir.join("ecompose.yml"))
                            .map(|c| ecompose::parse_project_name(&c))
                            .unwrap_or_default();
                        match run_pm2_action(&project, &action) {
                            Ok(acted_on) => (200, serde_json::json!({ "ok": true, "actedOn": acted_on }).to_string(), "application/json"),
                            Err(e) => json_error(400, &e),
                        }
                    }
                    Err(e) => json_error(400, &e),
                }
            }
            ("GET", "/api/auth-config") => {
                (200, serde_json::json!({
                    "authAvailable": auth_base_url().is_some(),
                    "profileAvailable": profile_base_url().is_some(),
                }).to_string(), "application/json")
            }
            ("POST", "/api/auth/login") => {
                let mut buf = Vec::new();
                let _ = request.as_reader().read_to_end(&mut buf);
                match auth_base_url() {
                    Some(auth) => match forward(&auth, "/api/auth/login", "POST", None, Some(&String::from_utf8_lossy(&buf))) {
                        Ok((st, body)) => (st, body, "application/json"),
                        Err(e) => json_error(502, &e),
                    },
                    None => json_error(503, "no dev auth running — start an estate with eco up dev (or set ECO_GENIE_AUTH_URL)"),
                }
            }
            ("GET", "/api/auth/session") => {
                let bearer = query.get("token").map(|s| s.as_str());
                match auth_base_url() {
                    Some(auth) => match forward(&auth, "/api/auth/session", "GET", bearer, None) {
                        Ok((st, body)) => (st, body, "application/json"),
                        Err(e) => json_error(502, &e),
                    },
                    None => json_error(503, "no dev auth running"),
                }
            }
            ("POST", "/api/auth/logout") => {
                let mut buf = Vec::new();
                let _ = request.as_reader().read_to_end(&mut buf);
                let body = String::from_utf8_lossy(&buf).to_string();
                let bearer: Option<String> = serde_json::from_str(&body).ok()
                    .and_then(|v: serde_json::Value| v["token"].as_str().map(|s| s.to_string()));
                match auth_base_url() {
                    Some(auth) => match forward(&auth, "/api/auth/logout", "POST", bearer.as_deref(), None) {
                        Ok((st, body)) => (st, body, "application/json"),
                        Err(e) => json_error(502, &e),
                    },
                    None => json_error(503, "no dev auth running"),
                }
            }
            ("GET", "/api/me") => {
                let bearer = query.get("token").map(|s| s.as_str());
                match dashboard_me(bearer) {
                    Ok((st, body)) => (st, body, "application/json"),
                    Err(e) => json_error(502, &e),
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
    fn apply_general_updates_project_main_hostname_description() {
        let content = "project: old\nmain: old\n\ndescription: \"Old desc\"\nestates:\n  old:\n    hostname: old.example.com\n    services: []\n\nservices:\n  a:\n    lxs: x@1.0.0\n";
        let out = apply_general(content, "new", "new", "new.example.com", "A new estate").unwrap();
        assert!(out.starts_with("project: new\nmain: new\n"));
        assert!(out.contains("description: \"A new estate\""));
        assert!(!out.contains("Old desc"));
        assert!(out.contains("    hostname: new.example.com"));
        assert!(out.contains("services:\n  a:\n    lxs: x@1.0.0"), "services must survive: {out}");
    }

    #[test]
    fn apply_general_inserts_description_when_missing() {
        let content = "project: x\nservices:\n  a:\n    lxs: x@1.0.0\n";
        let out = apply_general(content, "x", "", "", "Fresh description").unwrap();
        assert!(out.starts_with("project: x\ndescription: \"Fresh description\"\n"), "{out}");
        assert!(out.contains("services:\n  a:\n    lxs: x@1.0.0"));
    }
}
