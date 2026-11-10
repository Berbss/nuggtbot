// Streamlabs-chatbox emulation for the bot's overlay host. A pasted Streamlabs
// chat-box theme only styles/observes; its messages normally come from the
// Streamlabs widget runtime. This fills #log from the /events SSE feed using the
// theme's #chatlist_item template (or a default), rendering emotes + badges, so
// the pasted theme works unchanged. No-op if the theme has no #log.
(function () {
  var log = document.getElementById('log');
  if (!log) return;
  var tplEl = document.getElementById('chatlist_item');
  var tpl = tplEl ? tplEl.innerHTML
    : '<div data-from="{from}" data-id="{messageId}"><span class="meta" style="color:{color}"><span class="badges"></span><span class="name">{from}</span></span> <span class="message">{message}</span></div>';
  var MAX = 100;
  // Overlay-only ignore lists injected by the host page (window.__ovFilter).
  var F = window.__ovFilter || {};
  var ignUsers = new Set(String(F.users || '').split('\n').map(function (x) { return x.trim().toLowerCase(); }).filter(Boolean));
  var ignPrefixes = String(F.prefixes || '').split('\n').map(function (x) { return x.trim(); }).filter(Boolean);
  var ignRewards = new Set((F.rewards || []).map(String));
  function ignored(m) {
    if (ignRewards.size && m.reward && ignRewards.has(String(m.reward))) return true;
    if (ignUsers.has(String(m.login || '').toLowerCase()) || ignUsers.has(String(m.from || '').toLowerCase())) return true;
    if (ignPrefixes.length) {
      var t = (m.parts || []).filter(function (p) { return p.type === 'text'; }).map(function (p) { return p.text; }).join('').trim();
      for (var i = 0; i < ignPrefixes.length; i++) if (t.indexOf(ignPrefixes[i]) === 0) return true;
    }
    return false;
  }
  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"]/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c];
    });
  }
  function render(m) {
    if (ignored(m)) return;
    var html = tpl
      .replace(/\{from\}/g, esc(m.from))
      .replace(/\{messageId\}/g, esc(m.id))
      .replace(/\{color\}/g, esc(m.color || '#ffffff'))
      .replace(/\{message\}/g, '')
      .replace(/\{[a-zA-Z_]+\}/g, '');
    var wrap = document.createElement('div');
    wrap.innerHTML = html.trim();
    var node = wrap.firstElementChild;
    if (!node) return;
    node.setAttribute('data-id', m.id || '');
    node.setAttribute('data-from', m.from || '');
    var bslot = node.querySelector('.badges');
    if (bslot && m.badges) {
      m.badges.forEach(function (u) {
        if (/^https:/.test(u) || /^\/badges\/kick\//.test(u)) {
          var i = document.createElement('img');
          i.className = 'badge'; i.src = u; i.style.height = '1em'; i.style.verticalAlign = 'middle';
          bslot.appendChild(i);
        }
      });
    }
    var mslot = node.querySelector('.message');
    if (mslot) {
      (m.parts || []).forEach(function (p) {
        if (p.type === 'text') mslot.appendChild(document.createTextNode(p.text));
        else if (p.type === 'emote' && /^https:/.test(p.url)) {
          // Streamlabs emote markup: .emote wrapper carries the image as a
          // background (so the theme's CSS controls size/3D), inner img just
          // sizes the box. A theme without .emote CSS still shows the img.
          var span = document.createElement('span');
          span.className = 'emote';
          span.style.backgroundImage = "url('" + p.url + "')";
          var i = document.createElement('img');
          i.src = p.url; i.style.verticalAlign = 'middle';
          span.appendChild(i);
          mslot.appendChild(span);
        }
      });
    }
    log.appendChild(node);
    while (log.children.length > MAX) log.removeChild(log.firstChild);
  }
  function P(fn) { return function (e) { try { fn(JSON.parse(e.data)); } catch (_) { } }; }
  var src = new EventSource('/events');
  src.addEventListener('message', P(render));
  src.addEventListener('delete', P(function (d) {
    var ns = log.querySelectorAll('[data-id]');
    for (var k = 0; k < ns.length; k++) if (ns[k].getAttribute('data-id') === d.id) ns[k].remove();
  }));
  src.addEventListener('clear', P(function () { log.innerHTML = ''; }));
})();
