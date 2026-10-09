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

// Editor-session helpers shared by the four inline editors.
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
function _editorOpen(){return !!document.querySelector('.presemble-editing,.presemble-body-editor');}
// A reload concerns this page when: it is a resync (lagged socket or reconnect
// after loss), `pages` is missing/empty (full rebuild → reload in place), or
// this page is listed in `pages`.
function _reloadConcernsThisPage(m){return !!m.resync||!m.pages||!m.pages.length||m.pages.indexOf(location.pathname)!==-1;}
function _requestReload(m){
if(_editorOpen()){
// Never navigate away from an open editor; defer a same-page reload until it closes.
if(_reloadConcernsThisPage(m)){
_pendingReload=true;
_toast('Page updated — will refresh when you finish editing',true);
}
return;
}
if(_reloadConcernsThisPage(m)){location.reload();}
else if(m.primary){location.href=m.primary;}
}
// Call at the end of every editor cleanup.
function _onEditorClosed(){if(_pendingReload&&!_editorOpen()){location.reload();}}
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
if(m.type==='scroll'){
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
if(m.type==='resync'){
_requestReload({resync:true});
return;
}
if(m.type==='conflict'){
_noteConflict(m.file);
return;
}
if(m.type==='suggestion-list-changed'){
if(window._fetchSuggestionFiles){window._fetchSuggestionFiles();}
if(window._fetchSuggestionCount){window._fetchSuggestionCount();}
return;
}
if(m.anchor){sessionStorage.setItem('presemble-anchor',JSON.stringify(m.anchor));}
_requestReload(m);
}
// ── Disk conflicts (ADR-065) ──────────────────────────────────────────────
// A file that changed on disk while it has unsaved edits here. The notice is
// persistent and has no buttons: the toolbar Save (with its confirmation) is
// the one way to keep the edits; `presemble reload` keeps the file.
var _conflictFiles=[];
function _showConflictNotice(){
document.querySelectorAll('.presemble-conflict-notice').forEach(function(n){n.remove();});
var onPage={};
document.querySelectorAll('[data-presemble-file]').forEach(function(el){onPage[el.getAttribute('data-presemble-file')]=true;});
var here=_conflictFiles.filter(function(f){return onPage[f];});
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
function _noteConflict(file){
if(_conflictFiles.indexOf(file)<0){_conflictFiles.push(file);}
_showConflictNotice();
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
var summary=[];
if(errors>0){summary.push(errors+(errors===1?' error':' errors')+" on this page. The site can't be published until "+(errors===1?"it's":"they're")+" fixed.");}
if(warnings>0){summary.push(warnings+(warnings===1?' warning':' warnings')+' on this page.');}
head.textContent=summary.join(' ');
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
if(document.readyState==='loading'){document.addEventListener('DOMContentLoaded',_diagRender);}
else{_diagRender();}
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
var icon=document.createElement('button');icon.className='presemble-mascot-icon';
var badge=document.createElement('span');badge.className='presemble-mascot-badge';
var menu=document.createElement('div');menu.className='presemble-mascot-menu';
var viewBtn=document.createElement('button');viewBtn.textContent='\u{1F441} View';
var editBtn=document.createElement('button');editBtn.textContent='\u{270F}\u{FE0F} Edit';
var suggestBtn=document.createElement('button');suggestBtn.textContent='\u{1F4AC} Suggest';suggestBtn.style.position='relative';
var suggestBadge=document.createElement('span');suggestBadge.className='presemble-suggest-badge';suggestBadge.style.display='none';suggestBtn.appendChild(suggestBadge);
var structureBtn=document.createElement('button');structureBtn.textContent='\u{1F4D0} Structure';
menu.appendChild(viewBtn);menu.appendChild(editBtn);menu.appendChild(suggestBtn);menu.appendChild(structureBtn);
container.appendChild(icon);container.appendChild(badge);container.appendChild(menu);
document.body.appendChild(container);
function update(){
var totalBadge=_editorialSuggestCount+_dirtyCount;
if(totalBadge>0){badge.textContent=totalBadge;badge.style.display='block';}
else{badge.style.display='none';}
if(mode==='edit'){icon.textContent='\u{270F}\u{FE0F}';icon.title='Edit mode \u{2014} click to change';
if(_dirtyCount>0){icon.title+=' ('+_dirtyCount+' unsaved)';}
}
else if(mode==='suggest'){icon.textContent='\u{1F4AC}';icon.title='Suggest mode \u{2014} click to change';
if(_dirtyCount>0){icon.title+=' ('+_dirtyCount+' unsaved)';}
}
else if(mode==='schema'){icon.textContent='\u{1F4D0}';icon.title='Structure mode \u{2014} viewing schema';}
else if(totalBadge===0&&_dirtyCount===0){icon.textContent='\u{1F44D}';icon.title='All clear \u{2014} ready to publish';}
else if(_dirtyCount>0&&totalBadge===0){icon.textContent='\u{1F4BE}';icon.title=_dirtyCount+' unsaved change'+(_dirtyCount===1?'':'s')+' \u{2014} click to change';}
else{icon.textContent='\u{1F917}';icon.title=totalBadge+' suggestion'+(totalBadge===1?'':'s')+(_dirtyCount>0?' ('+_dirtyCount+' unsaved)':'')+' \u{2014} click to edit';}
viewBtn.className=mode==='view'?'active':'';
editBtn.className=mode==='edit'?'active':'';
suggestBtn.className=mode==='suggest'?'active':'';
structureBtn.className=mode==='schema'?'active':'';
document.body.classList.toggle('presemble-edit-mode',mode==='edit');
document.body.classList.toggle('presemble-suggest-mode',mode==='suggest');
}
update();
icon.onclick=function(e){e.stopPropagation();menu.classList.toggle('open');};
document.addEventListener('click',function(){menu.classList.remove('open');});
menu.onclick=function(e){e.stopPropagation();};
function cleanupEditing(){document.querySelectorAll('.presemble-editing').forEach(function(el){el.contentEditable='false';el.classList.remove('presemble-editing');});document.querySelectorAll('.presemble-edit-toolbar,.presemble-edit-error,.presemble-link-picker').forEach(function(el){el.remove();});_onEditorClosed();}
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
el.innerHTML=el.getAttribute('data-presemble-original-html');
el.removeAttribute('data-presemble-original-html');
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
document.querySelectorAll('[data-presemble-original-html]').forEach(function(el){
el.innerHTML=el.getAttribute('data-presemble-original-html');
el.removeAttribute('data-presemble-original-html');
});
if(_suggestions.length===0){return;}
var sug=_suggestions[_suggestIdx];
var el=_suggestFindTarget(sug);
_suggestActiveEl=el;
if(el){
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
// The change a block-edit suggestion proposes: the base markdown struck out,
// the proposed markdown after it (blocks keep their line breaks, so a
// structural change such as one paragraph into two reads as such).
function _renderPreviewDiff(el,base,proposed){
el.textContent='';
var del=_diffNode('del',base);
var ins=_diffNode('ins',proposed);
[del,ins].forEach(function(n){n.style.display='block';n.style.whiteSpace='pre-wrap';});
el.appendChild(del);
el.appendChild(ins);
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
function _fetchSuggestionCount(){
var fileEl=document.querySelector('[data-presemble-file]');
if(!fileEl){return;}
var file=fileEl.getAttribute('data-presemble-file');
if(!file){return;}
var enc=encodeURIComponent(file);
fetchJson('/_presemble/ned-suggestions?file='+enc).catch(function(err){console.warn('presemble: suggestion count fetch failed',err);return[];})
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
var fileEl=document.querySelector('[data-presemble-file]');
if(!fileEl){return;}
var file=fileEl.getAttribute('data-presemble-file');
if(!file){return;}
var enc=encodeURIComponent(file);
fetchJson('/_presemble/ned-suggestions?file='+enc).catch(function(err){_toast('Could not load suggestions: '+err.message);return[];})
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
function setMode(m){
if(m!=='edit'){cleanupEditing();_editCleanup();}
if(m!=='suggest'){_suggestCleanup();}
mode=m;
menu.classList.remove('open');
update();
if(m==='edit'){_editEnter();}
if(m==='suggest'){_suggestEnter();}else{_fetchSuggestionCount();}
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

// END pure-text-helpers
document.addEventListener('click',function(e){
if(!document.body.classList.contains('presemble-edit-mode')){return;}
var el=e.target.closest('[data-presemble-slot]');
if(!el||el.classList.contains('presemble-editing')){return;}
// An editor is already open: commit it and stop. The save is async and may
// fail (the editor then stays open with the user's text), so never open a
// second editor on top of it.
var editing=document.querySelector('.presemble-editing');
var openTa=document.querySelector('.presemble-body-editor');
if(editing||openTa){
e.preventDefault();
var pendingSave=null;
if(openTa){
pendingSave=openTa.nextElementSibling&&openTa.nextElementSibling.querySelector('.presemble-save');
if(!pendingSave){pendingSave=openTa.parentNode.querySelector('.presemble-edit-toolbar .presemble-save');}
}else{
pendingSave=editing.nextElementSibling&&editing.nextElementSibling.querySelector('.presemble-save');
if(!pendingSave){pendingSave=editing.parentNode.querySelector('.presemble-edit-toolbar .presemble-save');}
}
if(pendingSave&&!pendingSave.disabled){pendingSave.click();}
return;
}
if(el.getAttribute('data-presemble-slot')==='body'){
if(el.classList.contains('presemble-heading-folded')){e.preventDefault();if(window._foldToggle){window._foldToggle(el);}return;}
e.preventDefault();
var bfile=el.getAttribute('data-presemble-file');
if(!bfile){var bfEl=document.querySelector('[data-presemble-file]');if(bfEl){bfile=bfEl.getAttribute('data-presemble-file');}}
// The server's address and markdown for this block; without both it is not editable.
var btarget=_editTargetOf(el);
if(!btarget){_toast(_NOT_EDITABLE_MSG);return;}
var bmd=btarget.md;
el.style.display='none';
var ta=document.createElement('textarea');
ta.className='presemble-body-editor';
ta.value=bmd;
el.parentNode.insertBefore(ta,el.nextSibling);
ta.focus();
var btoolbar=document.createElement('div');
btoolbar.className='presemble-edit-toolbar';
btoolbar.innerHTML='<button class="presemble-save" title="Save">&#10003;</button><button class="presemble-undo" title="Undo">&#8630;</button>';
ta.after(btoolbar);
el.classList.add('presemble-editing');
var bac=new AbortController();
var bbusy=false;
var berrEl=null;
function bcleanup(){
bac.abort();
el.style.display='';
el.classList.remove('presemble-editing');
ta.remove();
btoolbar.remove();
if(berrEl){berrEl.remove();berrEl=null;}
var berr=el.parentNode&&el.parentNode.querySelector('.presemble-edit-error');
if(berr){berr.remove();}
_onEditorClosed();
}
// On failure the editor stays open with the user's text.
var bfail=function(msg){
bbusy=false;
_setToolbarBusy(btoolbar,false);
ta.readOnly=false;
berrEl=_showEditError(btoolbar,berrEl,msg);
ta.focus();
};
function bsave(){
if(bbusy){return;}
var bvalue=ta.value;
if(_sameMd(bvalue,bmd)){bcleanup();return;}
if(!bfile){console.error('bsave called without bfile',el);bfail('Cannot save: missing file attribute on element. This is a bug — please report.');return;}
bbusy=true;
_setToolbarBusy(btoolbar,true);
ta.readOnly=true;
_postBlockEdit(buildBlockEditRequest(bfile,btarget.address,bmd,bvalue,'apply')).then(function(outcome){
if(outcome.kind==='refused'){bfail(outcome.message||'Edit refused');return;}
bbusy=false;
bcleanup();
if(outcome.kind==='applied'&&window._fetchDirtyCount){window._fetchDirtyCount();}
}).catch(function(err){
bfail(err.message||'Edit failed');
});
}
btoolbar.querySelector('.presemble-save').onclick=function(ev){ev.stopPropagation();bsave();};
btoolbar.querySelector('.presemble-undo').onclick=function(ev){ev.stopPropagation();bcleanup();};
ta.addEventListener('keydown',function(ev){
if(ev.key==='Escape'&&!bbusy){bcleanup();}
},{signal:bac.signal});
return;
}
if(el.tagName==='IMG'){return;}
if(el.tagName==='A'&&!el.getAttribute('data-presemble-source-slot')){
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
e.preventDefault();
var pfile=el.getAttribute('data-presemble-file');
var slot=el.getAttribute('data-presemble-slot');
if(!pfile||!slot){return;}
// The element is edited as the server's markdown for it, addressed by the
// server's address; without both it is not editable. Cancelling restores the
// rendered markup.
var target=_editTargetOf(el);
if(!target){_toast(_NOT_EDITABLE_MSG);return;}
var savedHtml=el.innerHTML;
var original=target.md;
function restore(){el.innerHTML=savedHtml;}
el.innerText=original;
el.contentEditable='true';
el.classList.add('presemble-editing');
el.focus();
if(!el.textContent.trim()){var r=document.createRange();r.selectNodeContents(el);r.collapse(true);var s=window.getSelection();s.removeAllRanges();s.addRange(r);}
var toolbar=document.createElement('div');
toolbar.className='presemble-edit-toolbar';
toolbar.innerHTML='<button class="presemble-save" title="Save">&#10003;</button><button class="presemble-undo" title="Undo">&#8630;</button>';
el.after(toolbar);
var ac=new AbortController();
var busy=false;
var errEl=null;
function cleanup(){
ac.abort();
el.contentEditable='false';
el.classList.remove('presemble-editing');
toolbar.remove();
if(errEl){errEl.remove();errEl=null;}
var err=el.parentNode&&el.parentNode.querySelector('.presemble-edit-error');
if(err){err.remove();}
_onEditorClosed();
}
// On failure the editor stays open with the user's text.
function fail(msg){
busy=false;
_setToolbarBusy(toolbar,false);
el.contentEditable='true';
errEl=_showEditError(toolbar,errEl,msg);
el.focus();
}
function save(){
if(busy){return;}
var value=_editedText(el);
if(_sameMd(value,original)){restore();cleanup();return;}
if(!pfile){console.error('save called without pfile',el);fail('Cannot save: missing file attribute on element. This is a bug — please report.');return;}
busy=true;
_setToolbarBusy(toolbar,true);
el.contentEditable='false';
_postBlockEdit(buildBlockEditRequest(pfile,target.address,original,value,'apply')).then(function(outcome){
if(outcome.kind==='refused'){fail(outcome.message||'Edit refused');return;}
busy=false;
if(outcome.kind==='no-change'){restore();}
cleanup();
if(outcome.kind==='applied'&&window._fetchDirtyCount){window._fetchDirtyCount();}
}).catch(function(e){
fail(e.message||'Edit failed');
});
}
toolbar.querySelector('.presemble-save').onclick=function(e){e.stopPropagation();save();};
toolbar.querySelector('.presemble-undo').onclick=function(e){e.stopPropagation();restore();cleanup();};
el.addEventListener('keydown',function(e){
if(e.key==='Enter'&&!e.shiftKey){e.preventDefault();save();}
if(e.key==='Escape'&&!busy){restore();cleanup();}
},{signal:ac.signal});
});
document.addEventListener('click',function(e){
var suggestMode=document.body.classList.contains('presemble-suggest-mode');
if(!suggestMode){return;}
var el=e.target.closest('[data-presemble-slot]');
if(!el||el.classList.contains('presemble-editing')){return;}
var slot=el.getAttribute('data-presemble-slot');
if(!slot){return;}
if(slot==='body'){
e.preventDefault();
var bfile=el.getAttribute('data-presemble-file');
if(!bfile){var bfEl=document.querySelector('[data-presemble-file]');if(bfEl){bfile=bfEl.getAttribute('data-presemble-file');}}
// The server's address and markdown for this block; without both it is not editable.
var btarget=_editTargetOf(el);
if(!btarget){_toast(_NOT_EDITABLE_MSG);return;}
var bmd=btarget.md;
el.style.display='none';
var ta=document.createElement('textarea');
ta.className='presemble-body-editor';
ta.value=bmd;
el.parentNode.insertBefore(ta,el.nextSibling);
ta.focus();
var btoolbar=document.createElement('div');
btoolbar.className='presemble-edit-toolbar';
btoolbar.innerHTML='<button class="presemble-suggest-inline" title="Suggest">&#128172;</button><button class="presemble-undo" title="Cancel">&#8630;</button>';
ta.after(btoolbar);
var bac=new AbortController();
var bbusy=false;
var berrEl=null;
function bcleanup(){
bac.abort();
ta.remove();btoolbar.remove();el.style.display='';
if(berrEl){berrEl.remove();berrEl=null;}
_onEditorClosed();
}
// On failure the editor stays open with the user's text.
var bfail=function(msg){
bbusy=false;
_setToolbarBusy(btoolbar,false);
ta.readOnly=false;
berrEl=_showEditError(btoolbar,berrEl,msg);
ta.focus();
};
function bsuggest(){
if(bbusy){return;}
var bvalue=ta.value;
if(_sameMd(bvalue,bmd)){bcleanup();return;}
if(!bfile){console.error('bsuggest called without bfile',el);bfail('Cannot suggest: missing file attribute on element. This is a bug — please report.');return;}
bbusy=true;
_setToolbarBusy(btoolbar,true);
ta.readOnly=true;
_postBlockEdit(buildBlockEditRequest(bfile,btarget.address,bmd,bvalue,'suggest')).then(function(outcome){
if(outcome.kind==='refused'){bfail(outcome.message||'Suggest refused');return;}
bbusy=false;
bcleanup();
_afterSuggest(outcome);
}).catch(function(err){bfail(err.message||'Suggest failed');});
}
btoolbar.querySelector('.presemble-suggest-inline').onclick=function(ev){ev.stopPropagation();bsuggest();};
btoolbar.querySelector('.presemble-undo').onclick=function(ev){ev.stopPropagation();bcleanup();};
ta.addEventListener('keydown',function(ev){if(ev.key==='Escape'&&!bbusy){bcleanup();}},{signal:bac.signal});
return;
}
if(el.tagName==='IMG'){return;}
var pfile=el.getAttribute('data-presemble-file');
if(!pfile){return;}
e.preventDefault();
// The element is suggested against the server's markdown for it, addressed by
// the server's address; without both it is not editable. Closing restores the
// rendered markup.
var target=_editTargetOf(el);
if(!target){_toast(_NOT_EDITABLE_MSG);return;}
var savedHtml=el.innerHTML;
var original=target.md;
function restore(){el.innerHTML=savedHtml;}
el.innerText=original;
el.contentEditable='true';
el.classList.add('presemble-editing');
el.focus();
if(!el.textContent.trim()){var r=document.createRange();r.selectNodeContents(el);r.collapse(true);var s=window.getSelection();s.removeAllRanges();s.addRange(r);}
var toolbar=document.createElement('div');
toolbar.className='presemble-edit-toolbar';
toolbar.innerHTML='<button class="presemble-suggest-inline" title="Suggest">&#128172;</button><button class="presemble-undo" title="Cancel">&#8630;</button>';
el.after(toolbar);
var ac=new AbortController();
var busy=false;
var errEl=null;
function cleanup(){
ac.abort();
el.contentEditable='false';
el.classList.remove('presemble-editing');
toolbar.remove();
if(errEl){errEl.remove();errEl=null;}
var err=el.parentNode&&el.parentNode.querySelector('.presemble-edit-error');
if(err){err.remove();}
_onEditorClosed();
}
// On failure the editor stays open with the user's text.
function fail(msg){
busy=false;
_setToolbarBusy(toolbar,false);
el.contentEditable='true';
errEl=_showEditError(toolbar,errEl,msg);
el.focus();
}
function suggest(){
if(busy){return;}
var value=_editedText(el);
if(_sameMd(value,original)){restore();cleanup();return;}
busy=true;
_setToolbarBusy(toolbar,true);
el.contentEditable='false';
_postBlockEdit(buildBlockEditRequest(pfile,target.address,original,value,'suggest')).then(function(outcome){
if(outcome.kind==='refused'){fail(outcome.message||'Suggest refused');return;}
// A suggestion is not an edit: restore the original markup before closing.
busy=false;
restore();
cleanup();
_afterSuggest(outcome);
}).catch(function(e){
fail(e.message||'Suggest failed');
});
}
toolbar.querySelector('.presemble-suggest-inline').onclick=function(ev){ev.stopPropagation();suggest();};
toolbar.querySelector('.presemble-undo').onclick=function(ev){ev.stopPropagation();restore();cleanup();};
el.addEventListener('keydown',function(ev){
if(ev.key==='Enter'&&!ev.shiftKey){ev.preventDefault();suggest();}
if(ev.key==='Escape'&&!busy){restore();cleanup();}
},{signal:ac.signal});
});
})();
