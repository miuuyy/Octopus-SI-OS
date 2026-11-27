const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');
const {test} = require('node:test');
const {JSDOM} = require('jsdom');
const React = require('react');
const {createRoot} = require('react-dom/client');
const rootPath=path.resolve(__dirname,'..'),cache=new Map(),settingsCommands=[];
function load(relative) {
 const file=path.resolve(rootPath,relative); if(cache.has(file))return cache.get(file).exports;
 const module={exports:{}}; cache.set(file,module);
 const source=ts.transpileModule(fs.readFileSync(file,'utf8'),{fileName:file,compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.CommonJS,jsx:ts.JsxEmit.ReactJSX,esModuleInterop:true}}).outputText;
 function local(specifier) {
  if(specifier.endsWith('.css'))return {};
  if(specifier.endsWith('design-system/components'))return {
   OcButton:({children,size,variant,...props})=>React.createElement('button',props,children),
   OcSelect:({options,label,onChange,placeholder,...props})=>React.createElement('select',{...props,'aria-label':label,onChange:e=>onChange(e.target.value)},options.map(o=>React.createElement('option',{key:o.value,value:o.value},o.label))),
   OcCard:({children})=>React.createElement('div',null,children),OcGroup:({title,children})=>React.createElement('div',null,title,children),
   OcSettingsRow:({label,description,control})=>React.createElement('div',null,label,description,control),OcStatus:({label})=>React.createElement('span',null,label),
   OcSwitch:({label,checked,disabled,onCheckedChange})=>React.createElement('button',{'aria-label':label,role:'switch','aria-checked':checked,disabled,onClick:()=>onCheckedChange(!checked)})
  };
  if(specifier.endsWith('clientPreferences'))return {useSetClientPreferences:()=>value=>settingsCommands.push(value)};
  if(specifier.endsWith('clientActions'))return {dispatchClientCommand:(...value)=>settingsCommands.push(value)};
  if(specifier.endsWith('IconGlyphs'))return {Glyph:()=>null};
  if(specifier.endsWith('shell/ShellTooltip'))return {ShellTooltip:({children})=>children};
  if(specifier.endsWith('SettingsHeading'))return {SettingsHeading:({title,description})=>React.createElement('h2',null,title,description)};
  if(specifier.endsWith('SavedPermissionsSection'))return {SavedApplicationPermissionsSection:()=>React.createElement('div',null,'Always-allowed apps')};
  if(specifier.endsWith('settingsCatalog'))return {settingsTargetProps:()=>({})};
  if(specifier.endsWith('RunEnvironmentSurfacePrimitives'))return {LiveSurfaceEmpty:({title,detail})=>React.createElement('div',null,title,detail),dataText:()=>'',ManualRoomControlWarning:({open,onContinue})=>open?React.createElement('button',{onClick:onContinue},'Confirm control'):null};
  if(!specifier.startsWith('.'))return require(specifier);
  const target=path.resolve(path.dirname(file),specifier); for(const suffix of ['', '.ts','.tsx'])if(fs.existsSync(target+suffix))return load(target+suffix);
  throw new Error(`Unknown test import ${specifier}`);
 }
 new Function('require','module','exports',source)(local,module,module.exports);return module.exports;
}
const {DesktopSurface}=load('src/components/worker-environment/DesktopSurface.tsx');
const {ComputerUseSettingsSection}=load('src/components/settings/ComputerUseSettingsSection.tsx');
const evidence=owner=>({status:'ready',permissions:{screen_capture:true,accessibility:true},control:{mode:'model',owner,generation:1},screens:[],observation:null});
const surfaceFor=room=>({id:'desktop',mode:'direct',viewerUrl:'http://localhost/view',readOnly:false,data:{native_presentations:[{source_id:room+'-source',worker_environment_id:room,run_id:room+'-run',app_instance_id:'actual-app',source_window_id:'actual-window',display_name:'Actual App',window_title:'Document',phase:'published',completion_status:null,error:null}]}});
function fixture() {
 const dom=new JSDOM('<div id="root"></div>',{url:'http://localhost',pretendToBeVisual:true}),w=dom.window;
 for(const key of ['window','document','HTMLElement','Event','MouseEvent','MutationObserver'])Object.defineProperty(globalThis,key,{configurable:true,value:key==='window'?w:w[key]});
 for(const key of ['getComputedStyle','requestAnimationFrame','cancelAnimationFrame'])globalThis[key]=w[key].bind(w);
 globalThis.ResizeObserver=class {observe(){}disconnect(){}};globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 const bounds={x:20,y:80,width:700,height:500,left:20,top:80,right:720,bottom:580};
 w.HTMLElement.prototype.getBoundingClientRect=function(){return this.matches('.we-desktop-application-view,[role="menu"]')?bounds:{x:0,y:0,width:0,height:0,left:0,top:0,right:0,bottom:0};};
 const root=createRoot(w.document.querySelector('#root')),tick=ms=>React.act(async()=>new Promise(resolve=>setTimeout(resolve,ms??65)));
 const button=label=>[...w.document.querySelectorAll('button')].find(b=>b.textContent===label||b.getAttribute('aria-label')===label);
 return {dom,w,root,tick,button,click:label=>React.act(async()=>{assert(button(label),label);button(label).dispatchEvent(new w.MouseEvent('click',{bubbles:true}));}),close:async()=>{await React.act(async()=>root.unmount());dom.window.close();}};
}

test('application pane hides native layer under overlays and detaches to the widget only on pane hide',async()=>{
 const f=fixture(),calls=[],views=[];let resolveAttach;
 f.w.octopusTransport={desktopSurfaceRequest:async request=>{calls.push(request);return request.operation==='pause_view'?{...request,paused:true}:{...request,evidence:evidence('room')};},desktopApplicationView:request=>{views.push(request);return request.operation==='attach'&&views.length===1?new Promise(resolve=>{resolveAttach=resolve;}):Promise.resolve({attached:request.operation==='attach'});}};
 const props={environment:{id:'room',runId:'room-run'},surface:surfaceFor('room')},render=active=>React.act(async()=>f.root.render(React.createElement(DesktopSurface,{...props,active})));
 try {
  await render(true);await f.tick();assert.equal(views[0].operation,'attach');assert.equal(views[0].appInstanceId,'actual-app');assert.equal(f.w.document.querySelector('img'),null);
  assert(calls.filter(c=>c.operation==='view').every(c=>c.screenId===undefined),'viewer reads status, never starts a whole-display stream');
  const overlay=f.w.document.createElement('div');overlay.setAttribute('role','menu');f.w.document.body.append(overlay);await f.tick();assert.equal(views.at(-1).operation,'hide');
  await React.act(async()=>resolveAttach({attached:true}));assert.equal(f.w.document.querySelector('[data-attached]'),null,'late attach acknowledgement cannot cover an overlay');
  overlay.remove();await f.tick();assert.equal(views.at(-1).operation,'attach');assert.equal(views.at(-1).viewId,views[0].viewId);assert(f.w.document.querySelector('[data-attached]'));
  await f.click('Computer use settings');assert.deepEqual(settingsCommands.slice(-2),[{settingsSection:'computer-use'},['open_settings','surface',{sectionId:'computer-use'}]]);
  await render(false);assert.equal(views.at(-1).operation,'detach');assert(calls.some(c=>c.operation==='pause_view'));
  await render(true);await f.tick();assert.equal(views.at(-1).operation,'attach');assert.notEqual(views.at(-1).viewId,views[0].viewId);
 } finally {await f.close();}
});

test('desktop controls reject stale room completions and allow Stop to preempt a pending control',async()=>{
 const f=fixture(),pending=[],calls=[];let room='a',value=evidence(room),fail=false;
 f.w.octopusTransport={desktopApplicationView:async r=>({attached:r.operation==='attach'}),desktopSurfaceRequest:async request=>{
  calls.push(request);if(request.operation==='pause_view')return {...request,paused:true};
  if(request.operation!=='view')return new Promise(resolve=>pending.push({request,resolve}));
  if(fail)throw new Error('stream disconnected');return {...request,evidence:structuredClone(value)};
 }};
 const render=()=>React.act(async()=>f.root.render(React.createElement(DesktopSurface,{environment:{id:room,runId:room+'-run'},surface:surfaceFor(room)})));
 try {
  await render();await f.tick();await f.click('Stop computer use');const old=pending.at(-1);
  room='b';value=evidence(room);await render();await f.tick();
  await React.act(async()=>old.resolve({evidence:{...evidence('a'),control:{mode:'stopped',owner:null,generation:99}}}));assert.equal(f.w.document.querySelector('[data-control]').dataset.control,'model');
  value={...value,control:{mode:'human',owner:null,generation:2}};await f.tick(550);await f.click('Release control');const release=pending.at(-1);
  assert.equal(release.request.controlGeneration,2);assert(!f.button('Stop computer use').disabled);await f.click('Stop computer use');const stop=pending.at(-1);
  const stopped={...value,control:{mode:'stopped',owner:null,generation:3}};
  await React.act(async()=>stop.resolve({evidence:stopped}));await React.act(async()=>release.resolve({evidence:evidence('b')}));
  assert.equal(f.w.document.querySelector('[data-control]').dataset.control,'stopped');assert.equal(f.w.document.querySelector('[data-attached]'),null);
  fail=true;room='c';value=evidence(room);await render();await f.tick();assert.match(f.w.document.body.textContent,/stream disconnected/);assert(!f.button('Stop computer use').disabled);
  await f.click('Stop computer use');await React.act(async()=>pending.at(-1).resolve({evidence:stopped}));assert.match(f.w.document.body.textContent,/Computer use stopped/);
 } finally {await f.close();}
});

test('upfront settings persists the real enable gate, requests setup only explicitly, and refetches OS status on focus',async()=>{
 const f=fixture(),calls=[];let enabled=false,ready=false;
 const status=()=>({enabled,ready,permissions:[{kind:'screen_capture',status:ready?'granted':'not_granted'},{kind:'accessibility',status:ready?'granted':'not_granted'}]});
 f.w.octopusTransport={desktopComputerUseSettings:async request=>{calls.push(request);if(request.operation==='set_enabled')enabled=request.enabled;return status();}};
 try {
  await React.act(async()=>f.root.render(React.createElement(ComputerUseSettingsSection)));await f.tick();assert.deepEqual(calls,[{operation:'status'}]);assert.equal(f.button('Enable Computer use').getAttribute('aria-checked'),'false');
  await f.click('Enable Computer use');await f.tick();assert.deepEqual(calls.slice(1,3),[{operation:'set_enabled',enabled:true},{operation:'setup'}]);assert.equal(f.button('Enable Computer use').getAttribute('aria-checked'),'true');
  ready=true;await React.act(async()=>f.w.dispatchEvent(new f.w.Event('focus')));await f.tick();assert.match(f.w.document.body.textContent,/Ready to use apps you approve/);
  const setupCount=calls.filter(c=>c.operation==='setup').length;await f.click('Enable Computer use');await f.tick();assert.equal(enabled,false);assert.equal(calls.filter(c=>c.operation==='setup').length,setupCount);
  await f.click('Enable Computer use');await f.tick();assert.equal(enabled,true);assert.equal(calls.filter(c=>c.operation==='setup').length,setupCount,'already granted OS access does not automatically request permission');
  assert.match(f.w.document.body.textContent,/Always-allowed apps/);
 } finally {await f.close();}
});
