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

  function concernsThisPage(m){
    return !m.pages || !m.pages.length || m.pages.indexOf(location.pathname) !== -1;
  }

  function onMessage(e){
    var m;
    try { m = JSON.parse(e.data); } catch (err) { return; }
    if (!m || typeof m !== 'object') { return; }
    if (m.type === 'resync') { location.reload(); return; }
    if (m.type === 'reload' && concernsThisPage(m)) { location.reload(); }
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
