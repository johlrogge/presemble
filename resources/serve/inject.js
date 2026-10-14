(function(){
// ADR-059 section 10: the server addresses every editable element
// (data-presemble-address) and projects its canonical markdown
// (data-presemble-md). The browser edits that markdown and sends it back
// through POST /_presemble/block-edit; it builds no selections and no NED
// programs of its own.

// BEGIN protocol-builders
// Pure functions, no DOM: tools/protocol-fixtures.ts evaluates this region and
// checks it against resources/serve/protocol/*.json, as the Rust side does.

// The editable target of an element, from its emitted attributes
// (`getAttribute` results, so null when absent). An element without both an
// address and markdown is not editable: there is no fallback.
function editTarget(address, md) {
  if (address === null || address === undefined || address === '') return null;
  if (md === null || md === undefined) return null;
  return { address: address, md: md };
}

// The POST /_presemble/block-edit request for one edit. `intent` is 'apply' or
// 'suggest'; `reason` is optional.
function buildBlockEditRequest(file, selection, baseMarkdown, proposedMarkdown, intent, reason) {
  var req = {
    file: file,
    selection: selection,
    base: { markdown: baseMarkdown },
    proposed: { markdown: proposedMarkdown },
    intent: intent
  };
  if (reason) req.reason = reason;
  return req;
}

// The markdown of a link with this text and destination: `\`, `[` and `]` in
// the text are escaped; a destination with anything but plain characters is
// written in angle brackets, with `\`, `<` and `>` escaped.
function linkMarkdown(text, href) {
  var t = String(text).replace(/([\\\[\]])/g, '\\$1');
  var h = String(href);
  var d = /^[^\s()<>\\]*$/.test(h) ? h : '<' + h.replace(/([\\<>])/g, '\\$1') + '>';
  return '[' + t + '](' + d + ')';
}

// The block edit the link picker sends for the option `text`/`href`: apply
// `[text](href)` at `selection`, the link element or the empty slot's
// placeholder, with `baseMarkdown` its markdown now ('' for an empty slot).
function buildLinkPickerRequest(file, selection, baseMarkdown, text, href) {
  return buildBlockEditRequest(file, selection, baseMarkdown, linkMarkdown(text, href), 'apply');
}

// Read a block-edit response into what the browser acts on:
//   {kind:'applied', pages}        the edit was applied
//   {kind:'suggestion-created', id} stored as a pending suggestion
//   {kind:'no-change'}             nothing to do
//   {kind:'refused', cause, message} not done; the editor stays open
function interpretBlockEditResponse(response) {
  if (response && response.refused) {
    return { kind: 'refused', cause: response.refused.cause, message: response.refused.message };
  }
  var done = response && response.done;
  if (done === 'no-change') return { kind: 'no-change' };
  if (done && done['applied']) return { kind: 'applied', pages: done['applied'].pages };
  if (done && done['suggestion-created']) {
    return { kind: 'suggestion-created', id: done['suggestion-created'].id };
  }
  return { kind: 'refused', cause: 'unknown', message: 'Unexpected response from the server.' };
}

// What the page does with a message of the editorial socket: the name of the
// action for each type it knows, and 'ignore' for any other. A type the page
// does not know is never taken as a reload: that would reload every open
// editor page, and could drop what is being typed.
function editorialMessageAction(message) {
  var type = message && message.type;
  switch (type) {
    case 'reload':
    case 'scroll':
    case 'resync':
    case 'conflict':
    case 'conflict-resolved':
    case 'suggestion-list-changed':
    case 'save-state':
      return type;
    default:
      return 'ignore';
  }
}

// What the page at `pathname` does with a reload message:
//   {kind:'reload'}          reload in place
//   {kind:'navigate', url}   go to the page the edit was made on
//   {kind:'none'}            nothing
// The server answers a page at several paths (`/post/`, `/post`,
// `/post/index.html`), so paths are compared in one form: without a trailing
// `index.html`, then without a trailing `/` (except `/` itself).
function reloadDecision(message, pathname) {
  var m = message || {};
  var pages = m.pages;
  if (m.resync || !pages || !pages.length) return { kind: 'reload' };
  var here = _samePagePath(pathname);
  for (var i = 0; i < pages.length; i++) {
    if (_samePagePath(pages[i]) === here) return { kind: 'reload' };
  }
  if (m.primary) return { kind: 'navigate', url: m.primary };
  return { kind: 'none' };
}

// One form for every path the server answers a page at (see reloadDecision).
function _samePagePath(path) {
  var p = String(path);
  if (p.slice(-'index.html'.length) === 'index.html') p = p.slice(0, -'index.html'.length);
  if (p.length > 1 && p.charAt(p.length - 1) === '/') p = p.slice(0, -1);
  return p === '' ? '/' : p;
}

// The documents a page shows, from its [data-presemble-file] values: each
// non-empty value once, sorted. A listing page shows fields of other
// documents too, and their suggestions belong to those documents' files.
function pageFiles(values) {
  var seen = {};
  var files = [];
  for (var i = 0; i < values.length; i++) {
    var v = values[i];
    if (v && !Object.prototype.hasOwnProperty.call(seen, v)) {
      seen[v] = true;
      files.push(v);
    }
  }
  return files.sort();
}

// The URL listing the suggestions of `files`, in the given order; null for
// no files (nothing to ask for).
function suggestionListUrl(files) {
  if (!files.length) return null;
  return '/_presemble/ned-suggestions?' + files.map(function (f) {
    return 'file=' + encodeURIComponent(f);
  }).join('&');
}

// What the block editor does once the server has answered a submit with
// `intent` ('apply' | 'suggest'): keep it open (a refusal: the user's text
// stays), put the rendered markup back (inline editor only; the body editor
// never changes the rendered element), and what to refresh after closing.
function submitOutcomePlan(intent, outcomeKind) {
  if (outcomeKind === 'refused') {
    return { keepOpen: true, restoreMarkup: false, after: 'none' };
  }
  // A suggestion is not an edit: the page shows the original again.
  if (intent === 'suggest') {
    return { keepOpen: false, restoreMarkup: true, after: 'after-suggest' };
  }
  return {
    keepOpen: false,
    restoreMarkup: outcomeKind === 'no-change',
    after: outcomeKind === 'applied' ? 'dirty-count' : 'none',
  };
}

// The modes, by the names setMode uses ('schema' is the Structure mode).
var _MODES = ['edit', 'schema', 'suggest', 'view'];

// What switching from mode `from` to mode `to` does with the open block
// editor, whose state `editor` is 'none' | 'unchanged' | 'changed' | 'busy'
// (busy: a submit is in flight).
// An unchanged editor closes; a busy one refuses any switch; a changed one
// refuses view and structure but moves between edit and suggest, keeping its
// text and taking the new mode's save intent.
function modeSwitchDecision(from, to, editor) {
  if (from === to) return { kind: 'none' };
  if (editor === 'none') return { kind: 'switch' };
  if (editor === 'unchanged') return { kind: 'close-and-switch' };
  if (editor === 'busy') return { kind: 'refuse', reason: 'saving' };
  if (to === 'edit') return { kind: 'switch-keeping-editor', intent: 'apply' };
  if (to === 'suggest') return { kind: 'switch-keeping-editor', intent: 'suggest' };
  return { kind: 'refuse', reason: 'save-or-cancel' };
}

// The modes the user can switch to from `current` with the editor in state
// `editor`, sorted (see modeSwitchDecision).
function availableModes(current, editor) {
  if (editor === 'busy') return [current];
  if (editor === 'changed') {
    return _MODES.filter(function (m) {
      return m === current || m === 'edit' || m === 'suggest';
    });
  }
  return _MODES.slice();
}
// END protocol-builders

// Single source of truth for the heading text used in scroll anchors (ADR-046).
// Used when resolving anchors in the DOM.
function _headingAnchorText(el) {
  return normalizeAnchorText(el.innerText);
}

// Fetch a URL and parse the response body as JSON.
// Throws when the body is not JSON, or when the status is not 2xx (using the
// server's `error` field when present). Callers still check `data.ok` for
// endpoints that report logical failure with a 200.
async function fetchJson(url, opts) {
  var r = await fetch(url, opts);
  var text = await r.text();
  var data;
  try {
    data = JSON.parse(text);
  } catch (_e) {
    if (!r.ok) throw new Error('HTTP ' + r.status);
    throw new Error('Expected JSON from ' + url + ' (HTTP ' + r.status + ')');
  }
  if (!r.ok) throw new Error((data && data.error) || ('HTTP ' + r.status));
  return data;
}

// Single, reusable fixed-position notice for errors that have no natural
// element to anchor to. `sticky` keeps it visible until replaced.
var _toastEl = null;
var _toastTimer = null;
function _toast(msg, sticky) {
  if (!_toastEl) {
    _toastEl = document.createElement('div');
    _toastEl.className = 'presemble-toast';
    _toastEl.setAttribute('role', 'status');
    _toastEl.setAttribute('aria-live', 'polite');
  }
  if (!_toastEl.isConnected) document.body.appendChild(_toastEl);
  _toastEl.textContent = String(msg);
  _toastEl.style.display = 'block';
  clearTimeout(_toastTimer);
  if (!sticky) {
    _toastTimer = setTimeout(function() { _toastEl.style.display = 'none'; }, 6000);
  }
}

// Editor-session helpers of the block editor (_openBlockEditor).
function _setToolbarBusy(toolbar, busy) {
  toolbar.querySelectorAll('button').forEach(function(b) { b.disabled = busy; });
  toolbar.classList.toggle('presemble-busy', busy);
}
// Replace `prev` (may be null) with a fresh error element after `afterEl`.
function _showEditError(afterEl, prev, msg) {
  if (prev) prev.remove();
  var d = document.createElement('div');
  d.className = 'presemble-edit-error';
  d.setAttribute('role', 'alert');
  d.textContent = msg;
  afterEl.after(d);
  return d;
}

// The server's address and markdown for an element, or null when it lacks
// either (not editable: there is no fallback).
var _NOT_EDITABLE_MSG = 'This element cannot be edited here: the server gave it no address.';
function _editTargetOf(el) {
  return editTarget(el.getAttribute('data-presemble-address'), el.getAttribute('data-presemble-md'));
}
// Markdown equal for editing purposes: line endings and surrounding blank
// space do not count.
function _sameMd(a, b) {
  function norm(s) { return String(s).replace(/\r\n?/g, '\n').trim(); }
  return norm(a) === norm(b);
}
// The text of a contenteditable element as typed (non-breaking spaces are
// what the browser inserts for runs of spaces).
function _editedText(el) {
  return el.innerText.replace(/\xa0/g, ' ').trim();
}
// POST one block edit and read the response (see interpretBlockEditResponse).
// Rejects on a transport or server error; a refusal resolves as kind 'refused'.
function _postBlockEdit(req) {
  return fetchJson('/_presemble/block-edit', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify(req),
  }).then(interpretBlockEditResponse);
}
// After a suggestion was stored: enter suggest mode on it, or refresh the
// count. Nothing to do when the edit changed nothing.
function _afterSuggest(outcome) {
  if (outcome.kind === 'suggestion-created'
      && document.body.classList.contains('presemble-suggest-mode') && window._suggestEnter) {
    window._suggestEnter(outcome.id);
  } else if (window._fetchSuggestionCount) {
    window._fetchSuggestionCount();
  }
}

// --- Phase D: URL-fragment mode helpers (ADR-042) ---

// Parse a URL hash into a presemble mode name.
// Returns 'view' | 'edit' | 'suggest' | 'unknown'
// Note: '#_schema' is no longer a valid hash-based mode; schema mode is
// detected via location.pathname starting with '/_schema/'.
function parsePresembleHash(hash) {
  if (!hash || hash === '#') return 'view';
  var bare = hash.replace(/^#/, '');
  switch (bare) {
    case '_edit':    return 'edit';
    case '_suggest': return 'suggest';
    case '':         return 'view';
    default:         return 'unknown';
  }
}

// --- end Phase D helpers (setPresembleMode is defined inside the mode-management IIFE below) ---

// --- Live-reload socket + reload deferral while an editor is open ---
var _wsAttempts=0;var _wsLost=false;var _pendingReload=false;
// The one open block editor (see _openBlockEditor), or null.
var _activeEditor=null;
function _editorOpen(){return !!_activeEditor;}
// The open editor's state for the mode rules (modeSwitchDecision): 'none'
// when no editor is open.
function _editorState(){return _activeEditor?_activeEditor.state():'none';}
// The editor's state changed: the mode buttons follow (availableModes).
function _editorStateChanged(){if(window._refreshModeControls){window._refreshModeControls();}}
// Put back the markup a suggestion preview replaced, and forget it.
function _resetPreview(el){
el.innerHTML=el.getAttribute('data-presemble-original-html');
el.removeAttribute('data-presemble-original-html');
}
function _requestReload(m){
var decision=reloadDecision(m,location.pathname);
if(_editorOpen()){
// Never navigate away from an open editor; defer a same-page reload until it closes.
if(decision.kind==='reload'){
_pendingReload=true;
_toast('Page updated — will refresh when you finish editing',true);
}
return;
}
if(decision.kind==='reload'){_selfReload=true;location.reload();}
else if(decision.kind==='navigate'){_selfReload=true;location.href=decision.url;}
}
// The page is reloading itself because the site was rebuilt: that is not the
// editor leaving, so it sends no leave hint (the flush would cut short the
// idle time auto-save waits for after every edit).
var _selfReload=false;
// Call at the end of every editor cleanup.
function _onEditorClosed(){if(_pendingReload&&!_editorOpen()){_selfReload=true;location.reload();}}
function _wsConnect(){
var sock=new WebSocket((location.protocol==='https:'?'wss:':'ws:')+'//'+location.host+'/_presemble/editorial-ws');
var opened=false;
sock.onopen=function(){
opened=true;
var recovered=_wsLost;
_wsAttempts=0;_wsLost=false;
// Messages may have been missed while disconnected: treat as a same-page reload.
if(recovered){_requestReload({resync:true});}
};
sock.onmessage=_wsOnMessage;
sock.onclose=function(){
_wsLost=true;
if(!opened){fetch('/_presemble/session').then(function(r){if(r.status===401){_requestReload({resync:true});}}).catch(function(){});}
var base=Math.min(30000,500*Math.pow(2,Math.min(_wsAttempts,16)));
_wsAttempts++;
setTimeout(_wsConnect,base+Math.floor(Math.random()*base*0.25));
};
}
var _userScrolled=false;var _scrollTimer=null;var _presembleScrolling=false;
window.addEventListener('scroll',function(){if(_presembleScrolling){return;}_userScrolled=true;clearTimeout(_scrollTimer);_scrollTimer=setTimeout(function(){_userScrolled=false;},3000);},true);
function _wsOnMessage(e){
var m;
try{m=JSON.parse(e.data);}catch(err){console.warn('presemble: bad ws message',err);return;}
var action=editorialMessageAction(m);
if(action==='ignore'){
console.debug('presemble: ignoring an editorial message the page does not act on',m.type);
return;
}
if(action==='scroll'){
if(m.anchor&&typeof m.anchor==='object'){
var el=_findByStructuralAnchor(m.anchor);
if(el){
var r=el.getBoundingClientRect();
if(r.top<0||r.bottom>window.innerHeight){
_presembleScrolling=true;
el.scrollIntoView({behavior:'smooth',block:'center'});
setTimeout(function(){_presembleScrolling=false;},500);
}
}
}else if(m.anchor){
console.warn('presemble: legacy string anchor ignored');
}
return;
}
if(action==='resync'){
_requestReload({resync:true});
return;
}
if(action==='conflict'){
_noteConflict(m.file,m.state);
return;
}
if(action==='conflict-resolved'){
_conflictEnded(m.file);
return;
}
if(action==='save-state'){
_noteSaveState(m);
return;
}
if(action==='suggestion-list-changed'){
if(window._fetchSuggestionFiles){window._fetchSuggestionFiles();}
if(window._fetchSuggestionCount){window._fetchSuggestionCount();}
return;
}
// action === 'reload'
if(m.anchor){sessionStorage.setItem('presemble-anchor',JSON.stringify(m.anchor));}
_requestReload(m);
}
// ── Disk conflicts (ADR-065) ──────────────────────────────────────────────
// A file that changed on disk while it has unsaved edits here. A whole-file
// conflict gets the notice: it is persistent and has no buttons, the toolbar
// Save (with its confirmation) is the one way to keep the edits and
// `presemble reload` keeps the file. A conflict on blocks is shown on its
// blocks instead (below), each with its two ways out.
var _conflictFiles=[];
// Files whose conflict is on blocks, as the socket last said.
var _conflictOnBlocks={};
function _showConflictNotice(){
document.querySelectorAll('.presemble-conflict-notice').forEach(function(n){n.remove();});
var onPage={};
document.querySelectorAll('[data-presemble-file]').forEach(function(el){onPage[el.getAttribute('data-presemble-file')]=true;});
var shownOnBlocks=_conflictBlockFiles();
var here=_conflictFiles.filter(function(f){return onPage[f]&&!shownOnBlocks[f]&&!_conflictOnBlocks[f];});
if(!here.length){return;}
var box=document.createElement('div');
box.className='presemble-conflict-notice';
box.setAttribute('role','alert');
here.forEach(function(file){
var p=document.createElement('p');
p.textContent=file+' changed on disk while it has unsaved edits here. To keep your edits, use Save: you will be asked to confirm overwriting the file. To keep the file on disk, run `presemble reload <site-dir> '+file+'`.';
box.appendChild(p);
});
document.body.appendChild(box);
}
function _fetchConflicts(){
fetchJson('/_presemble/conflicts')
.then(function(list){
if(!Array.isArray(list)){return;}
_conflictFiles=list;
_showConflictNotice();
})
.catch(function(err){console.warn('presemble: conflicts poll failed',err);});
}
function _noteConflict(file,state){
if(_conflictFiles.indexOf(file)<0){_conflictFiles.push(file);}
delete _conflictGone[file];
if(state==='blocks'){_conflictOnBlocks[file]=true;}else{delete _conflictOnBlocks[file];}
// The conductor renders the page again with the blocks marked; its reload
// message brings them.
_showConflictNotice();
}
// The conflict of `file` has ended: its markers and its notice go now (the
// page may be reloaded later, or not at all while an editor is open).
function _conflictEnded(file){
_conflictFiles=_conflictFiles.filter(function(f){return f!==file;});
delete _conflictOnBlocks[file];
_conflictGone[file]=true;
document.querySelectorAll('[data-presemble-conflict]').forEach(function(el){
var rest=_conflictParse(el.getAttribute('data-presemble-conflict')).filter(function(c){return c.file!==file;});
if(rest.length){el.setAttribute('data-presemble-conflict',JSON.stringify(rest));}
else{el.removeAttribute('data-presemble-conflict');}
});
_conflictRender();
_showConflictNotice();
}
// ── Disk conflicts on their blocks (ADR-065 amendment F2) ─────────────────
// The server marks a block whose file changed differently from its unsaved
// edits with `data-presemble-conflict` (a JSON array of
// {id,file,disk,message}), and lists the ones no element can carry in the page
// script (`conflicts`). Each gets the file's version and two buttons. Like the
// diagnostics, the markers live in their own overlay layer, never inside the
// content; text reaches the DOM only as text nodes.
var _conflictGone={};
var _conflictBound=false;
function _conflictParse(text){
var list;
try{list=JSON.parse(text);}catch(err){return [];}
if(!Array.isArray(list)){return [];}
return list.filter(function(c){return c&&typeof c.id==='string'&&typeof c.file==='string'&&typeof c.message==='string'&&!_conflictGone[c.file];});
}
function _conflictsUnplaced(){
var script=document.getElementById('presemble-diagnostics');
if(!script){return [];}
var data;
try{data=JSON.parse(script.textContent);}catch(err){return [];}
return data&&Array.isArray(data.conflicts)?_conflictParse(JSON.stringify(data.conflicts)):[];
}
// The files this page shows conflicted blocks of.
function _conflictBlockFiles(){
var files={};
document.querySelectorAll('[data-presemble-conflict]').forEach(function(el){
_conflictParse(el.getAttribute('data-presemble-conflict')).forEach(function(c){files[c.file]=true;});
});
_conflictsUnplaced().forEach(function(c){files[c.file]=true;});
return files;
}
function _resolveConflict(c,take,status,buttons){
buttons.forEach(function(b){b.disabled=true;});
status.classList.remove('presemble-conflict-failed');
status.textContent='Resolving…';
fetchJson('/_presemble/resolve-conflict',{
method:'POST',
headers:{'Content-Type':'application/json'},
body:JSON.stringify({file:c.file,id:c.id,take:take})
})
.then(function(data){
if(!data||data.ok!==true){throw new Error((data&&data.error)||'the conflict was not resolved');}
status.textContent='Resolved. The page will update.';
})
.catch(function(err){
buttons.forEach(function(b){b.disabled=false;});
status.classList.add('presemble-conflict-failed');
status.textContent='Could not resolve this conflict: '+err.message;
});
}
function _conflictBox(c){
var box=document.createElement('div');
box.className='presemble-conflict-box';
box.addEventListener('click',function(ev){ev.stopPropagation();});
var msg=document.createElement('p');
msg.textContent=c.message;
box.appendChild(msg);
if(typeof c.disk==='string'){
var label=document.createElement('p');
label.textContent="The file's version:";
box.appendChild(label);
var disk=document.createElement('pre');
disk.className='presemble-conflict-disk';
disk.textContent=c.disk;
box.appendChild(disk);
}
var status=document.createElement('p');
status.className='presemble-conflict-status';
status.setAttribute('role','status');
var take=document.createElement('button');
take.type='button';
take.textContent="Take the file's version of this block";
var keep=document.createElement('button');
keep.type='button';
keep.textContent='Keep the unsaved version';
var buttons=[take,keep];
take.addEventListener('click',function(){_resolveConflict(c,'disk',status,buttons);});
keep.addEventListener('click',function(){_resolveConflict(c,'unsaved',status,buttons);});
var row=document.createElement('div');
row.className='presemble-conflict-actions';
row.appendChild(take);
row.appendChild(keep);
box.appendChild(row);
box.appendChild(status);
return box;
}
function _conflictPlace(){
var layer=document.querySelector('.presemble-conflict-layer');
if(!layer){return;}
layer.querySelectorAll('.presemble-conflict-badge').forEach(function(b){
var el=b._conflictTarget;
if(!el||!el.isConnected){b.remove();return;}
var r=el.getBoundingClientRect();
b.style.top=(r.top+window.scrollY-10)+'px';
b.style.left=(r.left+window.scrollX-6)+'px';
});
}
function _conflictRender(){
document.querySelectorAll('.presemble-conflict-layer,.presemble-conflict-panel').forEach(function(n){n.remove();});
document.querySelectorAll('.presemble-conflict-block').forEach(function(el){el.classList.remove('presemble-conflict-block');});
var layer=document.createElement('div');
layer.className='presemble-conflict-layer';
document.querySelectorAll('[data-presemble-conflict]').forEach(function(el){
var list=_conflictParse(el.getAttribute('data-presemble-conflict'));
if(!list.length){return;}
el.classList.add('presemble-conflict-block');
var badge=document.createElement('div');
badge.className='presemble-conflict-badge';
badge._conflictTarget=el;
var open=document.createElement('button');
open.type='button';
open.className='presemble-conflict-badge-button';
open.textContent='Conflict';
open.setAttribute('aria-label','This block conflicts with the file on disk');
open.addEventListener('click',function(ev){ev.stopPropagation();badge.classList.toggle('presemble-conflict-open');});
badge.appendChild(open);
var pop=document.createElement('div');
pop.className='presemble-conflict-popover';
list.forEach(function(c){pop.appendChild(_conflictBox(c));});
badge.appendChild(pop);
layer.appendChild(badge);
});
if(layer.childNodes.length){document.body.appendChild(layer);_conflictPlace();}
var unplaced=_conflictsUnplaced();
if(unplaced.length){
var panel=document.createElement('div');
panel.className='presemble-conflict-panel';
panel.setAttribute('role','alert');
var head=document.createElement('p');
head.textContent=unplaced.length===1?'A block changed on disk that is no longer on this page:':'Blocks changed on disk that are no longer on this page:';
panel.appendChild(head);
unplaced.forEach(function(c){panel.appendChild(_conflictBox(c));});
document.body.appendChild(panel);
}
if(!_conflictBound){
_conflictBound=true;
window.addEventListener('resize',_conflictPlace);
window.addEventListener('load',_conflictPlace);
document.addEventListener('click',function(){document.querySelectorAll('.presemble-conflict-open').forEach(function(b){b.classList.remove('presemble-conflict-open');});});
}
}
// ── Diagnostics (FU-7b) ───────────────────────────────────────────────────
// The server tags the element a diagnostic belongs to with
// `data-presemble-diagnostics` (a JSON array of {severity,message}) and puts the
// page-level counts and the unplaced ones in the JSON script element whose id is
// presemble-diagnostics.
// Messages quote user content: they only ever reach the DOM as text nodes.
// Badges live in one overlay layer (never inside the content, so editors that
// read an element's text never see them); `_diagRender` removes what it made
// before adding again, and the window listeners are registered once.
var _diagBound=false;
function _diagParse(text){
var list;
try{list=JSON.parse(text);}catch(err){return [];}
if(!Array.isArray(list)){return [];}
return list.filter(function(d){return d&&typeof d.message==='string'&&(d.severity==='error'||d.severity==='warning');});
}
// The page's error and warning counts from the #presemble-diagnostics script;
// none when it is missing or can't be parsed.
function _pageDiagCounts(){
var script=document.getElementById('presemble-diagnostics');
if(!script){return {errors:0,warnings:0};}
var data=null;
try{data=JSON.parse(script.textContent);}catch(err){data=null;}
if(!data||typeof data!=='object'){return {errors:0,warnings:0};}
return {errors:Number(data.errors)||0,warnings:Number(data.warnings)||0};
}
function _diagWorst(list){return list.some(function(d){return d.severity==='error';})?'error':'warning';}
function _diagMessagesBox(list){
var box=document.createElement('div');
box.className='presemble-diag-messages';
list.forEach(function(d){
var line=document.createElement('div');
line.className='presemble-diag-line presemble-diag-line-'+d.severity;
line.textContent=d.message;
box.appendChild(line);
});
return box;
}
function _diagPlaceBadges(){
var layer=document.querySelector('.presemble-diag-layer');
if(!layer){return;}
layer.querySelectorAll('.presemble-diag-badge').forEach(function(b){
var el=b._diagTarget;
if(!el||!el.isConnected){b.remove();return;}
var r=el.getBoundingClientRect();
b.style.top=(r.top+window.scrollY-8)+'px';
b.style.left=(r.right+window.scrollX-12)+'px';
});
}
function _diagRender(){
document.querySelectorAll('.presemble-diag-layer,.presemble-diag-panel').forEach(function(n){n.remove();});
document.querySelectorAll('.presemble-diag-error,.presemble-diag-warning').forEach(function(el){el.classList.remove('presemble-diag-error','presemble-diag-warning');});
var layer=document.createElement('div');
layer.className='presemble-diag-layer';
document.querySelectorAll('[data-presemble-diagnostics]').forEach(function(el){
var list=_diagParse(el.getAttribute('data-presemble-diagnostics'));
if(!list.length){return;}
var worst=_diagWorst(list);
el.classList.add('presemble-diag-'+worst);
var badge=document.createElement('button');
badge.type='button';
badge.className='presemble-diag-badge presemble-diag-badge-'+worst;
badge.textContent=String(list.length);
badge.setAttribute('aria-label',list.length+(list.length===1?' problem':' problems')+' on this element');
badge._diagTarget=el;
var pop=_diagMessagesBox(list);
pop.classList.add('presemble-diag-popover');
badge.appendChild(pop);
badge.addEventListener('click',function(ev){ev.stopPropagation();badge.classList.toggle('presemble-diag-open');});
layer.appendChild(badge);
});
if(layer.childNodes.length){document.body.appendChild(layer);_diagPlaceBadges();}
var data=null;
var script=document.getElementById('presemble-diagnostics');
if(script){try{data=JSON.parse(script.textContent);}catch(err){data=null;}}
if(data&&typeof data==='object'){
var errors=Number(data.errors)||0;
var warnings=Number(data.warnings)||0;
var unplaced=Array.isArray(data.unplaced)?_diagParse(JSON.stringify(data.unplaced)):[];
if(errors>0||warnings>0){
var panel=document.createElement('div');
panel.className='presemble-diag-panel';
panel.setAttribute('role','status');
var head=document.createElement('button');
head.type='button';
head.className='presemble-diag-panel-head';
head.textContent=diagSummary(errors,warnings);
var body=document.createElement('div');
body.className='presemble-diag-panel-body';
if(unplaced.length){body.appendChild(_diagMessagesBox(unplaced));}
else{body.style.display='none';}
head.addEventListener('click',function(){panel.classList.toggle('presemble-diag-collapsed');});
panel.appendChild(head);
panel.appendChild(body);
document.body.appendChild(panel);
}
}
if(!_diagBound){
_diagBound=true;
window.addEventListener('resize',_diagPlaceBadges);
window.addEventListener('load',_diagPlaceBadges);
document.addEventListener('click',function(){document.querySelectorAll('.presemble-diag-open').forEach(function(b){b.classList.remove('presemble-diag-open');});});
}
}
// ── Save state (ADR-070) ──────────────────────────────────────────────────
// The page's document is saved on its own (auto-save): a small indicator says
// where it stands (Saved, Unsaved, Conflict, Save failed; the last save time
// on hover). A failed save gets a banner with the error and Retry (an
// explicit save of the document); it stays until a save succeeds. Leaving or
// hiding the page asks for the document to be saved now; with auto-save off
// the server saves nothing.
// The page's own document, as the page script names it (not the first
// element with a file: on a collection page that may be one of its items).
var _pageFileCache;
function _pageFile(){
if(_pageFileCache!==undefined){return _pageFileCache;}
_pageFileCache=null;
var script=document.getElementById('presemble-diagnostics');
if(script){
try{var data=JSON.parse(script.textContent);if(data&&typeof data.file==='string'&&data.file){_pageFileCache=data.file;}}
catch(err){console.warn('presemble: unreadable page script',err);}
}
return _pageFileCache;
}
var _saveIndicator=null;
var _saveBanner=null;
// The error of the last failed save, as announced.
var _lastSaveError='';
var _SAVE_LABELS={saved:'Saved',unsaved:'Unsaved',conflict:'Conflict',failed:'Save failed'};
// The last save time of `file` (epoch ms), kept across the page's own reloads.
function _lastSavedAt(file){var v=Number(sessionStorage.getItem('presemble-saved-at:'+file));return v>0?v:null;}
function _showSaveState(file,state){
if(!_SAVE_LABELS[state]){return;}
if(!_saveIndicator){
_saveIndicator=document.createElement('div');
_saveIndicator.className='presemble-save-state';
_saveIndicator.setAttribute('role','status');
_saveIndicator.setAttribute('aria-live','polite');
}
if(!_saveIndicator.isConnected){document.body.appendChild(_saveIndicator);}
_saveIndicator.textContent=_SAVE_LABELS[state];
_saveIndicator.setAttribute('data-state',state);
var at=_lastSavedAt(file);
_saveIndicator.title=at?'Last saved '+new Date(at).toLocaleString():'Not saved since this page was opened';
}
// The banner of a failed save, in one of two modes: 'retry' (the error and
// Retry), or 'confirm' (the file changed on disk: Overwrite it with the
// edits, or Cancel; the choice the toolbar Save offers).
function _showSaveBanner(file,error,mode){
if(!_saveBanner){
_saveBanner=document.createElement('div');
_saveBanner.className='presemble-save-banner';
_saveBanner.setAttribute('role','alert');
var title=document.createElement('strong');
title.textContent='Save failed';
var detail=document.createElement('span');
detail.className='presemble-save-banner-error';
var buttons=document.createElement('span');
buttons.className='presemble-save-banner-actions';
_saveBanner.appendChild(title);
_saveBanner.appendChild(detail);
_saveBanner.appendChild(buttons);
}
// Updated in place: a failed save is announced again on every attempt. A
// choice being offered stays until it is made.
if(mode!=='confirm'&&_saveBanner.getAttribute('data-mode')==='confirm'&&_saveBanner.isConnected){return;}
_saveBanner.setAttribute('data-file',file);
_saveBanner.setAttribute('data-mode',mode||'retry');
_saveBanner.querySelector('.presemble-save-banner-error').textContent=error?' '+error+' Your edits are kept.':' Your edits are kept.';
var actions=_saveBanner.querySelector('.presemble-save-banner-actions');
actions.textContent='';
function button(label,onClick){
var b=document.createElement('button');
b.type='button';
b.textContent=label;
b.addEventListener('click',function(){onClick(b);});
actions.appendChild(b);
}
if(mode==='confirm'){
button('Overwrite the file',function(b){_retrySave(file,b,[file]);});
button('Cancel',function(){
// Nothing is saved: back to the failure and its Retry.
_saveBanner.removeAttribute('data-mode');
_showSaveBanner(file,_lastSaveError,'retry');
});
}else{
button('Retry',function(b){_retrySave(file,b,null);});
}
if(!_saveBanner.isConnected){document.body.appendChild(_saveBanner);}
}
function _hideSaveBanner(){if(_saveBanner){_saveBanner.remove();_saveBanner.removeAttribute('data-mode');}}
// Retry is an explicit save of the document (whatever auto-save says). If
// the file changed on disk meanwhile, the banner offers the choice instead;
// `overwrite` is the consent that names the file.
function _retrySave(file,btn,overwrite){
if(!file){return;}
btn.disabled=true;
var body={file:file};
if(overwrite){body.overwriteConflicts=overwrite;}
fetchJson('/_presemble/save',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)})
.then(function(data){
// A success hides the banner when its `saved` state arrives.
if(data.ok){return;}
if(data.needsConfirmation){
_showSaveBanner(file,file+' changed on disk while it has unsaved edits here. Overwrite it with your edits?','confirm');
return;
}
_toast('Save failed: '+(data.error||'unknown error'));
})
.catch(function(err){_toast('Save failed: '+err.message);})
.then(function(){btn.disabled=false;});
}
function _noteSaveState(m){
var file=_pageFile();
if(!file||m.file!==file){return;}
if(m.state==='saved'&&typeof m.at==='number'){sessionStorage.setItem('presemble-saved-at:'+file,String(m.at));}
_showSaveState(file,m.state);
if(m.state==='failed'){_lastSaveError=m.error||'';_showSaveBanner(file,_lastSaveError,'retry');}
else if(m.state==='saved'){_hideSaveBanner();}
}
// Where the page's document stands when the page opens: unsaved if listed,
// in conflict if listed so, otherwise saved.
function _fetchSaveState(){
var file=_pageFile();
if(!file){return;}
Promise.all([fetchJson('/_presemble/dirty-buffers'),fetchJson('/_presemble/conflicts')])
.then(function(lists){
var dirty=Array.isArray(lists[0])?lists[0]:[];
var conflicts=Array.isArray(lists[1])?lists[1]:[];
// A state announced meanwhile is newer: keep it.
if(_saveIndicator&&_saveIndicator.isConnected){return;}
_showSaveState(file,conflicts.indexOf(file)>=0?'conflict':(dirty.indexOf(file)>=0?'unsaved':'saved'));
})
.catch(function(err){console.warn('presemble: save state fetch failed',err);});
}
// The leave hint, when the page is hidden or goes away: a beacon (it
// outlives the page), or a keepalive request where there is none.
function _flushPage(){
if(_selfReload){return;}
var file=_pageFile();
if(!file){return;}
var body=JSON.stringify({file:file});
if(navigator.sendBeacon&&navigator.sendBeacon('/_presemble/flush',new Blob([body],{type:'text/plain'}))){return;}
fetch('/_presemble/flush',{method:'POST',headers:{'Content-Type':'application/json'},body:body,keepalive:true}).catch(function(){});
}
document.addEventListener('visibilitychange',function(){if(document.visibilityState==='hidden'){_flushPage();}});
window.addEventListener('pagehide',function(){_flushPage();});
if(document.readyState==='loading'){document.addEventListener('DOMContentLoaded',function(){_diagRender();_conflictRender();_fetchSaveState();});}
else{_diagRender();_conflictRender();_fetchSaveState();}
_fetchConflicts();
_wsConnect();
(function(){
var anchorRaw=sessionStorage.getItem('presemble-anchor');
if(!anchorRaw){return;}
sessionStorage.removeItem('presemble-anchor');
var anchor;
try{anchor=JSON.parse(anchorRaw);}catch(e){return;}
if(!anchor||typeof anchor!=='object'){return;}
function tryScroll(n){
var el=_findByStructuralAnchor(anchor);
if(el){
el.scrollIntoView({behavior:'smooth',block:'center'});
el.classList.add('presemble-changed');
setTimeout(function(){el.classList.remove('presemble-changed');},1500);
}else if(n>0){setTimeout(function(){tryScroll(n-1);},50);}
}
if(document.readyState==='loading'){document.addEventListener('DOMContentLoaded',function(){tryScroll(10);});}
else{tryScroll(10);}
})();
// Structural-anchor DOM lookup helpers — shared by scroll, post-reload, and suggest modes.
function _kindMatchesEl(kind,el){
var t=el.tagName;
if(kind==='paragraph')return t==='P';
if(kind==='heading')return /^H[1-6]$/.test(t);
if(kind==='code-block') return t === 'PRE';
if(kind==='blockquote')return t==='BLOCKQUOTE';
if(kind==='image')return t==='IMG';
if(kind==='table')return t==='TABLE';
if (kind === 'list') {
  if (t === 'UL' || t === 'OL') return true;
  if (t === 'DIV') {
    var md = el.getAttribute('data-presemble-md') || '';
    return /^\s*([-*+]|\d+\.)\s/.test(md);
  }
  return false;
}
return false;
}
function _findByStructuralAnchor(a){
var nk=a['node-kind'];
var heading=a['heading-text'];
var sel='[data-presemble-slot="'+a.slot+'"][data-presemble-file="'+a.file+'"]';
var sibs=document.querySelectorAll(sel);
if(sibs.length===0){sibs=document.querySelectorAll('[data-presemble-slot="'+a.slot+'"]');}
var arr=Array.prototype.slice.call(sibs);
if(heading){
var anchorIdx=-1;
for(var i=0;i<arr.length;i++){
var el=arr[i];
if(/^H[1-6]$/.test(el.tagName)&&_headingAnchorText(el)===heading){anchorIdx=i;break;}
}
if(anchorIdx<0)return null;
var count=0;
for(var j=anchorIdx+1;j<arr.length;j++){
if(_kindMatchesEl(nk,arr[j])){
if(count===a.offset)return arr[j];
count++;
}
}
return null;
}
var matching=arr.filter(function(el){return _kindMatchesEl(nk,el);});
return matching[a.offset]||null;
}
(function(){
// Mode is derived from the URL path first (schema URLs), then hash (ADR-042, T11).
// 'view' is the default; unknown or empty hashes also resolve to 'view'.
var _initialHashMode=parsePresembleHash(location.hash);
var mode=(location.pathname.indexOf('/_schema/')===0)
    ?'schema'
    :(_initialHashMode!=='unknown'?_initialHashMode:'view');
// Flag to suppress re-writing the hash while we are reacting to a hashchange.
var _handlingHashChange=false;
var _editorialSuggestCount=0;
var _dirtyCount=0;
var _dirtyPaths=[];
var _suggestionFilePaths=[];
function _contentPathToUrl(p){
var s=p;
if(s.indexOf('content/')===0){s=s.substring(8);}
if(s.endsWith('.md')){s=s.substring(0,s.length-3);}
if(s==='index'||s===''){return '/';}
if(s.endsWith('/index')){s=s.substring(0,s.length-6)+'/';}
return '/'+s;
}
function countSuggestions(){return document.querySelectorAll('.presemble-suggestion').length;}
var container=document.createElement('div');container.className='presemble-mascot';
// The page's state in words, beside the mascot: a label, never a control.
var status=document.createElement('span');
status.className='presemble-mascot-status';
status.setAttribute('role','status');
status.setAttribute('aria-live','polite');
status.title='No errors or warnings on this page. The site is published when no page has errors.';
status.hidden=true;
var icon=document.createElement('button');icon.className='presemble-mascot-icon';
var badge=document.createElement('span');badge.className='presemble-mascot-badge';
var menu=document.createElement('div');menu.className='presemble-mascot-menu';
var viewBtn=document.createElement('button');viewBtn.textContent='\u{1F441} View';
var editBtn=document.createElement('button');editBtn.textContent='\u{270F}\u{FE0F} Edit';
var suggestBtn=document.createElement('button');suggestBtn.textContent='\u{1F4AC} Suggest';suggestBtn.style.position='relative';
var suggestBadge=document.createElement('span');suggestBadge.className='presemble-suggest-badge';suggestBadge.style.display='none';suggestBtn.appendChild(suggestBadge);
var structureBtn=document.createElement('button');structureBtn.textContent='\u{1F4D0} Structure';
menu.appendChild(viewBtn);menu.appendChild(editBtn);menu.appendChild(suggestBtn);menu.appendChild(structureBtn);
container.appendChild(status);container.appendChild(icon);container.appendChild(badge);container.appendChild(menu);
document.body.appendChild(container);
function update(){
var totalBadge=_editorialSuggestCount+_dirtyCount;
if(totalBadge>0){badge.textContent=totalBadge;badge.style.display='block';}
else{badge.style.display='none';}
var diag=_pageDiagCounts();
var s=mascotState(mode,_editorialSuggestCount,_dirtyCount,diag.errors,diag.warnings);
icon.textContent=s.face;icon.title=s.title;icon.setAttribute('data-status',s.kind);
status.textContent=s.label;status.hidden=!s.label;
viewBtn.className=mode==='view'?'active':'';
editBtn.className=mode==='edit'?'active':'';
suggestBtn.className=mode==='suggest'?'active':'';
structureBtn.className=mode==='schema'?'active':'';
document.body.classList.toggle('presemble-edit-mode',mode==='edit');
document.body.classList.toggle('presemble-suggest-mode',mode==='suggest');
}
update();
if(document.readyState==='loading'){document.addEventListener('DOMContentLoaded',update);}
icon.onclick=function(e){e.stopPropagation();menu.classList.toggle('open');};
document.addEventListener('click',function(){menu.classList.remove('open');});
menu.onclick=function(e){e.stopPropagation();};
// Leaving edit mode: setMode has already closed the editor or kept it (with
// its text, in the new mode's intent), so the sweep skips everything it owns.
function cleanupEditing(){
var kept=_activeEditor;
function mine(node){return !!kept&&kept.owns(node);}
document.querySelectorAll('.presemble-editing').forEach(function(el){if(mine(el)){return;}el.contentEditable='false';el.classList.remove('presemble-editing');});
document.querySelectorAll('.presemble-edit-toolbar,.presemble-edit-error,.presemble-link-picker').forEach(function(el){if(mine(el)){return;}el.remove();});
_onEditorClosed();
}
var _editToolbar=null;
// Save every unsaved document. If some changed on disk while they have unsaved
// edits here, the server writes nothing and lists them: warn first, then
// Overwrite (post again with exactly the files shown) or Cancel (do nothing).
function _saveAll(overwrite){
var opts={method:'POST'};
if(overwrite){
opts.headers={'Content-Type':'application/json'};
opts.body=JSON.stringify({overwriteConflicts:overwrite});
}
return fetchJson('/_presemble/save-all',opts).then(function(data){
if(data.ok){_fetchDirtyCount();_fetchConflicts();return;}
if(data.needsConfirmation){
var files=data.conflicts||[];
if(confirm('These files changed on disk while they have unsaved edits here: '+files.join(', ')+'. Overwrite them with your edits? (Cancel saves nothing.)')){
return _saveAll(files);
}
return;
}
alert(data.error||'Save failed');
});
}
function _editEnter(){
if(_editToolbar){return;}
_editToolbar=document.createElement('div');
_editToolbar.className='presemble-edit-toolbar-bar';
_editToolbar.innerHTML='<button class="presemble-edit-save" style="display:none" title="Save all changes">\u{1F4BE} Save</button>'
+'<button class="presemble-edit-new" title="New content">\u{2795}</button>';
document.body.appendChild(_editToolbar);
_editToolbar.querySelector('.presemble-edit-new').onclick=function(){_openCreateDialog();};
_editToolbar.querySelector('.presemble-edit-save').onclick=function(){
_saveAll(null).catch(function(err){_toast('Save failed: '+err.message);});
};
_foldSetup();
_fetchSuggestionCount();
}
function _editMarkSuggestions(data){
document.querySelectorAll('.presemble-has-suggestions').forEach(function(el){
el.classList.remove('presemble-has-suggestions');
});
if(!data||!data.length){return;}
data.forEach(function(sug){
var el=_suggestFindTarget(sug);
if(el){el.classList.add('presemble-has-suggestions');}
});
}
function _editCleanup(){
_foldTeardown();
if(_editToolbar){_editToolbar.remove();_editToolbar=null;}
document.querySelectorAll('.presemble-has-suggestions').forEach(function(el){
el.classList.remove('presemble-has-suggestions');
});
}
var _sectionMap=null;
var _foldSummaries={};
function _foldBuildSectionMap(){
var allBody=Array.prototype.slice.call(document.querySelectorAll('[data-presemble-slot="body"]'));
var map=new Map();
for(var i=0;i<allBody.length;i++){
var el=allBody[i];
if(!/^H[1-6]$/.test(el.tagName)){continue;}
var level=parseInt(el.tagName.charAt(1),10);
var section=[];
for(var j=i+1;j<allBody.length;j++){
var next=allBody[j];
if(/^H[1-6]$/.test(next.tagName)&&parseInt(next.tagName.charAt(1),10)<=level){break;}
section.push(next);
}
if(section.length>0){map.set(i,{level:level,elements:section,heading:el});}
}
return map;
}
function _foldIndexOf(headingEl){
var allBody=document.querySelectorAll('[data-presemble-slot="body"]');
return Array.prototype.indexOf.call(allBody,headingEl);
}
function _foldSetup(){
_sectionMap=_foldBuildSectionMap();
_sectionMap.forEach(function(info,headingId){
var headingEl=info.heading;
var btn=document.createElement('button');
btn.className='presemble-fold-toggle';
btn.type='button';
// Glyph is drawn in CSS so it never leaks into the heading's innerText
// (which structural anchors match on).
btn.setAttribute('aria-label','Fold section');
btn.onclick=function(e){e.stopPropagation();_foldToggle(headingEl);};
headingEl.insertBefore(btn,headingEl.firstChild);
});
if(_editToolbar){
var foldAllBtn=document.createElement('button');
foldAllBtn.className='presemble-fold-all';
foldAllBtn.textContent='Fold All';
foldAllBtn.onclick=function(){_foldAll();};
var unfoldAllBtn=document.createElement('button');
unfoldAllBtn.className='presemble-unfold-all';
unfoldAllBtn.textContent='Unfold All';
unfoldAllBtn.onclick=function(){_unfoldAll();};
_editToolbar.appendChild(foldAllBtn);
_editToolbar.appendChild(unfoldAllBtn);
}
}
function _foldTeardown(){
if(!_sectionMap){return;}
document.querySelectorAll('.presemble-fold-toggle').forEach(function(btn){btn.remove();});
document.querySelectorAll('.presemble-fold-summary').forEach(function(el){el.remove();});
document.querySelectorAll('.presemble-folded-content').forEach(function(el){el.classList.remove('presemble-folded-content');});
document.querySelectorAll('.presemble-heading-folded').forEach(function(el){el.classList.remove('presemble-heading-folded');});
if(_editToolbar){
var faBtns=_editToolbar.querySelectorAll('.presemble-fold-all,.presemble-unfold-all');
faBtns.forEach(function(b){b.remove();});
}
_sectionMap=null;
_foldSummaries={};
}
function _foldToggle(headingEl){
if(!_sectionMap){return;}
var headingIdx=_foldIndexOf(headingEl);
var info=_sectionMap.get(headingIdx);
if(!info){return;}
var isFolded=headingEl.classList.contains('presemble-heading-folded');
if(isFolded){
info.elements.forEach(function(el){el.classList.remove('presemble-folded-content');});
var summary=_foldSummaries[headingIdx];
if(summary){summary.remove();delete _foldSummaries[headingIdx];}
headingEl.classList.remove('presemble-heading-folded');
// Re-apply folds for nested headings that were independently folded
info.elements.forEach(function(el){
if(/^H[1-6]$/.test(el.tagName)&&el.classList.contains('presemble-heading-folded')){
var nestedIdx=_foldIndexOf(el);
if(_sectionMap.has(nestedIdx)){
var nestedInfo=_sectionMap.get(nestedIdx);
if(nestedInfo){nestedInfo.elements.forEach(function(ne){ne.classList.add('presemble-folded-content');});}
}
}
});
}else{
var hasActive=info.elements.some(function(el){
return el.classList.contains('presemble-editing')||
(el.parentNode&&el.parentNode.querySelector('.presemble-body-editor'));
});
if(hasActive){return;}
info.elements.forEach(function(el){el.classList.add('presemble-folded-content');});
var foldSummary=document.createElement('div');
foldSummary.className='presemble-fold-summary';
foldSummary.textContent='\u2026 '+info.elements.length+' element'+(info.elements.length===1?'':'s')+' hidden';
foldSummary.onclick=function(){_foldToggle(headingEl);};
headingEl.after(foldSummary);
_foldSummaries[headingIdx]=foldSummary;
headingEl.classList.add('presemble-heading-folded');
}
}
function _foldAll(){
if(!_sectionMap){return;}
var headings=[];
_sectionMap.forEach(function(info,id){headings.push(info.heading);});
headings.reverse().forEach(function(h){if(!h.classList.contains('presemble-heading-folded')){_foldToggle(h);}});
}
function _unfoldAll(){
if(!_sectionMap){return;}
_sectionMap.forEach(function(info,id){if(info.heading.classList.contains('presemble-heading-folded')){_foldToggle(info.heading);}});
}
function _openCreateDialog(){
fetchJson('/_presemble/schemas').then(function(schemas){
if(!Array.isArray(schemas)||schemas.length===0){alert('No content schemas found.');return;}
var overlay=document.createElement('div');
overlay.style.cssText='position:fixed;inset:0;background:rgba(0,0,0,0.4);z-index:10001;display:flex;align-items:center;justify-content:center;';
var dialog=document.createElement('div');
dialog.style.cssText='background:#fff;border-radius:0.5rem;padding:1.5rem;min-width:20rem;box-shadow:0 4px 16px rgba(0,0,0,0.2);font-family:system-ui,sans-serif;';
var title=document.createElement('h3');title.textContent='New content';title.style.cssText='margin:0 0 1rem;font-size:1.1rem;';
dialog.appendChild(title);
var typeLabel=document.createElement('label');typeLabel.textContent='Type:';typeLabel.style.cssText='display:block;font-size:0.85rem;margin-bottom:0.25rem;';
var typeSelect=document.createElement('select');typeSelect.style.cssText='display:block;width:100%;padding:0.4rem;border:1px solid #ccc;border-radius:0.3rem;margin-bottom:0.75rem;font-size:1rem;';
schemas.forEach(function(s){var o=document.createElement('option');o.value=s;o.textContent=s;typeSelect.appendChild(o);});
var slugLabel=document.createElement('label');slugLabel.textContent='Slug:';slugLabel.style.cssText='display:block;font-size:0.85rem;margin-bottom:0.25rem;';
var slugInput=document.createElement('input');slugInput.type='text';
slugInput.value='';
slugInput.style.cssText='display:block;width:100%;box-sizing:border-box;padding:0.4rem;border:1px solid #ccc;border-radius:0.3rem;margin-bottom:0.75rem;font-size:1rem;';
typeSelect.onchange=function(){};
var errDiv=document.createElement('div');errDiv.style.cssText='color:#c00;font-size:0.85rem;margin-bottom:0.5rem;display:none;';
var btns=document.createElement('div');btns.style.cssText='display:flex;gap:0.5rem;justify-content:flex-end;';
var cancelBtn=document.createElement('button');cancelBtn.textContent='Cancel';cancelBtn.style.cssText='padding:0.4rem 0.9rem;border:1px solid #ccc;border-radius:0.3rem;background:#fff;cursor:pointer;';
var submitBtn=document.createElement('button');submitBtn.textContent='Create';submitBtn.style.cssText='padding:0.4rem 0.9rem;border:none;border-radius:0.3rem;background:#5d8a6e;color:#fff;cursor:pointer;';
btns.appendChild(cancelBtn);btns.appendChild(submitBtn);
dialog.appendChild(typeLabel);dialog.appendChild(typeSelect);
dialog.appendChild(slugLabel);dialog.appendChild(slugInput);
dialog.appendChild(errDiv);dialog.appendChild(btns);
overlay.appendChild(dialog);document.body.appendChild(overlay);
slugInput.focus();slugInput.select();
cancelBtn.onclick=function(){overlay.remove();};
overlay.onclick=function(e){if(e.target===overlay){overlay.remove();}};
submitBtn.onclick=function(){
var stem=typeSelect.value;var slug=slugInput.value.trim();
if(!slug){errDiv.textContent='Slug is required.';errDiv.style.display='block';return;}
submitBtn.disabled=true;
fetchJson('/_presemble/create-content',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({stem:stem,slug:slug})})
.then(function(data){
if(data.ok){overlay.remove();}
else{errDiv.textContent=data.error||'Create failed';errDiv.style.display='block';submitBtn.disabled=false;}
})
.catch(function(err){errDiv.textContent='Create failed: '+err.message;errDiv.style.display='block';submitBtn.disabled=false;});
};
slugInput.addEventListener('keydown',function(e){if(e.key==='Enter'){submitBtn.click();}if(e.key==='Escape'){overlay.remove();}});
}).catch(function(err){_toast('Could not load content types: '+err.message);});
}
var _suggestions=[];var _suggestIdx=0;var _suggestPreviewState=null;var _suggestActiveEl=null;
var _suggestToolbar=null;
function _suggestCleanup(){
document.querySelectorAll('[data-presemble-original-html]').forEach(function(el){
// An open editor on a previewed element keeps the user's text; when it
// closes outside suggest mode it resets the preview itself.
if(_activeEditor&&_activeEditor.owns(el)){return;}
_resetPreview(el);
});
document.querySelectorAll('.presemble-suggest-indicator,.presemble-suggest-active,.presemble-suggest-stale').forEach(function(el){
el.classList.remove('presemble-suggest-indicator','presemble-suggest-active','presemble-suggest-preview-active','presemble-suggest-stale');
});
if(_suggestToolbar){_suggestToolbar.remove();_suggestToolbar=null;}
_suggestPreviewState=null;_suggestActiveEl=null;
_suggestions=[];_suggestIdx=0;
}
function _stripMd(s){return s.replace(/`/g,'').replace(/\*\*/g,'').replace(/\*/g,'').replace(/_/g,'');}
function _suggestFindTarget(sug){
// NED suggestions: use anchor field when present (source === 'ned')
if(sug._source==='ned'&&sug.anchor){
var a=sug.anchor;
if(a.kind==='structural'){
return _findByStructuralAnchor(a);
}
if(a.kind==='slot-nth'){
var sibs=document.querySelectorAll('[data-presemble-slot="'+a.slot+'"][data-presemble-file="'+a.file+'"]');
if(sibs.length===0){sibs=document.querySelectorAll('[data-presemble-slot="'+a.slot+'"]');}
return sibs[a.index]||sibs[0]||null;
}
if(a.kind==='slot'){
return document.querySelector('[data-presemble-slot="'+a.slot+'"][data-presemble-file="'+a.file+'"]')||document.querySelector('[data-presemble-slot="'+a.slot+'"]');
}
// 'doc' kind — no specific element
console.warn('NED suggestion '+sug.id+' has doc-level anchor; no DOM target');
return null;
}
return null;
}
function _suggestRenderToolbar(){
if(_suggestions.length===0){if(_suggestToolbar){_suggestToolbar.remove();_suggestToolbar=null;}return;}
if(!_suggestToolbar){
_suggestToolbar=document.createElement('div');
_suggestToolbar.className='presemble-suggest-toolbar';
_suggestToolbar.innerHTML='<button class="presemble-suggest-prev" title="Previous">&#9664;</button>'
+'<span class="presemble-suggest-info"><span class="presemble-suggest-author"></span>: <span class="presemble-suggest-reason"></span><span class="presemble-suggest-counter"></span></span>'
+'<button class="presemble-suggest-accept" title="Accept">&#10003;</button>'
+'<button class="presemble-suggest-preview" title="Preview">\u{1F441}</button>'
+'<button class="presemble-suggest-reject" title="Reject">&#10007;</button>'
+'<button class="presemble-suggest-next" title="Next">&#9654;</button>';
document.body.appendChild(_suggestToolbar);
_suggestToolbar.querySelector('.presemble-suggest-prev').onclick=function(){_suggestNavigate(-1);};
_suggestToolbar.querySelector('.presemble-suggest-next').onclick=function(){_suggestNavigate(1);};
_suggestToolbar.querySelector('.presemble-suggest-accept').onclick=function(){_suggestAccept();};
_suggestToolbar.querySelector('.presemble-suggest-reject').onclick=function(){_suggestReject();};
_suggestToolbar.querySelector('.presemble-suggest-preview').onclick=function(){_suggestTogglePreview();};
}
var sug=_suggestions[_suggestIdx];
_suggestToolbar.querySelector('.presemble-suggest-author').textContent=sug.author||'';
var reasonText=sug.reason||'';
// NED stale: show stale reason in toolbar
var isStale=sug._source==='ned'&&sug.status&&typeof sug.status==='object'&&sug.status.Stale;
if(isStale){reasonText='[STALE: '+(sug.status.Stale.reason||'')+']: '+reasonText;}
_suggestToolbar.querySelector('.presemble-suggest-reason').textContent=reasonText;
var acceptBtn=_suggestToolbar.querySelector('.presemble-suggest-accept');
acceptBtn.disabled=!!isStale;
acceptBtn.title=isStale?'Stale suggestion — cannot be accepted':'Accept';
var targetText='';
if(sug._source==='ned'&&sug.anchor){
if(sug.anchor.kind==='slot'){targetText=sug.anchor.slot;}
else if(sug.anchor.kind==='slot-nth'){targetText=sug.anchor.slot+'['+sug.anchor.index+']';}
else if(sug.anchor.kind==='structural'){
var nk=sug.anchor['node-kind'];
var off=sug.anchor.offset;
if(sug.anchor['heading-text']){targetText='"'+sug.anchor['heading-text']+'" / '+nk+'['+off+']';}
else{targetText=sug.anchor.slot+' / '+nk+'['+off+']';}
}
}
_suggestToolbar.querySelector('.presemble-suggest-counter').textContent='('+(_suggestIdx+1)+'/'+_suggestions.length+') '+targetText;
}
function _suggestHighlight(){
document.querySelectorAll('.presemble-suggest-active').forEach(function(el){el.classList.remove('presemble-suggest-active','presemble-suggest-preview-active');});
// An open editor's element keeps the user's text: it is neither reset nor
// drawn as a preview (it can be one after an edit -> suggest switch).
function owned(node){return !!_activeEditor&&_activeEditor.owns(node);}
document.querySelectorAll('[data-presemble-original-html]').forEach(function(el){
if(owned(el)){return;}
_resetPreview(el);
});
if(_suggestions.length===0){return;}
var sug=_suggestions[_suggestIdx];
var el=_suggestFindTarget(sug);
_suggestActiveEl=el;
if(el&&owned(el)){el.classList.add('presemble-suggest-active');}
else if(el){
el.classList.add('presemble-suggest-active');
// NED stale suggestions get a stale class; T6 adds CSS for it
var isStale=sug._source==='ned'&&sug.status&&typeof sug.status==='object'&&sug.status.Stale;
if(isStale){
el.classList.add('presemble-suggest-stale');
var staleReason=sug.status.Stale.reason||'';
if(staleReason){el.title=staleReason;}
}else{
el.classList.remove('presemble-suggest-stale');
}
el.scrollIntoView({behavior:'smooth',block:'center'});
el.setAttribute('data-presemble-original-html',el.innerHTML);
// NED suggestions: a suggestion made through a block edit carries a preview
// (the base and proposed markdown): show the change from base to proposed.
// Otherwise render the diff from the SearchReplace or SetText mutation.
// Built from DOM nodes with textContent only — suggestion text is untrusted.
if(sug._source==='ned'&&sug.preview&&sug.preview.base&&sug.preview.proposed){
_renderPreviewDiff(el,String(sug.preview.base.markdown||''),String(sug.preview.proposed.markdown||''));
}else if(sug._source==='ned'&&sug.mutation&&sug.mutation.SearchReplace){
var sr=sug.mutation.SearchReplace;
_renderSearchReplaceDiff(el,String(sr.search||''),String(sr.replace||''));
}else if(sug._source==='ned'&&sug.mutation&&sug.mutation.SetText){
var oldText=el.textContent;
el.textContent='';
el.appendChild(_diffNode('del',oldText));
el.appendChild(document.createTextNode(' '));
el.appendChild(_diffNode('ins',String(sug.mutation.SetText)));
}
}
_suggestRenderToolbar();
}
function _diffNode(tag,text){
var n=document.createElement(tag);
n.className=tag==='del'?'presemble-diff-del':'presemble-diff-ins';
n.textContent=text;
return n;
}
// The change a block-edit suggestion proposes, word by word (wordDiff): what
// stays is plain text, what goes is struck, what comes is inserted, all in one
// container that keeps line breaks (pre-wrap), so a structural change such as
// one paragraph into two still reads as such.
function _renderPreviewDiff(el,base,proposed){
el.textContent='';
var box=document.createElement('span');
box.style.display='block';
box.style.whiteSpace='pre-wrap';
wordDiff(base,proposed).forEach(function(part){
box.appendChild(part.op==='eq'?document.createTextNode(part.text):_diffNode(part.op,part.text));
});
el.appendChild(box);
}
// Prefer an in-place split of the single text node containing `search`
// (preserves surrounding markup); otherwise fall back to a flat text rendering.
function _renderSearchReplaceDiff(el,search,replace){
var walker=document.createTreeWalker(el,NodeFilter.SHOW_TEXT);
for(var node=walker.nextNode();node;node=walker.nextNode()){
var i=node.nodeValue.indexOf(search);
if(i!==-1){
var match=node.splitText(i);
var rest=match.splitText(search.length);
var parent=match.parentNode;
parent.insertBefore(_diffNode('del',search),match);
parent.insertBefore(_diffNode('ins',replace),rest);
parent.removeChild(match);
return;
}
}
var txt=el.textContent;
var tidx=txt.indexOf(search);
if(tidx===-1){return;}
el.textContent='';
el.appendChild(document.createTextNode(txt.slice(0,tidx)));
el.appendChild(_diffNode('del',search));
el.appendChild(_diffNode('ins',replace));
el.appendChild(document.createTextNode(txt.slice(tidx+search.length)));
}
function _suggestNavigate(dir){
if(_suggestions.length===0){return;}
if(_suggestPreviewState){_suggestTogglePreview();}
_suggestIdx=(_suggestIdx+dir+_suggestions.length)%_suggestions.length;
_suggestHighlight();
}
function _suggestTogglePreview(){
if(_suggestions.length===0){return;}
var sug=_suggestions[_suggestIdx];
var el=_suggestActiveEl||_suggestFindTarget(sug);
if(!el){return;}
var previewBtn=_suggestToolbar?_suggestToolbar.querySelector('.presemble-suggest-preview'):null;
if(_suggestPreviewState){
el.innerHTML=_suggestPreviewState;
_suggestPreviewState=null;
el.classList.remove('presemble-suggest-preview-active');
el.classList.add('presemble-suggest-active');
if(previewBtn){previewBtn.classList.remove('active');}
}else{
_suggestPreviewState=el.innerHTML;
el.classList.add('presemble-suggest-preview-active');
el.classList.remove('presemble-suggest-active');
if(previewBtn){previewBtn.classList.add('active');}
}
}
function _suggestAccept(){
if(_suggestions.length===0){return;}
var sug=_suggestions[_suggestIdx];
if(sug._source==='ned'&&sug.status&&typeof sug.status==='object'&&sug.status.Stale){return;}
if(_suggestPreviewState){_suggestTogglePreview();}
fetchJson('/_presemble/ned-suggestions/accept',{method:'POST',headers:{'Content-Type':'application/json'},
body:JSON.stringify({id:sug.id})
}).then(function(data){
if(data&&!data.ok){alert(data.error||'Accept failed');}
// Refresh so suggestions that just became stale show their [STALE: …] reason.
else{_suggestEnter(sug.id);}
if(window._fetchSuggestionCount){window._fetchSuggestionCount();}
}).catch(function(err){_toast('Accept failed: '+err.message);});
}
function _suggestReject(){
if(_suggestions.length===0){return;}
var sug=_suggestions[_suggestIdx];
if(_suggestPreviewState){_suggestTogglePreview();}
var el=_suggestFindTarget(sug);
if(el){el.classList.remove('presemble-suggest-indicator','presemble-suggest-active','presemble-suggest-stale');}
fetchJson('/_presemble/ned-suggestions/reject',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({id:sug.id})})
// On failure, re-fetch the list so the optimistic removal below is undone.
.then(function(data){if(!data.ok){alert(data.error||'Reject failed');_suggestEnter();}})
.catch(function(err){_toast('Reject failed: '+err.message);_suggestEnter();});
_suggestions.splice(_suggestIdx,1);
if(_suggestIdx>=_suggestions.length&&_suggestions.length>0){_suggestIdx=_suggestions.length-1;}
_suggestHighlight();
}
// The URL listing the suggestions of every document this page shows (a
// listing shows fields of other documents); null when it shows none.
function _pageSuggestionUrl(){
var values=Array.prototype.map.call(document.querySelectorAll('[data-presemble-file]'),function(el){return el.getAttribute('data-presemble-file')||'';});
return suggestionListUrl(pageFiles(values));
}
function _fetchSuggestionCount(){
var url=_pageSuggestionUrl();
if(!url){
_editorialSuggestCount=0;
suggestBadge.style.display='none';
update();
return;
}
fetchJson(url).catch(function(err){console.warn('presemble: suggestion count fetch failed',err);return[];})
.then(function(ned){
var nedFiltered=(Array.isArray(ned)?ned:[]).filter(function(s){return s.status==='Pending'||(s.status&&typeof s.status==='object'&&s.status.Stale);});
var nedTagged=nedFiltered.map(function(s){return Object.assign({},s,{_source:'ned'});});
var cnt=nedTagged.length;
_editorialSuggestCount=cnt;
if(cnt>0){suggestBadge.textContent=cnt;suggestBadge.style.display='flex';}else{suggestBadge.style.display='none';}
update();
if(mode==='edit'){_editMarkSuggestions(nedTagged);}
})
.catch(function(err){console.warn('presemble: suggestion count update failed',err);});
}
function _fetchDirtyCount(){
fetchJson('/_presemble/dirty-buffers')
.then(function(paths){
if(!Array.isArray(paths)){return;}
_dirtyPaths=paths;
_dirtyCount=_dirtyPaths.length;
update();
if(_editToolbar){
var saveBtn=_editToolbar.querySelector('.presemble-edit-save');
if(saveBtn){
saveBtn.style.display=_dirtyCount>0?'':'none';
saveBtn.textContent='\u{1F4BE} Save ('+_dirtyCount+')';
}
}
_renderBufferLists();
})
.catch(function(err){console.warn('presemble: dirty-buffers poll failed',err);});
}
function _fetchSuggestionFiles(){
fetchJson('/_presemble/ned-suggestion-files').catch(function(err){console.warn('presemble: suggestion-files poll failed',err);return[];})
.then(function(ned){
var paths=Array.isArray(ned)?ned:[];
paths.sort();
_suggestionFilePaths=paths;
_renderBufferLists();
})
.catch(function(err){console.warn('presemble: suggestion-files render failed',err);});
}
function _renderBufferLists(){
var menu=document.querySelector('.presemble-mascot-menu');
if(!menu){return;}
var container=menu.querySelector('.presemble-buffer-lists');
if(!container){
container=document.createElement('div');
container.className='presemble-buffer-lists';
menu.appendChild(container);
}
// Paths come from the server and may contain markup characters: build DOM
// nodes with textContent/href rather than HTML strings.
container.textContent='';
function addSection(title,paths){
if(paths.length===0){return;}
var t=document.createElement('div');
t.className='presemble-buffer-section-title';
t.textContent=title+' ('+paths.length+')';
container.appendChild(t);
paths.forEach(function(p){
var url=_contentPathToUrl(String(p));
var a=document.createElement('a');
a.className='presemble-buffer-link';
if(window.location.pathname===url||window.location.pathname===url+'/'){a.classList.add('current');}
a.href=url;
a.textContent=url;
container.appendChild(a);
});
}
addSection('Unsaved',_dirtyPaths);
addSection('Suggestions',_suggestionFilePaths);
container.style.display=container.childNodes.length===0?'none':'block';
}
setInterval(_fetchDirtyCount,2000);
setInterval(_fetchSuggestionFiles,5000);
_fetchSuggestionFiles();
function _suggestEnter(targetId){
var url=_pageSuggestionUrl();
if(!url){return;}
fetchJson(url).catch(function(err){_toast('Could not load suggestions: '+err.message);return[];})
.then(function(ned){
var nedFiltered=(Array.isArray(ned)?ned:[]).filter(function(s){return s.status==='Pending'||(s.status&&typeof s.status==='object'&&s.status.Stale);});
var merged=nedFiltered.map(function(s){return Object.assign({},s,{_source:'ned'});});
_suggestions=merged;
if(targetId){
var foundIdx=merged.findIndex(function(s){return s.id===targetId;});
_suggestIdx=foundIdx>=0?foundIdx:0;
}else{
_suggestIdx=0;
}
var cnt=merged.length;
_editorialSuggestCount=cnt;
if(cnt>0){suggestBadge.textContent=cnt;suggestBadge.style.display='flex';}else{suggestBadge.style.display='none';}
merged.forEach(function(sug){
var el=_suggestFindTarget(sug);
if(el){el.classList.add('presemble-suggest-indicator');}
});
_suggestHighlight();
})
.catch(function(err){console.warn('presemble: suggest mode render failed',err);_toast('Could not show suggestions: '+err.message);});
}
// The one place the mode changes: the mode buttons and the hash route both
// come here. An open editor closes (unchanged), stays with the new mode's
// save intent (changed, edit <-> suggest) or refuses the switch (changed, to
// view or structure; or busy saving) — see modeSwitchDecision.
var _REFUSAL_MESSAGES={'save-or-cancel':'Save or cancel your edit first','saving':'Saving…'};
function _refreshModeControls(){
var state=_editorState();
var available=availableModes(mode,state);
var why=_REFUSAL_MESSAGES[state==='busy'?'saving':'save-or-cancel'];
[['view',viewBtn],['edit',editBtn],['suggest',suggestBtn],['schema',structureBtn]].forEach(function(pair){
var ok=available.indexOf(pair[0])>=0;
pair[1].disabled=!ok;
pair[1].title=ok?'':why;
});
}
// Put the hash back to the current mode without a hashchange (a refused
// switch that came from the address bar).
function _restoreModeHash(){
var url=location.pathname+location.search;
if(mode==='edit'||mode==='suggest'){url+='#_'+mode;}
history.replaceState(null,'',url);
}
function setMode(m){
var decision=modeSwitchDecision(mode,m,_editorState());
if(decision.kind==='none'){menu.classList.remove('open');return;}
if(decision.kind==='refuse'){
if(_handlingHashChange){_restoreModeHash();}
_toast(_REFUSAL_MESSAGES[decision.reason]);
_activeEditor.focus();
return;
}
if(decision.kind==='close-and-switch'){_activeEditor.close();}
if(decision.kind==='switch-keeping-editor'){_activeEditor.setIntent(decision.intent);}
if(m!=='edit'){cleanupEditing();_editCleanup();}
if(m!=='suggest'){_suggestCleanup();}
mode=m;
menu.classList.remove('open');
update();
if(m==='edit'){_editEnter();}
if(m==='suggest'){_suggestEnter();}else{_fetchSuggestionCount();}
_refreshModeControls();
// Update the URL hash to reflect the new mode, unless we are already
// responding to a hashchange (which would create an infinite loop).
if(!_handlingHashChange){setPresembleMode(m);}
}
// --- Phase D Wave D: schema mode handlers (canonical URL navigation) ---
function _enterSchemaMode(){
var page=location.pathname;
fetch('/_presemble/schema-for?page='+encodeURIComponent(page))
.then(function(r){
if(r.status===404){return null;}
if(!r.ok){throw new Error('schema-for failed: '+r.status);}
return r.json();
})
.then(function(data){
if(!data||!data.url){
console.warn('no schema available for',page);
return;
}
location.href=data.url;
})
.catch(function(err){
console.error('schema-for error:',err);
});
}
function _appendModeHash(url,targetMode){
if(!targetMode||targetMode==='view')return url;
if(targetMode==='edit'||targetMode==='suggest')return url+'#_'+targetMode;
return url;
}
function _leaveSchemaMode(targetMode){
var schemaUrl=location.pathname;
fetch('/_presemble/page-for?schema='+encodeURIComponent(schemaUrl))
.then(function(r){
if(r.status===404){location.href=_appendModeHash('/',targetMode);return null;}
if(!r.ok){throw new Error('page-for failed: '+r.status);}
return r.json();
})
.then(function(data){
if(data&&data.url){location.href=_appendModeHash(data.url,targetMode);}
else if(data!==null&&data!==undefined){location.href=_appendModeHash('/',targetMode);}
})
.catch(function(err){
console.error('page-for error:',err);
location.href=_appendModeHash('/',targetMode);
});
}
function _applyHashMode(){
_handlingHashChange=true;
var m=parsePresembleHash(location.hash);
// Schema mode is a URL-path concern (/_schema/...), not a hash concern.
// Stale #_schema hashes are treated as unknown and ignored.
if(m==='unknown'){
_handlingHashChange=false;
return;
}
setMode(m);
_handlingHashChange=false;
}
// Set the presemble mode.
// 'schema' triggers real navigation to the canonical /_schema/... URL via
// _enterSchemaMode() — no hash is written.
// When currently on a /_schema/... URL and switching to a non-schema mode,
// _leaveSchemaMode() navigates back to the corresponding content page.
// 'view' removes the hash (no page reload); other modes set #_<mode>.
// Also canonicalizes the pathname by stripping /index.html or /index.htm.
// NOTE: defined here (inside the mode-management IIFE) so it can close over
// _enterSchemaMode, _leaveSchemaMode and _applyHashMode.
function setPresembleMode(mode) {
  if (mode === 'schema') {
    _enterSchemaMode();
    return;
  }
  // If we are currently on a schema URL, leaving any non-schema mode requires
  // navigating back to the content page via the page-for API.
  if (location.pathname.indexOf('/_schema/') === 0) {
    _leaveSchemaMode(mode);
    return;
  }
  var p = location.pathname;
  if (p.endsWith('/index.html')) {
    p = p.slice(0, -('index.html'.length)); // keeps trailing /
  } else if (p.endsWith('/index.htm')) {
    p = p.slice(0, -('index.htm'.length));
  }
  var pathChanged = p !== location.pathname;

  if (mode === 'view') {
    history.replaceState(null, '', p + location.search);
  } else if (pathChanged) {
    // replaceState doesn't fire hashchange, so update URL then dispatch manually.
    history.replaceState(null, '', p + location.search + '#_' + mode);
    _applyHashMode();
  } else {
    location.hash = '_' + mode;
  }
}
window.addEventListener('hashchange',_applyHashMode);
// Schema-mode link intercept: when in schema mode, content link clicks are
// resolved to their canonical schema URL via the schema-for API.
document.addEventListener('click',function(e){
if(mode!=='schema'){return;}
var a=e.target.closest&&e.target.closest('a');
if(!a){return;}
var href=a.getAttribute('href');
if(!href){return;}
// Skip already-schema URLs
if(href.indexOf('/_schema/')===0){return;}
// Skip fragments, javascript:, mailto:
if(href.startsWith('#')||href.startsWith('javascript:')||href.startsWith('mailto:')){return;}
// Skip cross-origin external URLs
if(/^https?:\/\//.test(href)){
try{var u=new URL(href);if(u.origin!==location.origin){return;}}
catch(_){return;}
}
e.preventDefault();
fetch('/_presemble/schema-for?page='+encodeURIComponent(href))
.then(function(r){
if(r.status===404){location.href=href;return null;}
if(!r.ok){throw new Error('schema-for failed: '+r.status);}
return r.json();
})
.then(function(data){
if(data&&data.url){location.href=data.url;}else if(data!==null){location.href=href;}
})
.catch(function(){
location.href=href;
});
},true);
// --- End Phase D Wave D ---
if(mode==='edit'){_editEnter();}
if(mode==='suggest'){_suggestEnter();}else{_fetchSuggestionCount();}
viewBtn.onclick=function(){setMode('view');};
editBtn.onclick=function(){setMode('edit');};
suggestBtn.onclick=function(){setMode('suggest');};
structureBtn.onclick=function(){setMode('schema');};
window._fetchDirtyCount=_fetchDirtyCount;
window._fetchSuggestionCount=_fetchSuggestionCount;
window._suggestEnter=_suggestEnter;
window._fetchSuggestionFiles=_fetchSuggestionFiles;
window._foldToggle=function(el){_foldToggle(el);};
window._refreshModeControls=_refreshModeControls;
_refreshModeControls();
})();
// BEGIN pure-text-helpers
// Pure functions, no DOM: tools/text-vectors.ts evaluates this region and
// checks it against resources/serve/text-vectors.json, as the Rust side does.

// Whitespace-normalise anchor text the way the server does
// (content::inline::normalize_plain_text, ned normalize_ws): split on Unicode
// White_Space and join with one space. Not `\s`/trim(): they also strip
// U+FEFF, which is not White_Space.
function normalizeAnchorText(s) {
return String(s).split(/\p{White_Space}+/u).filter(Boolean).join(' ');
}

// What a suggestion changes, word by word: [{op:'eq'|'del'|'ins', text}].
// Tokens are runs of Unicode White_Space and runs of everything else (the
// boundary of normalizeAnchorText), so a line break is a token too: one
// paragraph into two shows as a change. The tokens are diffed by their longest
// common subsequence; adjacent entries of one op are merged. Past 250,000
// token pairs the whole base goes out and the whole proposal in.
function wordDiff(base, proposed) {
var tokens = function (s) {
  return String(s).split(/(\p{White_Space}+)/u).filter(function (t) { return t !== ''; });
};
var a = tokens(base);
var b = tokens(proposed);
var out = [];
var push = function (op, text) {
  var last = out[out.length - 1];
  if (last && last.op === op) last.text += text;
  else out.push({ op: op, text: text });
};
if (a.length * b.length > 250000) {
  if (a.length) push('del', a.join(''));
  if (b.length) push('ins', b.join(''));
  return out;
}
// lcs[i * (m + 1) + j]: the longest common subsequence of a[i..] and b[j..].
var n = a.length, m = b.length, w = m + 1;
var lcs = new Uint32Array((n + 1) * w);
for (var i = n - 1; i >= 0; i--) {
  for (var j = m - 1; j >= 0; j--) {
    lcs[i * w + j] = a[i] === b[j]
      ? lcs[(i + 1) * w + j + 1] + 1
      : Math.max(lcs[(i + 1) * w + j], lcs[i * w + j + 1]);
  }
}
var x = 0, y = 0;
while (x < n && y < m) {
  if (a[x] === b[y]) { push('eq', a[x]); x++; y++; }
  else if (lcs[(x + 1) * w + y] >= lcs[x * w + y + 1]) { push('del', a[x]); x++; }
  else { push('ins', b[y]); y++; }
}
while (x < n) { push('del', a[x]); x++; }
while (y < m) { push('ins', b[y]); y++; }
return out;
}

// END pure-text-helpers
// BEGIN page-status
// Pure functions, no DOM: tools/page-status.ts evaluates this region.

// The diagnostics panel's sentence for a page with `errors` errors and
// `warnings` warnings; '' when it has neither.
function diagSummary(errors, warnings) {
  var parts = [];
  if (errors > 0) {
    parts.push(errors + (errors === 1 ? ' error' : ' errors') + " on this page. The site can't be published until " + (errors === 1 ? "it's" : "they're") + ' fixed.');
  }
  if (warnings > 0) {
    parts.push(warnings + (warnings === 1 ? ' warning' : ' warnings') + ' on this page.');
  }
  return parts.join(' ');
}

// What the mascot shows: `kind` (its data-status), the `face`, the hover
// `title` and the `label` beside it ('' hides the label; only a page with no
// problems has one). Modes come first, then suggestions and unsaved edits;
// only when none of those apply do the page's errors and warnings replace
// the resting face.
function mascotState(mode, suggestions, dirty, errors, warnings) {
  var unsaved = dirty > 0 ? ' (' + dirty + ' unsaved)' : '';
  if (mode === 'edit') {
    return { kind: 'edit', face: '\u{270F}\u{FE0F}', title: 'Edit mode \u{2014} click to change' + unsaved, label: '' };
  }
  if (mode === 'suggest') {
    return { kind: 'suggest', face: '\u{1F4AC}', title: 'Suggest mode \u{2014} click to change' + unsaved, label: '' };
  }
  if (mode === 'schema') {
    return { kind: 'schema', face: '\u{1F4D0}', title: 'Structure mode \u{2014} viewing schema', label: '' };
  }
  if (suggestions > 0) {
    return { kind: 'suggestions', face: '\u{1F917}', title: suggestions + ' suggestion' + (suggestions === 1 ? '' : 's') + unsaved + ' \u{2014} click to edit', label: '' };
  }
  if (dirty > 0) {
    return { kind: 'unsaved', face: '\u{1F4BE}', title: dirty + ' unsaved document' + (dirty === 1 ? '' : 's') + ' \u{2014} click to change', label: '' };
  }
  if (errors > 0) {
    return { kind: 'errors', face: '\u{1F61F}', title: diagSummary(errors, warnings), label: '' };
  }
  if (warnings > 0) {
    return { kind: 'warnings', face: '\u{1F914}', title: diagSummary(errors, warnings), label: '' };
  }
  return { kind: 'clear', face: '\u{1F60C}', title: 'No issues on this page \u{2014} click to change mode', label: 'No issues' };
}

// END page-status

// What saving does in each intent: the primary button, the undo button's
// title and the messages. The class names are what CSS and the smoketest use.
var _EDITOR_INTENTS={
apply:{button:'presemble-save',icon:'&#10003;',title:'Save',cancel:'Undo',refused:'Edit refused',failed:'Edit failed',noFile:'Cannot save: missing file attribute on element. This is a bug — please report.'},
suggest:{button:'presemble-suggest-inline',icon:'&#128172;',title:'Suggest',cancel:'Cancel',refused:'Suggest refused',failed:'Suggest failed',noFile:'Cannot suggest: missing file attribute on element. This is a bug — please report.'}
};

// Open the block editor on `el` with `intent` ('apply' | 'suggest') and make
// it the active one. A body block is edited in a textarea in its place; any
// other element inline (contenteditable), as the server's markdown for it.
// Returns {intent, setIntent(intent), state(), changed(), owns(node), focus(),
// submit(), close()}, or null (with a toast) when the server gave the element
// no address or markdown. close() cancels: the rendered markup comes back.
// What saving does is read from `intent` at submit time: switching between
// edit and suggest mode changes it while the editor is open (setIntent).
// Every close ends in _onEditorClosed().
function _openBlockEditor(el,intent){
var labels=_EDITOR_INTENTS[intent];
var body=el.getAttribute('data-presemble-slot')==='body';
var file=el.getAttribute('data-presemble-file');
if(!file&&body){var fEl=document.querySelector('[data-presemble-file]');if(fEl){file=fEl.getAttribute('data-presemble-file');}}
// The server's address and markdown for this element; without both it is not editable.
var target=_editTargetOf(el);
if(!target){_toast(_NOT_EDITABLE_MSG);return null;}
var original=target.md;
var toolbar=document.createElement('div');
toolbar.className='presemble-edit-toolbar';
toolbar.innerHTML='<button class="'+labels.button+'" title="'+labels.title+'">'+labels.icon+'</button><button class="presemble-undo" title="'+labels.cancel+'">&#8630;</button>';
var ac=new AbortController();
var busy=false;
var errEl=null;
// The surface the user types in: a textarea for a body block, the element
// itself otherwise.
var ta=null;
var savedHtml=null;
function setEditable(on){if(on){el.contentEditable='true';}else{el.contentEditable='false';}}
function text(){return body?ta.value:_editedText(el);}
function setReadOnly(ro){if(body){ta.readOnly=ro;}else{setEditable(!ro);}}
function focus(){(body?ta:el).focus();}
if(body){
el.style.display='none';
ta=document.createElement('textarea');
ta.className='presemble-body-editor';
ta.value=original;
el.parentNode.insertBefore(ta,el.nextSibling);
ta.after(toolbar);
}else{
savedHtml=el.innerHTML;
el.innerText=original;
setEditable(true);
el.after(toolbar);
}
el.classList.add('presemble-editing');
focus();
if(!body&&!el.textContent.trim()){var r=document.createRange();r.selectNodeContents(el);r.collapse(true);var s=window.getSelection();s.removeAllRanges();s.addRange(r);}
// Close the editor; `restoreMarkup` puts the rendered markup back (inline only:
// a body block's rendered element is never changed).
function finish(restoreMarkup){
ac.abort();
if(body){el.style.display='';ta.remove();}
else{if(restoreMarkup){el.innerHTML=savedHtml;}setEditable(false);}
// Opened on a suggestion preview and closed outside suggest mode: the
// preview it put back is stale there, so show the page's own markup.
if(el.hasAttribute('data-presemble-original-html')&&!document.body.classList.contains('presemble-suggest-mode')){_resetPreview(el);}
el.classList.remove('presemble-editing');
toolbar.remove();
if(errEl){errEl.remove();errEl=null;}
var err=el.parentNode&&el.parentNode.querySelector('.presemble-edit-error');
if(err){err.remove();}
if(_activeEditor===editor){_activeEditor=null;}
_editorStateChanged();
_onEditorClosed();
}
function state(){return busy?'busy':(_sameMd(text(),original)?'unchanged':'changed');}
// On failure the editor stays open with the user's text.
function fail(msg){
busy=false;
_setToolbarBusy(toolbar,false);
setReadOnly(false);
errEl=_showEditError(toolbar,errEl,msg);
focus();
_editorStateChanged();
}
function submit(){
if(busy){return;}
// What saving does now: the intent may have changed since the editor opened.
var now=editor.intent;
var nowLabels=_EDITOR_INTENTS[now];
var value=text();
if(_sameMd(value,original)){finish(true);return;}
if(!file){console.error('block editor submit without a file',el);fail(nowLabels.noFile);return;}
busy=true;
_setToolbarBusy(toolbar,true);
setReadOnly(true);
_editorStateChanged();
_postBlockEdit(buildBlockEditRequest(file,target.address,original,value,now)).then(function(outcome){
var plan=submitOutcomePlan(now,outcome.kind);
if(plan.keepOpen){fail(outcome.message||nowLabels.refused);return;}
busy=false;
finish(plan.restoreMarkup);
if(plan.after==='dirty-count'){if(window._fetchDirtyCount){window._fetchDirtyCount();}}
else if(plan.after==='after-suggest'){_afterSuggest(outcome);}
}).catch(function(err){
fail(err.message||nowLabels.failed);
});
}
// Change what saving does (edit <-> suggest mode): the primary button's
// class, icon and title and the undo title follow; the text, the caret and
// any error stay as they are.
function setIntent(next){
var from=_EDITOR_INTENTS[editor.intent];
var to=_EDITOR_INTENTS[next];
var primary=toolbar.querySelector('.'+from.button);
primary.classList.remove(from.button);
primary.classList.add(to.button);
primary.innerHTML=to.icon;
primary.title=to.title;
toolbar.querySelector('.presemble-undo').title=to.cancel;
editor.intent=next;
}
var editor={
intent:intent,
setIntent:setIntent,
state:state,
focus:focus,
changed:function(){return !_sameMd(text(),original);},
// Whether `node` is part of this editor (the element, its textarea, toolbar
// or error message).
owns:function(node){return node===el||node===ta||node===toolbar||(!!errEl&&node===errEl);},
submit:submit,
close:function(){finish(true);}
};
toolbar.querySelector('.'+labels.button).onclick=function(ev){ev.stopPropagation();submit();};
toolbar.querySelector('.presemble-undo').onclick=function(ev){ev.stopPropagation();finish(true);};
(body?ta:el).addEventListener('keydown',function(ev){
if(!body&&ev.key==='Enter'&&!ev.shiftKey){ev.preventDefault();submit();}
if(ev.key==='Escape'&&!busy){finish(true);}
},{signal:ac.signal});
// The mode buttons follow the editor's state; typing only matters when it
// moves the editor between unchanged and changed.
var lastState=state();
(body?ta:el).addEventListener('input',function(){
var now=state();
if(now!==lastState){lastState=now;_editorStateChanged();}
},{signal:ac.signal});
_activeEditor=editor;
_editorStateChanged();
return editor;
}

// A click on an addressable element in edit mode (intent apply) or suggest
// mode (intent suggest) opens the block editor on it.
document.addEventListener('click',function(e){
var intent=document.body.classList.contains('presemble-edit-mode')?'apply'
:document.body.classList.contains('presemble-suggest-mode')?'suggest':null;
if(!intent){return;}
var el=e.target.closest('[data-presemble-slot]');
if(!el||el.classList.contains('presemble-editing')){return;}
// An editor is already open: submit it and stop. The submit is async and may
// fail (the editor then stays open with the user's text), so never open a
// second editor on top of it.
if(_activeEditor){
e.preventDefault();
_activeEditor.submit();
return;
}
var slot=el.getAttribute('data-presemble-slot');
if(slot==='body'){
e.preventDefault();
if(el.classList.contains('presemble-heading-folded')){if(window._foldToggle){window._foldToggle(el);}return;}
_openBlockEditor(el,intent);
return;
}
if(el.tagName==='IMG'){return;}
// In edit mode a link (not one shown from another slot) is changed by
// picking a target; in suggest mode it is edited inline as markdown.
if(intent==='apply'&&el.tagName==='A'&&!el.getAttribute('data-presemble-source-slot')){
e.preventDefault();
var afile=el.getAttribute('data-presemble-file');
var aslot=el.getAttribute('data-presemble-slot');
if(!afile||!aslot){return;}
var astem=afile.split('/')[1];
fetchJson('/_presemble/links?schema='+encodeURIComponent(astem)+'&slot='+encodeURIComponent(aslot))
.then(function(options){
if(!Array.isArray(options)){throw new Error('unexpected response');}
var lac=new AbortController();
var sel=document.createElement('select');
sel.className='presemble-link-picker';
var ph=document.createElement('option');
ph.textContent='Select '+aslot+'...';
ph.value='';
sel.appendChild(ph);
options.forEach(function(opt){
var o=document.createElement('option');
o.textContent=opt.text;
o.value=opt.href;
sel.appendChild(o);
});
el.after(sel);
sel.focus();
// The pick is a block edit: `[text](href)` at the link element, or at the
// empty slot's placeholder (ADR-059 section 10). The page reloads when the
// server says it was rebuilt.
sel.onchange=function(){
lac.abort();
var choice=options[sel.selectedIndex-1];
if(choice){
var ltarget=_editTargetOf(el);
if(!ltarget){sel.remove();_toast(_NOT_EDITABLE_MSG);return;}
_postBlockEdit(buildLinkPickerRequest(afile,ltarget.address,ltarget.md,choice.text,choice.href)).then(function(outcome){
sel.remove();
if(outcome.kind==='refused'){_toast('Link update failed: '+(outcome.message||'refused'));return;}
if(outcome.kind==='applied'&&window._fetchDirtyCount){window._fetchDirtyCount();}
}).catch(function(err){
sel.remove();
_toast('Link update failed: '+err.message);
});
}
};
sel.onblur=function(){lac.abort();setTimeout(function(){sel.remove();},200);};
document.addEventListener('keydown',function(e){if(e.key==='Escape'){lac.abort();sel.remove();}},{signal:lac.signal});
}).catch(function(err){_toast('Could not load link options: '+err.message);});
return;
}
// Edit mode swallowed the click before checking the element (as it always
// did); suggest mode only once the element is known to be editable.
if(intent==='apply'){e.preventDefault();}
var pfile=el.getAttribute('data-presemble-file');
if(!pfile||!slot){return;}
e.preventDefault();
_openBlockEditor(el,intent);
});
})();
