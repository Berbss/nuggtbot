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
  // Streamlabs serves 1x badge/emote images; a theme without explicit sizing
  // relies on that natural size, so use the 1x variant of each CDN URL.
  function x1(u) {
    return String(u)
      .replace(/^(https:\/\/static-cdn\.jtvnw\.net\/badges\/v1\/[^/]+)\/[23]$/, '$1/1')
      .replace(/^(https:\/\/static-cdn\.jtvnw\.net\/emoticons\/v2\/.+)\/[23]\.0$/, '$1/1.0')
      .replace(/^(https:\/\/cdn\.(?:7tv\.app|betterttv\.net)\/emote\/[^/]+)\/[234]x(\.webp)?$/, '$1/1x$2');
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
      m.badges.forEach(function (u, n) {
        if (/^https:/.test(u) || /^\/badges\/kick\//.test(u)) {
          var i = document.createElement('img');
          // Streamlabs badge markup: class="badge <type>-icon".
          var t = String((m.badgeTypes || [])[n] || '').replace(/[^a-zA-Z0-9_-]/g, '');
          i.src = x1(u); i.className = 'badge' + (t ? ' ' + t + '-icon' : '');
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
          var eu = x1(p.url);
          span.style.backgroundImage = "url('" + eu + "')";
          var i = document.createElement('img');
          i.src = eu;
          span.appendChild(i);
          mslot.appendChild(span);
        }
      });
    }
    log.appendChild(node);
    while (log.children.length > MAX) log.removeChild(log.firstChild);
    // Streamlabs custom JS listens for this on every chat message.
    var tags = m.tags || {
      'display-name': m.from, color: m.color || '', id: m.id, 'user-id': m.userId || '',
      badges: (m.badgeTypes || []).join(','),
      mod: has(m, 'moderator'), subscriber: has(m, 'subscriber')
    };
    var login = m.login || m.from, body = m.text != null ? m.text : '';
    // Same shape Streamlabs passes (measured on the real widget); no tokens.
    fire('onEventReceived', {
      command: 'PRIVMSG', from: login, body: body, messageId: m.id,
      platform: m.platform + '_account', tags: tags, priority: 10, pic: '',
      owner: /(^|,)broadcaster\//.test(tags.badges || '') || has(m, 'broadcaster') === '1',
      subscriber: tags.subscriber === '1', userType: tags['user-type'] || 'user',
      payload: { command: 'PRIVMSG', tags: tags, params: ['#' + login], crlf: body, prefix: login + '!' + login + '@' + login + '.tmi.twitch.tv' }
    });
  }
  function has(m, type) { return (m.badgeTypes || []).indexOf(type) >= 0 ? '1' : '0'; }
  function fire(name, detail) {
    try { document.dispatchEvent(new CustomEvent(name, { detail: detail })); } catch (_) { }
  }
  function P(fn) { return function (e) { try { fn(JSON.parse(e.data)); } catch (_) { } }; }
  var src = new EventSource('/events');
  src.addEventListener('message', P(render));
  src.addEventListener('delete', P(function (d) {
    var ns = log.querySelectorAll('[data-id]');
    // Like Streamlabs: mark as .deleted (themes style it, default is hidden).
    for (var k = 0; k < ns.length; k++) if (ns[k].getAttribute('data-id') === d.id) {
      ns[k].classList.add('deleted'); ns[k].style.visibility = 'hidden';
    }
  }));
  src.addEventListener('clear', P(function () { log.innerHTML = ''; }));
  fire('onLoad', {});
})();
