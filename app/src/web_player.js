// Injected into every frame of VRT's pages before their own scripts run.
// Hides the page around the <video>, takes commands from redjelly, accepts
// the consent banner, fills the login form when asked, and reports the
// player state every 500 ms (child frames via the top frame).
(() => {
  if (window.__rj) return;
  const TOP = window.top === window;

  function deepAll(sel, root = document, out = []) {
    root.querySelectorAll(sel).forEach(e => out.push(e));
    root.querySelectorAll('*').forEach(e => { if (e.shadowRoot) deepAll(sel, e.shadowRoot, out); });
    return out;
  }
  function video() {
    const vs = deepAll('video');
    return vs.find(v => v.readyState > 0 && v.videoWidth > 0) || vs.find(v => v.readyState > 0) || vs[0] || null;
  }
  const clamp = (x, a, b) => Math.max(a, Math.min(b, x));

  // --- page chrome: everything but the video is hidden once it loads ---
  // vrt.be's CSP blocks injected <style> elements and style attributes, but
  // not constructable stylesheets or CSSOM property writes.
  let chrome = true;
  let videoFrame = null; // iframe element (in this document) holding the video
  const FILL = { visibility: 'visible', position: 'fixed', inset: '0', width: '100vw', height: '100vh',
    'max-width': 'none', 'max-height': 'none', 'z-index': '2147483647', 'object-fit': 'contain',
    background: '#000', transform: 'none', margin: '0', border: '0' };
  // Only ever undo what we set: VRT's player owns the video's inline styles.
  function fill(el, on) {
    if (!el || !!el.__rjSaved === on) return;
    if (on) {
      el.__rjSaved = {};
      for (const [k, v] of Object.entries(FILL)) {
        el.__rjSaved[k] = [el.style.getPropertyValue(k), el.style.getPropertyPriority(k)];
        el.style.setProperty(k, v, 'important');
      }
    } else {
      for (const [k, [v, prio]] of Object.entries(el.__rjSaved)) {
        if (v) el.style.setProperty(k, v, prio); else el.style.removeProperty(k);
      }
      el.__rjSaved = null;
    }
  }
  let sheet = null;
  function applyChrome() {
    try {
      if (!sheet) {
        sheet = new CSSStyleSheet();
        document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
      }
      sheet.replaceSync(chrome ? '' :
        'html,body{background:#000 !important;overflow:hidden !important} body *{visibility:hidden !important}');
    } catch (e) {}
    fill(video(), !chrome);
    fill(videoFrame, !chrome);
  }

  // --- commands from redjelly (the top frame relays them to every frame) ---
  function cmd(c, arg) {
    for (let i = 0; i < window.frames.length; i++) window.frames[i].postMessage({ rj: 'cmd', c, arg }, '*');
    const v = video();
    if (!v) return;
    const s = v.seekable, end = s.length ? s.end(s.length - 1) : v.duration;
    const to = t => { v.currentTime = s.length ? clamp(t, s.start(0), end - 1) : Math.max(0, t); };
    switch (c) {
      case 'toggle': v.paused ? v.play() : v.pause(); break;
      case 'seek': to(v.currentTime + arg); break;
      case 'seekto': to(arg); if (v.paused) v.play(); break;
      case 'live': if (s.length) v.currentTime = end - 3; if (v.paused) v.play(); break;
      case 'vol': v.muted = false; v.volume = clamp(arg, 0, 1); break;
      case 'mute': v.muted = !v.muted; break;
      case 'subs': {
        const tt = Array.from(v.textTracks).filter(t => t.kind !== 'metadata');
        const cur = tt.findIndex(t => t.mode === 'showing');
        tt.forEach(t => { t.mode = 'disabled'; });
        if (cur + 1 < tt.length) tt[cur + 1].mode = 'showing';
        break;
      }
    }
  }

  // --- consent banner: accept all ---
  let consentClicks = 0;
  function consent() {
    if (consentClicks >= 3) return;
    const re = /^\s*(alles? (accepteren|aanvaarden|toestaan)|accepteer alle(s| cookies)?|alle cookies (accepteren|aanvaarden)|akkoord|accept all|ik ga akkoord)\s*$/i;
    const b = deepAll('button,[role=button],a').find(e => re.test(e.textContent || '') && e.offsetParent !== null);
    if (b) { b.click(); consentClicks++; }
  }

  // --- login form (redjelly only sends credentials to a top-level login.vrt.be page) ---
  let loginClicks = 0, lastLoginAt = 0;
  function setVal(el, v) {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, v);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  }
  function login(user, pass) {
    if (!TOP || location.hostname !== 'login.vrt.be' || loginClicks >= 4) return;
    // One attempt per 4 s, so a slow page isn't hammered into a captcha.
    if (Date.now() - lastLoginAt < 4000) return;
    lastLoginAt = Date.now();
    const vis = e => e.offsetParent !== null;
    const u = deepAll('input[type=email],input[name=loginID],input[name=email],input[name=username],input[autocomplete=username]').find(vis);
    const p = deepAll('input[type=password]').find(vis);
    if (u && u.value !== user) setVal(u, user);
    if (p && p.value !== pass) setVal(p, pass);
    if (!u && !p) return;
    const btn = deepAll('button[type=submit],input[type=submit]').find(vis) ||
      deepAll('button').find(e => vis(e) && /aanmelden|inloggen|log in|doorgaan|volgende|verder/i.test(e.textContent || ''));
    const form = (p || u).form;
    loginClicks++;
    setTimeout(() => { if (btn) btn.click(); else if (form) form.requestSubmit(); }, 400);
  }

  // --- state: child frames -> top -> redjelly ---
  function notices() {
    // Visible error text from VRT's player or the login page.
    return deepAll('[class*=error],[class*=Error],[role=alert],[class*=message],[class*=notice]')
      .filter(e => e.offsetParent !== null).map(e => (e.textContent || '').trim().slice(0, 120)).filter(Boolean).slice(0, 3);
  }
  function state() {
    const v = video();
    const base = { frame: TOP ? 'top' : location.origin, url: location.href, msgs: notices(), login: loginClicks };
    if (!v) return Object.assign(base, { video: false });
    const s = v.seekable;
    return Object.assign(base, {
      video: true, t: v.currentTime, d: isFinite(v.duration) ? v.duration : -1, paused: v.paused, vol: v.volume,
      muted: v.muted, rs: v.readyState, seek: s.length ? [s.start(0), s.end(s.length - 1)] : null,
      tracks: Array.from(v.textTracks).filter(t => t.kind !== 'metadata').map(t => ({ lang: t.language || '', label: t.label || '', on: t.mode === 'showing' })),
    });
  }
  window.addEventListener('message', e => {
    const m = e.data;
    if (!m || typeof m !== 'object') return;
    if (m.rj === 'cmd' && !TOP && e.source === window.parent) cmd(m.c, m.arg);
    if (m.rj === 'state' && TOP && m.st && typeof m.st === 'object') {
      const st = Object.assign({}, m.st, { frame: e.origin });
      if (st.video) {
        // Make the iframe holding the video fill the top page too.
        const f = Array.from(document.querySelectorAll('iframe')).find(f => f.contentWindow === e.source);
        if (f && f !== videoFrame) videoFrame = f;
        if (st.rs >= 1) chrome = false;
        applyChrome();
      }
      window.ipc.postMessage(JSON.stringify(st));
    }
  });
  setInterval(() => {
    const v = video();
    if (chrome && v && v.readyState >= 1) chrome = false;
    applyChrome(); consent();
    const st = state();
    if (TOP) window.ipc.postMessage(JSON.stringify(st));
    else if (st.video) window.top.postMessage({ rj: 'state', st }, '*');
  }, 500);

  window.__rj = { cmd, login };
})();
