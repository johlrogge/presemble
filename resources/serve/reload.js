// Reader live-reload script for Presemble serve mode.
//
// Injected into pages served to anonymous readers. It connects to the public
// `/_presemble/ws` socket, which carries only page reloads and resyncs, and
// reloads the current page when it is affected. It never navigates to another
// page and contains no editor code.
//
// TODO(Wave 3): merge into the TypeScript build alongside the editor bundle.
(function(){
  'use strict';
  var attempts = 0;
  var lost = false;

  // BEGIN reader-reload
  // Pure functions, no DOM: tools/protocol-fixtures.ts evaluates this region
  // and checks it against resources/serve/protocol/public/*.json.

  // Whether the reader's page at `pathname` reloads for this message of the
  // public socket. Readers never navigate: a rebuild of other pages does
  // nothing here.
  function readerReloads(message, pathname){
    var m = message || {};
    if (m.type === 'resync') { return true; }
    if (m.type !== 'reload') { return false; }
    if (m.resync || !m.pages || !m.pages.length) { return true; }
    var here = samePagePath(pathname);
    for (var i = 0; i < m.pages.length; i++) {
      if (samePagePath(m.pages[i]) === here) { return true; }
    }
    return false;
  }

  // The server answers a page at several paths (`/post/`, `/post`,
  // `/post/index.html`); this is the one form they are compared in: without a
  // trailing `index.html`, then without a trailing `/` (except `/` itself).
  // Twin of `_samePagePath` in inject.js; the protocol fixtures keep the two
  // in step until the Wave 3 bundle shares one.
  function samePagePath(path){
    var p = String(path);
    if (p.slice(-'index.html'.length) === 'index.html') { p = p.slice(0, -'index.html'.length); }
    if (p.length > 1 && p.charAt(p.length - 1) === '/') { p = p.slice(0, -1); }
    return p === '' ? '/' : p;
  }
  // END reader-reload

  function onMessage(e){
    var m;
    try { m = JSON.parse(e.data); } catch (err) { return; }
    if (!m || typeof m !== 'object') { return; }
    if (readerReloads(m, location.pathname)) { location.reload(); }
  }

  function connect(){
    var sock = new WebSocket((location.protocol === 'https:' ? 'wss:' : 'ws:') + '//' + location.host + '/_presemble/ws');
    sock.onopen = function(){
      var recovered = lost;
      attempts = 0;
      lost = false;
      // Messages may have been missed while disconnected.
      if (recovered) { location.reload(); }
    };
    sock.onmessage = onMessage;
    sock.onclose = function(){
      lost = true;
      var base = Math.min(30000, 500 * Math.pow(2, Math.min(attempts, 16)));
      attempts++;
      setTimeout(connect, base + Math.floor(Math.random() * base * 0.25));
    };
  }

  connect();
})();
