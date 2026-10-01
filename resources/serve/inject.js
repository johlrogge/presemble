(function(){
// Phase B5: edit-mode handlers emit NED programs via /_presemble/apply. Suggestion paths remain on legacy endpoints pending Phase C.

// Escape a JS string for embedding as a Clojure string literal.
// Handles backslash and double-quote. Keep this in sync with the server's
// clj_str_escape helper.
function cljStr(s) {
  return '"' + String(s).replace(/\\/g, '\\\\').replace(/"/g, '\\"') + '"';
}

// Legacy fallback used inside buildStructural* helpers when kind detection
// fails or the element is not in the slot-sibling list. New code should
// build selectors via buildStructuralSelection.
function slotChildIndex(el) {
  var slot = el.getAttribute('data-presemble-slot');
  if (!slot) return 0;
  var idx = 0;
  var sib = el.previousElementSibling;
  while (sib) {
    if (sib.getAttribute('data-presemble-slot') === slot) idx++;
    sib = sib.previousElementSibling;
  }
  return idx;
}

// --- Phase 2: Structural-anchor selectors (dead code; Phase 4 wires callers) ---
// Composer helpers — build NED Clojure source strings.
function nedDoc(file) {
  return '(ned/doc-by-path '+cljStr(file)+')';
}
function nedSlot(file, slot) {
  return '(ned/slot '+nedDoc(file)+' '+cljStr(slot)+')';
}
function nedChildrenSlot(file, slot) {
  return '(ned/children '+nedSlot(file, slot)+')';
}
function nedHeadingWithText(slotChildrenExpr, text) {
  return '(ned/heading-with-text '+slotChildrenExpr+' '+cljStr(text)+')';
}
function nedNthAfter(anchorExpr, kind, idx) {
  return '(ned/nth-after '+anchorExpr+' '+cljStr(kind)+' '+idx+')';
}
function nedNthOfKind(selExpr, kind, idx) {
  return '(ned/nth-of-kind '+selExpr+' '+cljStr(kind)+' '+idx+')';
}

// Map a DOM element to its NodeStore element-name (kind) per content_bridge.
function kindFromTag(el) {
  var t = el.tagName;
  if (/^H[1-6]$/.test(t)) return 'heading';
  if (t === 'P') return 'paragraph';
  if (t === 'PRE') return 'code-block';
  if (t === 'BLOCKQUOTE') return 'blockquote';
  if (t === 'IMG') return 'image';
  if (t === 'TABLE') return 'table';
  if (t === 'UL' || t === 'OL') return 'list';
  if (t === 'DIV') {
    var md = el.getAttribute('data-presemble-md') || '';
    if (/^\s*([-*+]|\d+\.)\s/.test(md)) return 'list';
    return 'raw-html';
  }
  return null;
}

// Collect all body-slot elements for a given file in document order.
function bodySiblingsForFile(file) {
  return Array.prototype.slice.call(
    document.querySelectorAll('[data-presemble-slot="body"][data-presemble-file="'+file+'"]')
  );
}

// Find the most-recent heading at or before `targetEl` within `sibs` (an
// ordered list of body-slot elements). Returns {el, text, anchorPos, targetPos}
// or null when targetEl has no preceding heading. anchorPos/targetPos are
// indices into sibs. Text is whitespace-normalised to match
// ned/heading-with-text's exact-equality semantics.
// Single source of truth for the heading text used in structural anchors.
// Used both when building selectors and when resolving anchors in the DOM.
function _headingAnchorText(el) {
  return el.innerText.trim().replace(/\s+/g, ' ');
}
function findPreviousHeadingForElement(sibs, targetEl) {
  var targetPos = sibs.indexOf(targetEl);
  if (targetPos < 0) return null;
  for (var i = targetPos; i >= 0; i--) {
    if (/^H[1-6]$/.test(sibs[i].tagName)) {
      return {
        el: sibs[i],
        text: _headingAnchorText(sibs[i]),
        anchorPos: i,
        targetPos: targetPos
      };
    }
  }
  return null;
}

// Count elements in sibs[fromExclusive..toExclusive) whose kind matches.
function countKindInRange(sibs, fromExclusive, toExclusive, kind) {
  var count = 0;
  for (var i = fromExclusive + 1; i < toExclusive; i++) {
    if (kindFromTag(sibs[i]) === kind) count++;
  }
  return count;
}

// Count elements in sibs[0..toExclusive) whose kind matches.
function countKindBeforeIndex(sibs, toExclusive, kind) {
  var count = 0;
  for (var i = 0; i < toExclusive; i++) {
    if (kindFromTag(sibs[i]) === kind) count++;
  }
  return count;
}

// Build a NED selection for a body-slot element, anchored on the nearest
// preceding heading when possible. Pure DOM-relative — no positional IDs.
function buildBodyAnchor(el, file) {
  var bodyEl = el.closest('[data-presemble-slot="body"]') || el;
  var sibs = bodySiblingsForFile(file);
  var targetPos = sibs.indexOf(bodyEl);
  if (targetPos < 0) {
    console.warn('buildBodyAnchor: target not in body slot list; falling back', bodyEl);
    return '(ned/nth-child '+nedSlot(file, 'body')+' '+slotChildIndex(el)+')';
  }
  var kind = kindFromTag(bodyEl);
  if (kind === null) {
    console.warn('buildBodyAnchor: unknown kind from tag', bodyEl.tagName);
    return '(ned/nth-child '+nedSlot(file, 'body')+' '+slotChildIndex(el)+')';
  }
  var slotChildren = nedChildrenSlot(file, 'body');
  var anchor = findPreviousHeadingForElement(sibs, bodyEl);
  if (anchor && anchor.anchorPos === targetPos) {
    // Editing the heading itself.
    return nedHeadingWithText(slotChildren, anchor.text);
  }
  if (anchor) {
    var relIdx = countKindInRange(sibs, anchor.anchorPos, targetPos, kind);
    return nedNthAfter(nedHeadingWithText(slotChildren, anchor.text), kind, relIdx);
  }
  // No preceding heading — slot-boundary fallback.
  var preIdx = countKindBeforeIndex(sibs, targetPos, kind);
  return nedNthOfKind(slotChildren, kind, preIdx);
}

// Build a NED selection for a preamble-slot element.
function buildPreambleAnchor(el, file, slot) {
  el = el.closest('[data-presemble-slot]') || el;
  var bySlot = Array.prototype.slice.call(
    document.querySelectorAll('[data-presemble-slot="'+slot+'"][data-presemble-file="'+file+'"]')
  );
  var bySource = Array.prototype.slice.call(
    document.querySelectorAll('[data-presemble-source-slot="'+slot+'"][data-presemble-file="'+file+'"]')
  );
  var sibs = bySlot.slice();
  for (var i = 0; i < bySource.length; i++) {
    if (sibs.indexOf(bySource[i]) === -1) sibs.push(bySource[i]);
  }
  sibs.sort(function(a, b) {
    var pos = a.compareDocumentPosition(b);
    if (pos & Node.DOCUMENT_POSITION_FOLLOWING) return -1;
    if (pos & Node.DOCUMENT_POSITION_PRECEDING) return 1;
    return 0;
  });
  // Single-child slot: select the slot Element directly. Safe for set-text
  // (descendants/texts walk the only child anyway). DO NOT use this branch
  // if a future caller passes the result to ned/replace — that would replace
  // the slot element rather than its child.
  if (sibs.length === 1) {
    return nedSlot(file, slot);
  }
  var kind = kindFromTag(el);
  if (kind === null) {
    console.warn('buildPreambleAnchor: unknown kind for', el.tagName);
    return '(ned/nth-child '+nedSlot(file, slot)+' '+slotChildIndex(el)+')';
  }
  var idx = 0;
  for (var m = 0; m < sibs.length; m++) {
    if (sibs[m] === el) break;
    if (kindFromTag(sibs[m]) === kind) idx++;
  }
  return nedNthOfKind(nedChildrenSlot(file, slot), kind, idx);
}

// Top-level dispatcher. Returns null when the element lacks file/slot data,
// letting callers fall back to the legacy slotChildIndex path.
function buildStructuralSelection(el) {
  var fileEl = el.closest('[data-presemble-file]');
  if (!fileEl) return null;
  var file = fileEl.getAttribute('data-presemble-file');
  var slot = el.getAttribute('data-presemble-source-slot') || el.getAttribute('data-presemble-slot');
  if (!file || !slot) return null;
  if (slot === 'body') return buildBodyAnchor(el, file);
  return buildPreambleAnchor(el, file, slot);
}
// --- end Phase 2 helpers ---

// Guardrails: if the server ever emits data-presemble-file="" these helpers
// fail fast rather than issuing /_presemble/grammar?stem=undefined requests.

var grammarCache = {};

async function fetchGrammar(stem) {
  if (stem === undefined || stem === null) {
    throw new Error('fetchGrammar called with undefined/null stem — check stemFromFile');
  }
  if (grammarCache[stem] !== undefined) return grammarCache[stem];
  var resp = await fetch('/_presemble/grammar?stem=' + encodeURIComponent(stem));
  if (!resp.ok) throw new Error('no grammar for stem "' + stem + '" (status ' + resp.status + ')');
  var src = await resp.text();
  grammarCache[stem] = src;
  return src;
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

async function applyNed(program) {
  var data = await fetchJson('/_presemble/apply', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({program: program}),
  });
  if (!data || !data.ok) throw new Error((data && data.error) || 'apply failed');
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

function stemFromFile(file) {
  // content/<stem>/<slug>.md -> "<stem>"
  // content/<slug>.md -> "" (root collection)
  if (!file) return ''; // empty / null / undefined → root collection
  var parts = String(file).split('/');
  if (parts.length <= 2) return ''; // content/<slug>.md OR single segment → root
  return parts[1]; // content/<stem>/...
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
var sock=new WebSocket((location.protocol==='https:'?'wss:':'ws:')+'//'+location.host+'/_presemble/ws');
sock.onopen=function(){
var recovered=_wsLost;
_wsAttempts=0;_wsLost=false;
// Messages may have been missed while disconnected: treat as a same-page reload.
if(recovered){_requestReload({resync:true});}
};
sock.onmessage=_wsOnMessage;
sock.onclose=function(){
_wsLost=true;
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
if(m.type==='suggestion-list-changed'){
if(window._fetchSuggestionFiles){window._fetchSuggestionFiles();}
if(window._fetchSuggestionCount){window._fetchSuggestionCount();}
return;
}
if(m.anchor){sessionStorage.setItem('presemble-anchor',JSON.stringify(m.anchor));}
_requestReload(m);
}
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
function _editEnter(){
if(_editToolbar){return;}
_editToolbar=document.createElement('div');
_editToolbar.className='presemble-edit-toolbar-bar';
_editToolbar.innerHTML='<button class="presemble-edit-save" style="display:none" title="Save all changes">\u{1F4BE} Save</button>'
+'<button class="presemble-edit-new" title="New content">\u{2795}</button>';
document.body.appendChild(_editToolbar);
_editToolbar.querySelector('.presemble-edit-new').onclick=function(){_openCreateDialog();};
_editToolbar.querySelector('.presemble-edit-save').onclick=function(){
fetchJson('/_presemble/save-all',{method:'POST'})
.then(function(data){
if(data.ok){_fetchDirtyCount();}
else{alert(data.error||'Save failed');}
})
.catch(function(err){_toast('Save failed: '+err.message);});
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
// NED suggestions: render diff from SearchReplace mutation.
// Built from DOM nodes with textContent only — suggestion text is untrusted.
if(sug._source==='ned'&&sug.mutation&&sug.mutation.SearchReplace){
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
function minimalDiff(original, edited) {
if (original === edited) return null;

// Find first differing character
var prefixLen = 0;
var minLen = Math.min(original.length, edited.length);
while (prefixLen < minLen && original[prefixLen] === edited[prefixLen]) {
prefixLen++;
}

// Find last differing character (from end), don't overlap prefix
var suffixLen = 0;
while (suffixLen < (minLen - prefixLen)
       && original[original.length - 1 - suffixLen] === edited[edited.length - 1 - suffixLen]) {
suffixLen++;
}

// If everything changed, return full strings
if (prefixLen === 0 && suffixLen === 0) {
return { search: original, replace: edited };
}

// Expand context for uniqueness — start with the bare change
var ctxLeft = 0;
var ctxRight = 0;
var maxLeft = prefixLen;
var maxRight = suffixLen;

while (true) {
var searchStart = prefixLen - ctxLeft;
var searchEnd = original.length - suffixLen + ctxRight;
var candidate = original.slice(searchStart, searchEnd);

// Count occurrences in original
var count = 0;
var idx = -1;
while ((idx = original.indexOf(candidate, idx + 1)) !== -1) {
count++;
if (count > 1) break;
}

if (count <= 1) break; // unique!

// Expand by snapping to word boundaries (~10 chars at a time)
if (ctxLeft < maxLeft) {
ctxLeft = Math.min(ctxLeft + 10, maxLeft);
// Snap to word boundary (space/newline) on left
while (ctxLeft < maxLeft && original[prefixLen - ctxLeft] !== ' ' && original[prefixLen - ctxLeft] !== '\n') {
ctxLeft++;
}
}
if (ctxRight < maxRight) {
ctxRight = Math.min(ctxRight + 10, maxRight);
// Snap to word boundary on right
while (ctxRight < maxRight && original[original.length - suffixLen + ctxRight - 1] !== ' ' && original[original.length - suffixLen + ctxRight - 1] !== '\n') {
ctxRight++;
}
}

// If we've expanded to full string, stop
if (ctxLeft >= maxLeft && ctxRight >= maxRight) break;
}

// Build final search/replace with the context
var finalStart = prefixLen - ctxLeft;
var finalEnd = original.length - suffixLen + ctxRight;
var search = original.slice(finalStart, finalEnd);
var replace = edited.slice(finalStart, edited.length - suffixLen + ctxRight);

return { search: search, replace: replace };
}
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
var bmd=el.getAttribute('data-presemble-md')||el.innerText;
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
if(bvalue===bmd||!bvalue.trim()){bcleanup();return;}
if(!bfile){console.error('bsave called without bfile',el);bfail('Cannot save: missing file attribute on element. This is a bug — please report.');return;}
bbusy=true;
_setToolbarBusy(btoolbar,true);
ta.readOnly=true;
var bstem=stemFromFile(bfile);
fetchGrammar(bstem).then(function(schemaSrc){
var sel = buildStructuralSelection(el);
if (sel === null) {
  console.error('buildStructuralSelection returned null for body element', el);
  throw new Error('Cannot save: could not locate this element in the document.');
}
var program = '(let [g (ned/parse-grammar '+cljStr(schemaSrc)+')]\n  (ned/replace\n    '+sel+'\n    (ned/parse-body '+cljStr(bvalue)+' g)))';
return applyNed(program);
}).then(function(){
bbusy=false;
bcleanup();
if(window._fetchDirtyCount){window._fetchDirtyCount();}
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
o.value=opt.text+'|'+opt.href;
sel.appendChild(o);
});
el.after(sel);
sel.focus();
// TODO(phase-c): route through /_presemble/apply once NED has link/image mutations
sel.onchange=function(){
lac.abort();
if(sel.value){
fetchJson('/_presemble/edit',{
method:'POST',
headers:{'Content-Type':'application/json'},
body:JSON.stringify({file:afile,slot:aslot,value:sel.value})
}).then(function(data){
sel.remove();
if(data.ok){setTimeout(function(){location.reload();},500);}
else{alert(data.error||'Link update failed');}
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
var editSlot=el.getAttribute('data-presemble-source-slot')||slot;
if(!pfile||!slot){return;}
var original=el.innerText;
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
var value=el.innerText.trim();
if(value===original||!value){cleanup();return;}
if(!pfile){console.error('save called without pfile',el);fail('Cannot save: missing file attribute on element. This is a bug — please report.');return;}
var sel = buildStructuralSelection(el);
if (sel === null) {
  var idx = slotChildIndex(el);
  sel = '(ned/nth-child (ned/slot (ned/doc-by-path '+cljStr(pfile)+') '+cljStr(editSlot)+') '+idx+')';
}
var program = '(ned/set-text (-> '+sel+' ned/descendants ned/texts) '+cljStr(value)+')';
busy=true;
_setToolbarBusy(toolbar,true);
el.contentEditable='false';
applyNed(program).then(function(){
busy=false;
cleanup();
if(window._fetchDirtyCount){window._fetchDirtyCount();}
}).catch(function(e){
fail(e.message||'Edit failed');
});
}
toolbar.querySelector('.presemble-save').onclick=function(e){e.stopPropagation();save();};
toolbar.querySelector('.presemble-undo').onclick=function(e){e.stopPropagation();el.innerText=original;cleanup();};
el.addEventListener('keydown',function(e){
if(e.key==='Enter'&&!e.shiftKey){e.preventDefault();save();}
if(e.key==='Escape'&&!busy){el.innerText=original;cleanup();}
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
var bmd=el.getAttribute('data-presemble-md')||el.innerText;
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
var diff=minimalDiff(bmd,bvalue);
if(!diff){bcleanup();return;}
var sel = buildStructuralSelection(el);
if (sel === null) {
  console.error('buildStructuralSelection returned null for body element', el);
  bfail('Cannot suggest: could not locate this element in the document.');
  return;
}
bbusy=true;
_setToolbarBusy(btoolbar,true);
ta.readOnly=true;
fetchJson('/_presemble/ned-suggestions',{method:'POST',headers:{'Content-Type':'application/json'},
body:JSON.stringify({file:bfile,selection:sel,mutation:{SearchReplace:{search:diff.search,replace:diff.replace}},reason:''})
}).then(function(data){
if(!data.ok){throw new Error(data.error||'Suggest failed');}
bbusy=false;
bcleanup();
if(data.id&&document.body.classList.contains('presemble-suggest-mode')&&window._suggestEnter){
window._suggestEnter(data.id);
}else if(window._fetchSuggestionCount){
window._fetchSuggestionCount();
}
}).catch(function(err){bfail(err.message||'Suggest failed');});
}
btoolbar.querySelector('.presemble-suggest-inline').onclick=function(ev){ev.stopPropagation();bsuggest();};
btoolbar.querySelector('.presemble-undo').onclick=function(ev){ev.stopPropagation();bcleanup();};
ta.addEventListener('keydown',function(ev){if(ev.key==='Escape'&&!bbusy){bcleanup();}},{signal:bac.signal});
return;
}
var pfile=el.getAttribute('data-presemble-file');
var editSlot=el.getAttribute('data-presemble-source-slot')||slot;
if(!pfile||!editSlot){return;}
e.preventDefault();
var original=el.innerText;
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
var value=el.innerText.trim().replace(/\u00a0/g,' ');
var origText=original.replace(/\u00a0/g,' ');
var diff=minimalDiff(origText,value);
if(!diff){cleanup();return;}
var sel = buildStructuralSelection(el);
if (sel === null) {
  var idx = slotChildIndex(el);
  sel = '(ned/nth-child (ned/slot (ned/doc-by-path '+cljStr(pfile)+') '+cljStr(editSlot)+') '+idx+')';
}
busy=true;
_setToolbarBusy(toolbar,true);
el.contentEditable='false';
fetchJson('/_presemble/ned-suggestions',{method:'POST',headers:{'Content-Type':'application/json'},
body:JSON.stringify({file:pfile,selection:sel,mutation:{SearchReplace:{search:diff.search,replace:diff.replace}},reason:''})
}).then(function(data){
if(!data.ok){throw new Error(data.error||'Suggest failed');}
// A suggestion is not an edit: restore the original text before closing.
busy=false;
el.innerText=original;
cleanup();
if(data.id&&document.body.classList.contains('presemble-suggest-mode')&&window._suggestEnter){
window._suggestEnter(data.id);
}else if(window._fetchSuggestionCount){
window._fetchSuggestionCount();
}
}).catch(function(e){
fail(e.message||'Suggest failed');
});
}
toolbar.querySelector('.presemble-suggest-inline').onclick=function(ev){ev.stopPropagation();suggest();};
toolbar.querySelector('.presemble-undo').onclick=function(ev){ev.stopPropagation();el.innerText=original;cleanup();};
el.addEventListener('keydown',function(ev){
if(ev.key==='Enter'&&!ev.shiftKey){ev.preventDefault();suggest();}
if(ev.key==='Escape'&&!busy){el.innerText=original;cleanup();}
},{signal:ac.signal});
});
})();
