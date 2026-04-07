(function(){
var ws=new WebSocket('ws://'+location.host+'/_presemble/ws');
var _userScrolled=false;var _scrollTimer=null;var _presembleScrolling=false;
window.addEventListener('scroll',function(){if(_presembleScrolling){return;}_userScrolled=true;clearTimeout(_scrollTimer);_scrollTimer=setTimeout(function(){_userScrolled=false;},3000);},true);
ws.onmessage=function(e){
var m=JSON.parse(e.data);
if(m.type==='scroll'){
if(m.anchor){
var el=document.getElementById(m.anchor);
if(el){
var r=el.getBoundingClientRect();
if(r.top<0||r.bottom>window.innerHeight){
_presembleScrolling=true;
el.scrollIntoView({behavior:'smooth',block:'center'});
setTimeout(function(){_presembleScrolling=false;},500);
}
}
}
return;
}
if(m.anchor){sessionStorage.setItem('presemble-anchor',m.anchor);}
if(document.querySelector('.presemble-editing,.presemble-body-editor')){return;}
if(!m.pages||!m.pages.length||m.pages.indexOf(location.pathname)!==-1){location.reload();}
else{location.href=m.primary;}
};
ws.onclose=function(){setTimeout(function(){location.reload();},1000);};
(function(){
var anchor=sessionStorage.getItem('presemble-anchor');
if(!anchor){return;}
sessionStorage.removeItem('presemble-anchor');
function tryScroll(n){
var el=document.getElementById(anchor);
if(el){
el.scrollIntoView({behavior:'smooth',block:'center'});
el.classList.add('presemble-changed');
setTimeout(function(){el.classList.remove('presemble-changed');},1500);
}else if(n>0){setTimeout(function(){tryScroll(n-1);},50);}
}
if(document.readyState==='loading'){document.addEventListener('DOMContentLoaded',function(){tryScroll(10);});}
else{tryScroll(10);}
})();
(function(){
var mode=sessionStorage.getItem('presemble-mode')||'view';
var _editorialSuggestCount=0;
var _dirtyCount=0;
function countSuggestions(){return document.querySelectorAll('.presemble-suggestion').length;}
var container=document.createElement('div');container.className='presemble-mascot';
var icon=document.createElement('button');icon.className='presemble-mascot-icon';
var badge=document.createElement('span');badge.className='presemble-mascot-badge';
var menu=document.createElement('div');menu.className='presemble-mascot-menu';
var viewBtn=document.createElement('button');viewBtn.textContent='\u{1F441} View';
var editBtn=document.createElement('button');editBtn.textContent='\u{270F}\u{FE0F} Edit';
var suggestBtn=document.createElement('button');suggestBtn.textContent='\u{1F4AC} Suggest';suggestBtn.style.position='relative';
var suggestBadge=document.createElement('span');suggestBadge.className='presemble-suggest-badge';suggestBadge.style.display='none';suggestBtn.appendChild(suggestBadge);
menu.appendChild(viewBtn);menu.appendChild(editBtn);menu.appendChild(suggestBtn);
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
else if(totalBadge===0&&_dirtyCount===0){icon.textContent='\u{1F44D}';icon.title='All clear \u{2014} ready to publish';}
else if(_dirtyCount>0&&totalBadge===0){icon.textContent='\u{1F4BE}';icon.title=_dirtyCount+' unsaved change'+(_dirtyCount===1?'':'s')+' \u{2014} click to change';}
else{icon.textContent='\u{1F917}';icon.title=totalBadge+' suggestion'+(totalBadge===1?'':'s')+(_dirtyCount>0?' ('+_dirtyCount+' unsaved)':'')+' \u{2014} click to edit';}
viewBtn.className=mode==='view'?'active':'';
editBtn.className=mode==='edit'?'active':'';
suggestBtn.className=mode==='suggest'?'active':'';
if(mode==='edit'){document.body.classList.add('presemble-edit-mode');}else{document.body.classList.remove('presemble-edit-mode');}
}
if(mode==='edit'){document.body.classList.add('presemble-edit-mode');}
update();
icon.onclick=function(e){e.stopPropagation();menu.classList.toggle('open');};
document.addEventListener('click',function(){menu.classList.remove('open');});
menu.onclick=function(e){e.stopPropagation();};
function cleanupEditing(){document.querySelectorAll('.presemble-editing').forEach(function(el){el.contentEditable='false';el.classList.remove('presemble-editing');});document.querySelectorAll('.presemble-edit-toolbar,.presemble-edit-error,.presemble-link-picker').forEach(function(el){el.remove();});}
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
fetch('/_presemble/save-all',{method:'POST'})
.then(function(r){return r.json();})
.then(function(data){
if(data.ok){_fetchDirtyCount();}
else{alert(data.error||'Save failed');}
});
};
}
function _editCleanup(){
if(_editToolbar){_editToolbar.remove();_editToolbar=null;}
}
function _openCreateDialog(){
fetch('/_presemble/schemas').then(function(r){return r.json();}).then(function(schemas){
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
var ts=Date.now().toString(36);slugInput.value=typeSelect.value+'-'+ts;
slugInput.style.cssText='display:block;width:100%;box-sizing:border-box;padding:0.4rem;border:1px solid #ccc;border-radius:0.3rem;margin-bottom:0.75rem;font-size:1rem;';
typeSelect.onchange=function(){slugInput.value=typeSelect.value+'-'+Date.now().toString(36);};
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
fetch('/_presemble/create-content',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({stem:stem,slug:slug})})
.then(function(r){return r.json();})
.then(function(data){
if(data.ok){overlay.remove();}
else{errDiv.textContent=data.error||'Create failed';errDiv.style.display='block';submitBtn.disabled=false;}
})
.catch(function(err){errDiv.textContent='Network error: '+err.message;errDiv.style.display='block';submitBtn.disabled=false;});
};
slugInput.addEventListener('keydown',function(e){if(e.key==='Enter'){submitBtn.click();}if(e.key==='Escape'){overlay.remove();}});
});
}
var _suggestions=[];var _suggestIdx=0;var _suggestPreviewState=null;var _suggestActiveEl=null;
var _suggestToolbar=null;
function _suggestCleanup(){
document.querySelectorAll('[data-presemble-original-html]').forEach(function(el){
el.innerHTML=el.getAttribute('data-presemble-original-html');
el.removeAttribute('data-presemble-original-html');
});
document.querySelectorAll('.presemble-suggest-indicator,.presemble-suggest-active').forEach(function(el){
el.classList.remove('presemble-suggest-indicator','presemble-suggest-active','presemble-suggest-preview-active');
});
if(_suggestToolbar){_suggestToolbar.remove();_suggestToolbar=null;}
_suggestPreviewState=null;_suggestActiveEl=null;
_suggestions=[];_suggestIdx=0;
}
function _stripMd(s){return s.replace(/`/g,'').replace(/\*\*/g,'').replace(/\*/g,'').replace(/_/g,'');}
function _suggestFindTarget(sug){
if(sug.target_type==='slot'){
return document.querySelector('[data-presemble-slot="'+sug.slot+'"]');
}
if(sug.target_type==='body'&&sug.search){
var needle=_stripMd(sug.search);
var els=document.querySelectorAll('[data-presemble-slot="body"]');
for(var i=0;i<els.length;i++){if(els[i].textContent.indexOf(needle)!==-1){return els[i];}}
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
_suggestToolbar.querySelector('.presemble-suggest-reason').textContent=sug.reason||'';
var targetText=sug.target_type==='slot'?sug.slot:(sug.search?'"'+sug.search.substring(0,30)+'..."':'');
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
el.scrollIntoView({behavior:'smooth',block:'center'});
el.setAttribute('data-presemble-original-html',el.innerHTML);
if(sug.target_type==='slot'){
el.innerHTML='<del class="presemble-diff-del">'+el.textContent+'</del> <ins class="presemble-diff-ins">'+(sug.proposed_value||'')+'</ins>';
}else if(sug.target_type==='body'&&sug.search){
var html=el.innerHTML;
var searchEsc=sug.search.replace(/[&<>]/g,function(c){return{'&':'&amp;','<':'&lt;','>':'&gt;'}[c];});
var replaceEsc=(sug.replace||'').replace(/[&<>]/g,function(c){return{'&':'&amp;','<':'&lt;','>':'&gt;'}[c];});
var idx=html.indexOf(searchEsc);
if(idx!==-1){
el.innerHTML=html.slice(0,idx)+'<del class="presemble-diff-del">'+searchEsc+'</del><ins class="presemble-diff-ins">'+replaceEsc+'</ins>'+html.slice(idx+searchEsc.length);
}else{
var txt=el.textContent;
var needle=_stripMd(sug.search);
var tidx=txt.indexOf(needle);
if(tidx!==-1){
var before=txt.slice(0,tidx).replace(/[&<>]/g,function(c){return{'&':'&amp;','<':'&lt;','>':'&gt;'}[c];});
var after=txt.slice(tidx+needle.length).replace(/[&<>]/g,function(c){return{'&':'&amp;','<':'&lt;','>':'&gt;'}[c];});
var needleEsc=needle.replace(/[&<>]/g,function(c){return{'&':'&amp;','<':'&lt;','>':'&gt;'}[c];});
el.innerHTML=before+'<del class="presemble-diff-del">'+needleEsc+'</del><ins class="presemble-diff-ins">'+replaceEsc+'</ins>'+after;
}
}
}
}
_suggestRenderToolbar();
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
if(sug.target_type==='slot'){
el.textContent=sug.proposed_value||'';
}else if(sug.target_type==='body'&&sug.search&&sug.replace){
var origHtml=el.getAttribute('data-presemble-original-html')||el.innerHTML;
var searchEsc=sug.search.replace(/[&<>]/g,function(c){return{'&':'&amp;','<':'&lt;','>':'&gt;'}[c];});
var replaceEsc=sug.replace.replace(/[&<>]/g,function(c){return{'&':'&amp;','<':'&lt;','>':'&gt;'}[c];});
var idx=origHtml.indexOf(searchEsc);
if(idx!==-1){
el.innerHTML=origHtml.slice(0,idx)+replaceEsc+origHtml.slice(idx+searchEsc.length);
}else{
var origText=el.textContent;
var needle=_stripMd(sug.search);var replacement=_stripMd(sug.replace);
var tIdx=origText.indexOf(needle);
if(tIdx!==-1){el.textContent=origText.slice(0,tIdx)+replacement+origText.slice(tIdx+needle.length);}
}
}
el.classList.add('presemble-suggest-preview-active');
el.classList.remove('presemble-suggest-active');
if(previewBtn){previewBtn.classList.add('active');}
}
}
function _suggestAccept(){
if(_suggestions.length===0){return;}
var sug=_suggestions[_suggestIdx];
if(_suggestPreviewState){_suggestTogglePreview();}
var fileEl=document.querySelector('[data-presemble-file]');
var bfile=fileEl?fileEl.getAttribute('data-presemble-file'):'';
var editPromise;
if(sug.target_type==='slot'&&sug.slot&&sug.proposed_value){
editPromise=fetch('/_presemble/edit',{method:'POST',headers:{'Content-Type':'application/json'},
body:JSON.stringify({file:bfile,slot:sug.slot,value:sug.proposed_value})});
}else if(sug.target_type==='body'&&sug.search&&sug.replace){
var bodyEl=_suggestFindTarget(sug);
var bodyIdx=0;
if(bodyEl&&bodyEl.id){var m=bodyEl.id.match(/presemble-body-(\d+)/);if(m){bodyIdx=parseInt(m[1],10);}}
editPromise=fetch('/_presemble/edit-body',{method:'POST',headers:{'Content-Type':'application/json'},
body:JSON.stringify({file:bfile,body_idx:bodyIdx,
content:(bodyEl&&bodyEl.getAttribute('data-presemble-md')||'').replace(sug.search,sug.replace)})});
}else{editPromise=Promise.resolve({json:function(){return{ok:true};}});}
editPromise.then(function(r){return r.json();}).then(function(data){
if(!data.ok){alert(data.error||'Edit failed');return;}
return fetch('/_presemble/accept-suggestion',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({id:sug.id})});
}).then(function(r){if(r)return r.json();}).then(function(data){
if(data&&!data.ok){alert(data.error||'Accept failed');}
});
}
function _suggestReject(){
if(_suggestions.length===0){return;}
var sug=_suggestions[_suggestIdx];
if(_suggestPreviewState){_suggestTogglePreview();}
var el=_suggestFindTarget(sug);
if(el){el.classList.remove('presemble-suggest-indicator','presemble-suggest-active');}
fetch('/_presemble/reject-suggestion',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({id:sug.id})})
.then(function(r){return r.json();})
.then(function(data){if(!data.ok){alert(data.error||'Reject failed');}});
_suggestions.splice(_suggestIdx,1);
if(_suggestIdx>=_suggestions.length&&_suggestions.length>0){_suggestIdx=_suggestions.length-1;}
_suggestHighlight();
}
function _fetchSuggestionCount(){
var fileEl=document.querySelector('[data-presemble-file]');
if(!fileEl){return;}
var file=fileEl.getAttribute('data-presemble-file');
if(!file){return;}
fetch('/_presemble/suggestions?file='+encodeURIComponent(file))
.then(function(r){return r.json();})
.then(function(data){
if(!Array.isArray(data)){return;}
var cnt=data.length;
_editorialSuggestCount=cnt;
if(cnt>0){suggestBadge.textContent=cnt;suggestBadge.style.display='flex';}else{suggestBadge.style.display='none';}
update();
});
}
function _fetchDirtyCount(){
fetch('/_presemble/dirty-buffers')
.then(function(r){return r.json();})
.then(function(paths){
if(!Array.isArray(paths)){return;}
_dirtyCount=paths.length;
update();
if(_editToolbar){
var saveBtn=_editToolbar.querySelector('.presemble-edit-save');
if(saveBtn){
saveBtn.style.display=_dirtyCount>0?'':'none';
saveBtn.textContent='\u{1F4BE} Save ('+_dirtyCount+')';
}
}
})
.catch(function(){});
}
setInterval(_fetchDirtyCount,2000);
function _suggestEnter(){
var fileEl=document.querySelector('[data-presemble-file]');
if(!fileEl){return;}
var file=fileEl.getAttribute('data-presemble-file');
if(!file){return;}
fetch('/_presemble/suggestions?file='+encodeURIComponent(file))
.then(function(r){return r.json();})
.then(function(data){
if(!Array.isArray(data)){return;}
_suggestions=data;
_suggestIdx=0;
var cnt=data.length;
_editorialSuggestCount=cnt;
if(cnt>0){suggestBadge.textContent=cnt;suggestBadge.style.display='flex';}else{suggestBadge.style.display='none';}
data.forEach(function(sug){
var el=_suggestFindTarget(sug);
if(el){el.classList.add('presemble-suggest-indicator');}
});
_suggestHighlight();
});
}
function setMode(m){
if(m!=='edit'){cleanupEditing();_editCleanup();}
if(m!=='suggest'){_suggestCleanup();}
mode=m;
sessionStorage.setItem('presemble-mode',m);
menu.classList.remove('open');
update();
if(m==='edit'){_editEnter();}
if(m==='suggest'){_suggestEnter();}else{_fetchSuggestionCount();}
}
if(mode==='edit'){_editEnter();}
if(mode==='suggest'){_suggestEnter();}else{_fetchSuggestionCount();}
viewBtn.onclick=function(){setMode('view');};
editBtn.onclick=function(){setMode('edit');};
suggestBtn.onclick=function(){setMode('suggest');};
window._fetchDirtyCount=_fetchDirtyCount;
})();
document.addEventListener('click',function(e){
if(!document.body.classList.contains('presemble-edit-mode')){return;}
var el=e.target.closest('[data-presemble-slot]');
if(!el||el.classList.contains('presemble-editing')){return;}
var editing=document.querySelector('.presemble-editing');
if(editing){
var saveBtn=editing.parentNode.querySelector('.presemble-edit-toolbar .presemble-save');
if(saveBtn){saveBtn.click();}
}
var openTa=document.querySelector('.presemble-body-editor');
if(openTa){
var bSaveBtn=openTa.parentNode.querySelector('.presemble-edit-toolbar .presemble-save');
if(!bSaveBtn){bSaveBtn=openTa.nextElementSibling;if(bSaveBtn){bSaveBtn=bSaveBtn.querySelector('.presemble-save');}}
if(bSaveBtn){bSaveBtn.click();}
}
if(el.getAttribute('data-presemble-slot')==='body'){
e.preventDefault();
var bfile=el.getAttribute('data-presemble-file');
if(!bfile){var bfEl=document.querySelector('[data-presemble-file]');if(bfEl){bfile=bfEl.getAttribute('data-presemble-file');}}
var bidxAttr=el.id;
var bidx=0;
if(bidxAttr){var m=bidxAttr.match(/presemble-body-(\d+)/);if(m){bidx=parseInt(m[1],10);}}
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
function bcleanup(){
el.style.display='';
el.classList.remove('presemble-editing');
ta.remove();
btoolbar.remove();
var berr=el.parentNode&&el.parentNode.querySelector('.presemble-edit-error');
if(berr){berr.remove();}
}
function bsave(){
var bvalue=ta.value;
bcleanup();
if(bvalue===bmd){return;}
if(!bvalue.trim()){return;}
fetch('/_presemble/edit-body',{
method:'POST',
headers:{'Content-Type':'application/json'},
body:JSON.stringify({file:bfile,body_idx:bidx,content:bvalue})
}).then(function(r){return r.json();}).then(function(data){
if(!data.ok){
var berr2=document.createElement('div');
berr2.className='presemble-edit-error';
berr2.textContent=data.error||'Edit failed';
el.after(berr2);
el.style.display='';
}else{
if(window._fetchDirtyCount){window._fetchDirtyCount();}
}
}).catch(function(err){
var berr3=document.createElement('div');
berr3.className='presemble-edit-error';
berr3.textContent='Network error: '+err.message;
el.after(berr3);
el.style.display='';
});
}
btoolbar.querySelector('.presemble-save').onclick=function(ev){ev.stopPropagation();bsave();};
btoolbar.querySelector('.presemble-undo').onclick=function(ev){ev.stopPropagation();bcleanup();};
ta.addEventListener('keydown',function bkeyHandler(ev){
if(ev.key==='Escape'){bcleanup();ta.removeEventListener('keydown',bkeyHandler);}
});
return;
}
if(el.tagName==='IMG'){return;}
if(el.tagName==='A'&&!el.getAttribute('data-presemble-source-slot')){
e.preventDefault();
var afile=el.getAttribute('data-presemble-file');
var aslot=el.getAttribute('data-presemble-slot');
if(!afile||!aslot){return;}
var astem=afile.split('/')[1];
fetch('/_presemble/links?schema='+astem+'&slot='+aslot)
.then(function(r){return r.json();})
.then(function(options){
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
sel.onchange=function(){
if(sel.value){
fetch('/_presemble/edit',{
method:'POST',
headers:{'Content-Type':'application/json'},
body:JSON.stringify({file:afile,slot:aslot,value:sel.value})
}).then(function(r){return r.json();}).then(function(data){
sel.remove();
if(data.ok){setTimeout(function(){location.reload();},500);}
else{alert(data.error);}
});
}
};
sel.onblur=function(){setTimeout(function(){sel.remove();},200);};
function onKey(e){if(e.key==='Escape'){sel.remove();document.removeEventListener('keydown',onKey);}}
document.addEventListener('keydown',onKey);
});
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
function cleanup(){
el.contentEditable='false';
el.classList.remove('presemble-editing');
toolbar.remove();
var err=el.parentNode.querySelector('.presemble-edit-error');
if(err){err.remove();}
}
function save(){
var value=el.innerText.trim();
cleanup();
if(value===original){return;}
if(!value){return;}
fetch('/_presemble/edit',{
method:'POST',
headers:{'Content-Type':'application/json'},
body:JSON.stringify({file:pfile,slot:editSlot,value:value})
}).then(function(r){return r.json();}).then(function(data){
if(!data.ok){
var err=document.createElement('div');
err.className='presemble-edit-error';
err.textContent=data.error||'Edit failed';
el.after(err);
el.innerText=original;
}
if(window._fetchDirtyCount){window._fetchDirtyCount();}
}).catch(function(e){
var err=document.createElement('div');
err.className='presemble-edit-error';
err.textContent='Network error: '+e.message;
el.after(err);
el.innerText=original;
});
}
toolbar.querySelector('.presemble-save').onclick=function(e){e.stopPropagation();save();};
toolbar.querySelector('.presemble-undo').onclick=function(e){e.stopPropagation();el.innerText=original;cleanup();};
el.addEventListener('keydown',function handler(e){
if(e.key==='Enter'&&!e.shiftKey){e.preventDefault();save();el.removeEventListener('keydown',handler);}
if(e.key==='Escape'){el.innerText=original;cleanup();el.removeEventListener('keydown',handler);}
});
});
})();
