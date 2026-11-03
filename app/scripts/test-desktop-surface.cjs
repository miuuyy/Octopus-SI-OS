const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { createHash, randomUUID } = require('node:crypto');
const { test } = require('node:test');
const { createDesktopSurfaceHost } = require('../electron/desktopSurface.cjs');
const screen = {id:'left',x:-960,y:0,width:960,height:540,pixel_width:1920,pixel_height:1080};
const imageBytes = Buffer.alloc(128 * 1024, 17);
const image = {media_type:'image/jpeg',data_base64:imageBytes.toString('base64'),sha256:createHash('sha256').update(imageBytes).digest('hex'),byte_length:imageBytes.length,width:1920,height:1080};
const state = () => ({status:'ready',permissions:{screen_capture:true,accessibility:true},control:{mode:'model',owner:'room-a',generation:1},screens:[screen],observation:{id:'frame-1',screen,focus:{app_id:'example.app',process_id:5,window_id:7},input_revision:'1',captured_at_unix_ms:1,image}});
const input = (viewId, operation='view') => ({workerEnvironmentId:'room-a',surfaceId:'desktop',runId:'run-a',viewId,operation,screenId:'left'});
async function fixture(timeout=1000, native=false) {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(),'octopus-B2-bridge-')));
  const dir = path.join(root,'registry/interface/system_gui'); await fs.mkdir(dir,{recursive:true});
  const config = {schema_version:1,viewer_url:'http://127.0.0.1:8766/view',action_url:'http://127.0.0.1:8766/v1/action',auth_header:'Authorization',auth_value_prefix:'Bearer ',auth_env_key:'DESKTOP_TEST_SECRET',request_timeout_ms:timeout,max_response_bytes:97871192};
  const secret = native ? 'a'.repeat(64) : randomUUID();
  const manifest = {schema_version:1,interface:'system_gui',host_generation:'b'.repeat(32),action_url:config.action_url,viewer_url:config.viewer_url,token:secret};
  const connection = path.join(root,'interfaces/system_gui/desktop-host/connection.json');
  if(native) {
   delete config.auth_env_key; config.action_url='http://127.0.0.1:0/v1/action'; config.viewer_url='http://127.0.0.1:0/view'; config.headers={}; config.host={connection_file:'desktop-host/connection.json'};
   await fs.mkdir(path.dirname(connection),{recursive:true,mode:0o700}); await fs.chmod(path.join(root,'interfaces/system_gui'),0o700);
   await fs.writeFile(connection,JSON.stringify(manifest),{mode:0o600});
  }
  await fs.writeFile(path.join(dir,'desktop.json'),JSON.stringify(config));
  let descriptor = {runId:'run-a',environmentKind:'desktop_session',protocol:'desktop_session',mode:'direct',doorInterfaceId:'system_gui',viewerUrl:native?manifest.viewer_url:config.viewer_url,formationRevision:'one',readOnly:false,...(native?{data:{host_generation:manifest.host_generation}}:{})};
  const calls=[]; let responseBytes=0; let next = () => state(); let failPause=false; let pendingAdmission;
  const host = createDesktopSurfaceHost({homePath:()=>root,environment:()=>{if(native)throw new Error('Native must never inspect ambient credentials');return {DESKTOP_TEST_SECRET:secret};},resolveSurface:async()=> { if(pendingAdmission) await pendingAdmission; return descriptor; },fetch:async(url,request)=>{
    const body=JSON.parse(request.body); calls.push(body);
    assert.equal(request.headers.Authorization,`Bearer ${secret}`); assert(!String(url).includes(secret));
    if(failPause && body.action.type==='pause_view') throw new Error('detach transport failed');
    const evidence=next(body,request);
    if(evidence instanceof Response) return evidence;
    if(body.action.type==='view' && evidence.observation?.image && body.action.known_image_sha256===evidence.observation.image.sha256) { evidence.view_image_ref={sha256:evidence.observation.image.sha256}; evidence.observation={...evidence.observation,image:null}; }
    const raw=JSON.stringify({schema_version:2,worker_environment_id:body.worker_environment_id,request_id:body.request_id,operation:body.action.type,accepted:true,evidence});
    responseBytes+=Buffer.byteLength(raw); return new Response(raw);
  }});
  return {host,calls,config,root,dir,secret,connection,manifest,setDescriptor:v=>descriptor=v,descriptor:()=>descriptor,setNext:v=>next=v,setFailPause:v=>failPause=v,setAdmission:v=>pendingAdmission=v,bytes:()=>responseBytes,close:()=>fs.rm(root,{recursive:true,force:true})};
}

test('native manifest binds private exact door, endpoint and generation without ambient credentials',async()=>{
 const f=await fixture(1000,true);
 try {
  await fs.chmod(path.join(f.root,'interfaces'),0o755);
  const settings = { workerEnvironmentId:'room-a', surfaceId:'desktop', runId:'run-a', hostGeneration:f.manifest.host_generation, operation:'list' };
  f.setNext(body => new Response(JSON.stringify({schema_version:2,worker_environment_id:body.worker_environment_id,request_id:body.request_id,operation:'application_permissions',accepted:true,
    application_result:{type:'application_permissions',host_generation:f.manifest.host_generation,applications:body.action.operation==='list'?[{bundle_id:'example.app',display_name:'Example'}]:[],removed:body.action.operation==='remove'}})));
  const saved = await f.host.applicationPermissions(settings, () => {}); assert.equal(saved.applications[0].bundle_id,'example.app');
  assert.equal(f.calls.at(-1).action.host_generation,f.manifest.host_generation);
  assert.equal((await f.host.applicationPermissions({...settings,operation:'remove',bundleId:'example.app'}, () => {})).removed,true);
  const countBeforeRefusal=f.calls.length;
  await assert.rejects(f.host.applicationPermissions({...settings,hostGeneration:'old'}, () => {}), /current native host/);
  await assert.rejects(f.host.applicationPermissions(settings, () => {throw new Error('access withdrawn');}), /withdrawn/);
  assert.equal(f.calls.length,countBeforeRefusal,'stale settings never send a bearer request');
  const prefs=path.join(f.root,'preferences'), otherPrefs=path.join(f.root,'separate-preferences');
  for(const directory of [prefs,otherPrefs]) await fs.mkdir(directory,{mode:0o700});
  await fs.writeFile(path.join(f.root,'host.json'),JSON.stringify({host_manifest_version:1,root_refs:{desktop_app_preferences_root:prefs}}));
  const projected=new Map(), transports=new Map(); let currentHome=f.root, failSettings=false;
  async function addOwner(door,room,opened,store,port) {
   const directory=path.join(f.root,'registry/interface',door), privateRoot=path.join(f.root,'interfaces',door,'desktop-host');
   await fs.mkdir(directory,{recursive:true});await fs.mkdir(privateRoot,{recursive:true,mode:0o700});await fs.chmod(path.dirname(privateRoot),0o700);
   const descriptor={id:door,interface_driver_ref:'system_gui',...(door!=='system_gui'?{interface_driver_config:{host_slots:{desktop_app_preferences_root:{path:store}}}}:{})};
   await fs.writeFile(path.join(directory,'interface.json'),JSON.stringify(descriptor));
   await fs.writeFile(path.join(directory,'desktop.json'),JSON.stringify({...f.config,host:{...f.config.host,application_preferences:{root_slot:'desktop_app_preferences_root'}}}));
   const manifest={...f.manifest,interface:door,host_generation:createHash('sha256').update(door).digest('hex').slice(0,32),action_url:`http://127.0.0.1:${port}/v1/action`,viewer_url:`http://127.0.0.1:${port}/view`};
   const connection=path.join(privateRoot,'connection.json');await fs.writeFile(connection,JSON.stringify(manifest),{mode:0o600});
   projected.set(room,{...f.descriptor(),runId:`run-${room}`,doorInterfaceId:door,viewerUrl:manifest.viewer_url,data:{host_generation:manifest.host_generation}});
   transports.set(room,{manifest,connection,store});
   return {worker_environment_id:room,run_id:`run-${room}`,worker_environment_kind:'desktop_session',door,lifecycle:'active',opened_at_movement_seq:opened,
    client_surfaces:[{id:'desktop',protocol:'desktop_session',mode:'direct',data:{host_generation:manifest.host_generation}}]};
  }
  const homeOwner=await addOwner('system_gui','home',10,prefs,8766), taskOwner=await addOwner('system_gui_task','task',20,prefs,8767);
  const otherOwner=await addOwner('system_gui_other','other',30,otherPrefs,8768);
  let rows=[otherOwner,taskOwner,homeOwner]; const preferenceCalls=[];
  const globalSettings=createDesktopSurfaceHost({homePath:()=>currentHome,resolveSurface:async id=>projected.get(id),
   withDefaultPreferenceDriver:operation=>operation('system_gui'),resolvePreferenceEnvironments:async()=>rows,
   fetch:async(url,request)=>{
    const body=JSON.parse(request.body), owner=transports.get(body.worker_environment_id);preferenceCalls.push(body);
    assert.equal(new URL(url).origin,new URL(owner.manifest.action_url).origin);assert.equal(request.headers.Authorization,`Bearer ${owner.manifest.token}`);
    assert.equal(body.action.host_generation,owner.manifest.host_generation);assert.equal(owner.store,prefs,'unrelated configured stores never supply global Settings');
    if(failSettings)throw new Error('settings outcome unavailable');
    return new Response(JSON.stringify({schema_version:2,worker_environment_id:body.worker_environment_id,request_id:body.request_id,operation:'application_permissions',accepted:true,
     application_result:{type:'application_permissions',host_generation:owner.manifest.host_generation,applications:[{bundle_id:'example.app',display_name:'Example'}],removed:body.action.operation==='remove'}}));
   }});
  const current=()=>{assert.equal(currentHome,f.root,'Home changed');};
  const listed=await globalSettings.defaultApplicationPermissions({operation:'list'},current);
  assert.deepEqual(Object.keys(listed).sort(),['applications','removed','sourceId']);assert.equal(listed.applications[0].display_name,'Example');
  assert.equal(preferenceCalls.at(-1).worker_environment_id,'home','canonical owner wins across equivalent Home/task hosts');
  rows.reverse();assert.equal((await globalSettings.defaultApplicationPermissions({operation:'list'},current)).sourceId,listed.sourceId);
  assert.equal((await globalSettings.defaultApplicationPermissions({operation:'remove',sourceId:listed.sourceId,bundleId:'example.app'},current)).removed,true);
  let sent=preferenceCalls.length;currentHome=path.join(f.root,'different-home');
  await assert.rejects(globalSettings.defaultApplicationPermissions({operation:'remove',sourceId:listed.sourceId,bundleId:'example.app'},current),/Home changed/);
  assert.equal(preferenceCalls.length,sent);currentHome=f.root;
  failSettings=true;await assert.rejects(globalSettings.defaultApplicationPermissions({operation:'remove',sourceId:listed.sourceId,bundleId:'example.app'},current),/outcome unavailable/);
  assert.equal(preferenceCalls.length,sent+1,'unknown effect never retries another equivalent host');failSettings=false;
  rows=[otherOwner,taskOwner];sent=preferenceCalls.length;
  await assert.rejects(globalSettings.defaultApplicationPermissions({operation:'remove',sourceId:listed.sourceId,bundleId:'example.app'},current),/source changed/);
  assert.equal(preferenceCalls.length,sent,'a list pin cannot retarget to another live owner');
  const taskList=await globalSettings.defaultApplicationPermissions({operation:'list'},current);assert.equal(preferenceCalls.at(-1).worker_environment_id,'task');
  const task=transports.get('task');task.manifest.host_generation='c'.repeat(32);await fs.writeFile(task.connection,JSON.stringify(task.manifest));
  projected.get('task').data.host_generation=task.manifest.host_generation;taskOwner.client_surfaces[0].data.host_generation=task.manifest.host_generation;sent=preferenceCalls.length;
  await assert.rejects(globalSettings.defaultApplicationPermissions({operation:'remove',sourceId:taskList.sourceId,bundleId:'example.app'},current),/source changed/);assert.equal(preferenceCalls.length,sent);
  rows=[otherOwner];await assert.rejects(globalSettings.defaultApplicationPermissions({operation:'list'},current),/unavailable/);assert.equal(preferenceCalls.length,sent);
  const {createNativeDesktopSettingsHost}=require('../electron/nativeDesktopSettings.cjs');const {EventEmitter}=require('node:events'),{PassThrough,Writable}=require('node:stream');
  let dispatched=0;const settingsOwner={id:17,identity:'operator',current};
  const cold=createNativeDesktopSettingsHost({homePath:()=>currentHome,prepareHelper:async()=>{
   const selected=await globalSettings.defaultPreferenceStore(current);assert.equal(typeof selected.identity,'string');assert.equal(typeof selected.generation,'string');
   return {home:currentHome,preferenceRoot:selected.root,applicationIdentity:JSON.stringify([selected.identity,selected.generation]),current:selected.current,settingsProgram:'/installed/reader',directory:'/installed',timeout:1000};
  },spawn(){++dispatched;const child=new EventEmitter();child.stdout=new PassThrough();child.stderr=new PassThrough();child.kill=()=>{};
   child.stdin=new Writable({write(_bytes,_encoding,done){done();queueMicrotask(()=>{child.stdout.end('{"schema_version":1,"applications":[],"removed":false}');child.emit('close',0);});}});return child;}});
  try {
   const first=await cold.applicationPermissions({operation:'list'},settingsOwner),store=await globalSettings.defaultPreferenceStore(current);
   await fs.rename(prefs,prefs+'-old');await fs.mkdir(prefs,{mode:0o700});
   await assert.rejects(store.current(),/changed/);
   await assert.rejects(cold.applicationPermissions({operation:'remove',sourceId:first.sourceId,bundleId:'example.app'},settingsOwner),/source changed/);assert.equal(dispatched,1);
   const replaced=await cold.applicationPermissions({operation:'list'},settingsOwner);assert.notEqual(replaced.sourceId,first.sourceId);
   const configFile=path.join(f.root,'registry/interface/system_gui/desktop.json'),configured=JSON.parse(await fs.readFile(configFile));configured.request_timeout_ms+=1;await fs.writeFile(configFile,JSON.stringify(configured));
   await assert.rejects(cold.applicationPermissions({operation:'remove',sourceId:replaced.sourceId,bundleId:'example.app'},settingsOwner),/source changed/);assert.equal(dispatched,2);
  } finally {cold.stop();}
  await fs.writeFile(f.connection,JSON.stringify(f.manifest));await fs.writeFile(path.join(f.dir,'desktop.json'),JSON.stringify(f.config));
  f.setNext(() => state());
  const viewed=await f.host.request(input('native'),40);assert(!JSON.stringify(viewed).includes(f.secret));
  await fs.writeFile(path.join(path.dirname(f.connection),'unrelated-state.json'),'{}');
  await f.host.request(input('native'),40);await f.host.request(input('native','pause_view'),40);
  for(const patch of [{interface:'other'},{host_generation:'c'.repeat(32)},{token:[f.secret]},{viewer_url:'http://127.0.0.1:9999/view'},{action_url:'http://example.test:8766/v1/action'},{action_url:'http://127.0.0.1/v1/action'}]) {
   await fs.writeFile(f.connection,JSON.stringify({...f.manifest,...patch}));const count=f.calls.length;
   await assert.rejects(f.host.request(input(randomUUID()),41));assert.equal(f.calls.length,count);
  }
  await fs.writeFile(f.connection,JSON.stringify(f.manifest));
  await fs.chmod(f.connection,0o644);await assert.rejects(f.host.request(input('public'),41),/mode 0600/);await fs.chmod(f.connection,0o600);
  await fs.chmod(path.dirname(f.connection),0o755);await assert.rejects(f.host.request(input('public-parent'),41),/0700/);await fs.chmod(path.dirname(f.connection),0o700);
  const link=path.join(f.root,'hardlink');await fs.link(f.connection,link);await assert.rejects(f.host.request(input('linked'),41),/single-link/);await fs.unlink(link);
  await fs.rename(f.connection,link);await fs.symlink(link,f.connection);await assert.rejects(f.host.request(input('symlink'),41));await fs.unlink(f.connection);await fs.rename(link,f.connection);
  await fs.writeFile(f.connection,' '.repeat(4097));await assert.rejects(f.host.request(input('oversized'),41),/4096/);
  await fs.writeFile(f.connection,JSON.stringify(f.manifest));
  await f.host.request(input('rotation'),42);const count=f.calls.length;
  await fs.writeFile(f.connection,JSON.stringify({...f.manifest,host_generation:'c'.repeat(32)}));
  await assert.rejects(f.host.request(input('rotation'),42),AggregateError);assert.equal(f.calls.length,count,'rotation must not send stale bearer even for cleanup');
 } finally {await f.close();}
});

test('exact owner, pending-start close, retained failed detach and captured-route cleanup',async()=>{
 const f=await fixture();
 try {
  await assert.rejects(f.host.request({...input('malformed'),extra:true},1),/Invalid/);
  await f.host.closeOwner(1); assert.equal(f.calls.length,0);
  await f.host.request(input('shared'),2);
  await assert.rejects(f.host.request(input('shared'),3),/another owner/);
  await assert.rejects(f.host.request(input('shared','pause_view'),3),/another owner/);
  await assert.rejects(f.host.request({...input('shared'),runId:'elsewhere'},2),/another owner/);
  assert.equal(f.calls.filter(c=>c.action.type==='pause_view').length,0);
  assert.notEqual(f.calls[0].action.view_id,'shared');
  f.setFailPause(true);
  await assert.rejects(f.host.request(input('shared','pause_view'),2),/detach transport/);
  f.setDescriptor(null);
  await assert.rejects(f.host.closeOwner(2),AggregateError);
  f.setFailPause(false); await f.host.closeOwner(2);
  assert.equal(f.calls.filter(c=>c.action.type==='pause_view').length,3);
  f.setDescriptor({runId:'run-a',environmentKind:'desktop_session',protocol:'desktop_session',mode:'direct',doorInterfaceId:'system_gui',viewerUrl:f.config.viewer_url,formationRevision:'one',readOnly:false});
  const beforePending=f.calls.length;
  let admit; f.setAdmission(new Promise(r=>admit=r));
  const opening=f.host.request(input('late'),4); const rejected=assert.rejects(opening,/closed|room/);
  const closing=f.host.closeOwner(4); admit(); await rejected; await closing;
  assert.equal(f.calls.length,beforePending);
  await f.host.request(input('before-logout'),5); await f.host.pauseOwner(5);
  await assert.rejects(f.host.request(input('before-logout'),5),/closed/);
  await f.host.request(input('after-login'),5); await f.host.closeOwner(5);
  await assert.rejects(f.host.request(input('after-destroy'),5),/owner is unavailable/);
 } finally { await f.close(); }
});

test('unchanged frame bytes shrink while control updates survive; config generation revokes attachment',async()=>{
 const f=await fixture(); let jsonReads=0;
 const original=fs.readFile; fs.readFile=async(...args)=>{if(String(args[0]).startsWith(f.root))jsonReads++;return original(...args);};
 try {
  const request=input('frames'); const first=await f.host.request(request,10); const firstBytes=f.bytes();
  assert.equal(first.evidence.observation.image.sha256,image.sha256);
  const start=performance.now(); let ipcBytes=0;
  for(let i=0;i<100;i++) {
   f.setNext(()=>{const v=state();v.control={mode:'human',owner:null,generation:i+2};return v;});
   const result=await f.host.request(request,10); ipcBytes+=Buffer.byteLength(JSON.stringify(result));
   assert.equal(result.evidence.control.mode,'human'); assert.equal(result.evidence.observation.image,null); assert.equal(result.evidence.view_image_ref.sha256,image.sha256);
  }
  assert.equal(jsonReads,1); assert(f.bytes()-firstBytes<firstBytes*2);
  assert(!JSON.stringify(first).includes(f.secret));
  console.log(JSON.stringify({fixtureFrameBytes:imageBytes.length,unchangedReads:100,configurationJsonReads:jsonReads,fullFrameResponseBytes:firstBytes,unchangedHttpBytes:f.bytes()-firstBytes,unchangedIpcBytes:ipcBytes,elapsedMs:Math.round(performance.now()-start)}));
  await fs.writeFile(path.join(f.dir,'desktop.json'),JSON.stringify({...f.config,request_timeout_ms:900}));
  await assert.rejects(f.host.request(request,10),/configuration changed/);
  assert.equal(f.calls.at(-1).action.type,'pause_view');
  await f.host.closeOwner(10);
 } finally {fs.readFile=original; await f.close();}
});

test('declared deadline includes streaming body and refusal cannot become success',async()=>{
 const f=await fixture(30);
 try {
  f.setNext((body,request)=>{
   if(body.action.type==='pause_view')return state();
   return new Response(new ReadableStream({start(controller){request.signal.addEventListener('abort',()=>controller.error(request.signal.reason),{once:true});}}));
  });
  const before=performance.now(); await assert.rejects(f.host.request(input('hung'),20),/declared request timeout/);
  assert(performance.now()-before<1000);
  assert.equal(f.calls.at(-1).action.type,'pause_view'); await f.host.closeOwner(20);
  f.setNext((body)=>new Response(JSON.stringify({schema_version:2,worker_environment_id:body.worker_environment_id,request_id:body.request_id,operation:body.action.type,accepted:true,error:{code:'bad',message:'failed'},evidence:state()})));
  await assert.rejects(f.host.request(input('contradiction'),21),AggregateError);
 } finally {await f.close();}
});

test('Stop admits the current room without reopening a failed view and forwards exact control generations',async()=>{
 const f=await fixture();
 const stopped=()=>({...state(),observation:null,control:{mode:'stopped',owner:null,generation:3}});
 try {
  const view=input('broken'); await f.host.request(view,30);
  f.setNext(body=>{ if(body.action.type==='view')throw new Error('stream lost');return state(); });
  await assert.rejects(f.host.request(view,30),/stream lost/);
  const before=f.calls.length; f.setNext(()=>stopped());
  const result=await f.host.request({...view,operation:'stop'},30);
  assert.equal(result.evidence.control.mode,'stopped'); assert.deepEqual(f.calls.slice(before).map(c=>c.action.type),['stop']);
  await assert.rejects(f.host.request(view,30),/closed/);
  await assert.rejects(f.host.request({...view,operation:'stop'},31),/another owner/);
  const current=f.descriptor(); f.setDescriptor({...current,readOnly:true});
  await assert.rejects(f.host.request(input('new-stop','stop'),30),/read-only/); f.setDescriptor(current);
  f.setNext(()=>state()); await f.host.request(input('control'),30);
  await assert.rejects(f.host.request(input('control','release_control'),30),/observed generation/);
  await f.host.request({...input('control','release_control'),controlGeneration:3},30);
  assert.equal(f.calls.at(-1).action.control_generation,3);
  let admission; f.setAdmission(new Promise(resolve=>admission=resolve));
  const pending=f.host.request(input('late-stop','stop'),30); const denied=assert.rejects(pending,/owner changed/);
  await f.host.pauseOwner(30); const last=f.calls.length; admission(); await denied; assert.equal(f.calls.length,last);
 } finally {await f.close();}
});

test('native PiP bootstrap retains the exact private source and rejects generation or source changes before install', async () => {
 const f=await fixture(1000,true);
 const source={scope:{host_generation:f.manifest.host_generation,worker_environment_id:'room-a',formation_pin:'formed'},grant_request_id:'approved',
  application:{app:{host_generation:f.manifest.host_generation,app_instance_id:'native-app'},process_id:123,bundle_id:'test.app',bundle_path:'/Applications/Fixture.app',display_name:'Fixture',launch_time_unix_ms:1},
  source:{tool:'gui.app_state',correlation_id:'call1',source_movement_seq:12,source_artifact:{id:'artifact1',content:Array(32).fill(2)}},windows:[{window_id:'w1',cg_window_id:7,source_image:{sha256:'c'.repeat(64)}}]};
 const descriptor={...f.descriptor(),lifecycle:'active',formationPin:'formed',data:{...f.descriptor().data,desktop:{applications:[source]}}};f.setDescriptor(descriptor);
 const request={workerEnvironmentId:'room-a',surfaceId:'desktop',runId:'run-a',appInstanceId:'native-app',sourceWindowId:'w1'};
 const nonce='d'.repeat(64),instance='e'.repeat(64),key=Buffer.alloc(32,17);let mutate=false;
 try {
  f.setNext(body=>{
   if(body.action.type==='presentation_complete'){assert.equal(body.action.scope.grant_request_id,'approved');assert.equal(body.action.presentation_id,'accepted-publication');assert.equal(body.action.receiver_instance_nonce,instance);return new Response(JSON.stringify({schema_version:2,worker_environment_id:body.worker_environment_id,request_id:body.request_id,operation:'presentation_complete',accepted:true,error:null,presentation_result:{schema_version:1,status:'ended',target:((({type,...target})=>target)(body.action))}}));}
   assert.equal(body.action.type,'receiver_bootstrap');assert.equal(body.tool,'operator.desktop');assert.equal(body.action.receiver_pid,process.pid);assert.equal(body.action.scope.grant_request_id,'approved');
   const binding={schema_version:1,bootstrap_id:'f'.repeat(32),request_id:body.request_id,scope:body.action.scope};
   if(mutate)f.setDescriptor({...descriptor,data:{...descriptor.data,desktop:{applications:[{...source,grant_request_id:'withdrawn'}]}}});
   return new Response(JSON.stringify({schema_version:2,worker_environment_id:body.worker_environment_id,request_id:body.request_id,operation:'receiver_bootstrap',accepted:true,error:null,evidence:null,
    receiver_bootstrap:{schema_version:1,bootstrap_id:binding.bootstrap_id,bootstrap_key_base64:key.toString('base64'),binding_bytes_base64:Buffer.from(JSON.stringify(binding)).toString('base64')}}));
  });
  const prepared=await f.host.preparePresentationSource(request);
  const response=await prepared.bootstrap({requestId:'http-request',receiverPid:process.pid,receiverNonce:nonce,receiverInstanceNonce:instance},new AbortController().signal);
  assert.deepEqual(response.key,key);assert.equal(JSON.parse(response.bindingBytes).request_id,'http-request');response.key.fill(0);
  await assert.rejects(f.host.preparePresentationSource({...request,sourceWindowId:'foreign'}),/window/);
  const completed=await prepared.complete({presentationId:'accepted-publication',streamGeneration:1,receiverInstanceNonce:instance});assert.equal(completed.status,'ended');assert.equal(completed.target.source_window_id,request.sourceWindowId);
  mutate=true;await assert.rejects(prepared.bootstrap({requestId:'withdrawn-request',receiverPid:process.pid,receiverNonce:nonce,receiverInstanceNonce:instance}),/withdrawn|replaced/);
  f.setDescriptor(descriptor);const captured=await f.host.preparePresentationSource(request);const calls=f.calls.length;
  await fs.writeFile(f.connection,JSON.stringify({...f.manifest,host_generation:'c'.repeat(32)}));await assert.rejects(captured.current(),/changed/);assert.equal(f.calls.length,calls);
 } finally {await f.close();}
});

test('native PiP keeps multiple sources through tool-idle and completes only the recorded whole turn', async () => {
 const {createDesktopPresentationHost}=require('../electron/desktopSurface.cjs');let event,home='/actual-home',allowed=true;const calls=[],buffers=[],nonces=new Map();
 const fake={beginSource(v){calls.push(['begin',v]);const nonce=require('node:crypto').randomBytes(32).toString('hex');nonces.set(v.requestId,nonce);return{requestId:v.requestId,receiverPid:process.pid,receiverNonce:nonce,receiverInstanceNonce:'b'.repeat(64)};},
  installSource(v){buffers.push(v.key);calls.push(['install',v.requestId]);event({type:'source-authenticated',requestId:v.requestId,receiverNonce:nonces.get(v.requestId)});},
  cancelSource(v){calls.push(['cancel',v.requestId]);},embedSource(v){calls.push(['embed',v]);},detachEmbeddedSource(v){calls.push(['detach',v]);},hideEmbeddedSource(v){calls.push(['hide',v]);},completeGroup(v){calls.push(['complete',v.groupId]);},clearCompletedGroup(v){calls.push(['clear',v.groupId]);},setSelection(){},stop(){calls.push(['stop']);}};
  const helper={process_id:77,executable_path:'/helper/bin',display_name:'helper',bundle:null,responsible_app:null};
 const origin={api:'AEDeterminePermissionToAutomateTarget',event_class:'OcPi',event_id:'BtV1',
  sender:{...helper,process_id:process.pid,executable_path:process.execPath,start_seconds:'1',start_microseconds:'0'},recipient:{...helper,start_seconds:'2',start_microseconds:'0'}};
 let automationStatus='not_determined',pendingSettings=null;
 const permission=()=>({id:'native-opaque-automation',kind:'automation',title:'Native presentation connection',status:automationStatus,can_request:automationStatus==='not_determined',can_open_settings:true,detail:'Native observed fixture',provenance:origin});
 fake.prepareAutomationPermission=()=>permission();
 fake.cancelAutomationSettings=value=>calls.push(['cancel-settings',value]);
 fake.beginAutomationPermission=value=>{assert.equal(value.timeoutMs,1000);calls.push(['permission',value]);if(value.operation==='open_settings'){pendingSettings=value;return;}if(value.operation==='request')automationStatus='granted';event({type:'automation-permission-result',requestId:value.requestId,operationId:value.operationId,requirement:permission(),error:null});};
 const candidates=['w1','w2'].map(sourceWindowId=>({input:{workerEnvironmentId:'room',surfaceId:'desktop',runId:'run',appInstanceId:'app',sourceWindowId},
  source:{scope:{formation_pin:'formation'},grant_request_id:'grant',application:{display_name:'Fixture'},windows:[{window_id:sourceWindowId,title:'Window'}],source:{source_movement_seq:12}},identity:{sourceWindowId,grant:'grant'}}));
 let pendingSourceRead=null;
 const host=createDesktopPresentationHost({homePath:()=>home,requireAccess:()=>{if(!allowed)throw new Error('locked');},getNativeHost:cb=>{event=cb;return fake;},
  prepareSource:async input=>({home,timeout:1000,scope:{grant_request_id:'grant'},source:candidates.find(c=>c.input.sourceWindowId===input.sourceWindowId).source,current:async()=>{if(pendingSourceRead){const pending=pendingSourceRead;pendingSourceRead=null;await pending;}},complete:async publication=>{calls.push(['complete-source',input.sourceWindowId,publication]);return{status:'ended'};},
    bootstrap:async pending=>({key:Buffer.alloc(32,8),bindingBytes:Buffer.from(JSON.stringify({host_generation:'host',presentation_id:pending.requestId,stream_generation:1}))})})});
 const flush=()=>new Promise(resolve=>setImmediate(resolve));
 const turn={run_id:'run',turn_open:true,turn_state:'working',turn_opened_movement_seq:10};
 await host.reconcile(candidates,[turn]);await flush();assert.equal(calls.filter(c=>c[0]==='install').length,2);assert(buffers.every(b=>b.every(v=>v===0)));
 const sourceRequest=calls.find(c=>c[0]==='begin')[1].requestId;
 event({type:'published',identity:{hostGeneration:'host',presentationId:sourceRequest,streamGeneration:1}});
 const owner={id:123,current:()=>{if(!allowed)throw new Error('locked');},hostIdentity:()=>({hostId:'actual-window',generation:'window-generation'}),windowHandle:()=>Buffer.alloc(8)};
 const view={workerEnvironmentId:'room',surfaceId:'desktop',runId:'run',viewId:'view',operation:'attach',appInstanceId:'app',sourceWindowId:'w1',rect:{x:20,y:30,width:500,height:300}};
 assert.deepEqual(await host.applicationView(view,owner),{attached:true});
 assert.equal(calls.at(-1)[1].requestId,sourceRequest);assert.equal(calls.at(-1)[1].hostId,'actual-window');
 const attached=calls.at(-1)[1], count=calls.length;
 await assert.rejects(host.applicationView(view,{...owner,id:124}),/another owner/);assert.equal(calls.length,count);
 await assert.rejects(host.applicationView({...view,appInstanceId:'foreign'},owner),/not currently published/);
 assert.equal(calls.filter(c=>c[0]==='begin').length,2,'embedding reuses the same authenticated source');
 await host.applicationView({...view,rect:{...view.rect,width:550}},owner);assert.equal(calls.at(-1)[1].viewGeneration,attached.viewGeneration);
 await host.applicationView({workerEnvironmentId:'room',surfaceId:'desktop',runId:'run',viewId:'view',operation:'hide'},owner);
 assert.deepEqual(calls.at(-1),['hide',{viewId:'view',viewGeneration:attached.viewGeneration}]);
 await host.applicationView(view,owner);assert.equal(calls.at(-1)[1].viewGeneration,attached.viewGeneration);
 host.closeViewsOwner(124);assert.equal(calls.at(-1)[0],'embed');host.closeViewsOwner(123);assert.equal(calls.at(-1)[0],'detach');
 assert.deepEqual(calls.at(-1)[1],{viewId:'view',viewGeneration:attached.viewGeneration});
 let releaseSourceRead; pendingSourceRead=new Promise(resolve=>{releaseSourceRead=resolve;});
 const outdatedAttach=host.applicationView(view,owner),outdatedResult=assert.rejects(outdatedAttach,/withdrawn/);
 await flush();host.closeViewsOwner(123);await host.applicationView(view,owner);
 const replacementView=calls.at(-1)[1],detachCount=calls.filter(c=>c[0]==='detach').length;
 releaseSourceRead();await outdatedResult;
 assert.notEqual(replacementView.viewGeneration,attached.viewGeneration);
 assert.equal(calls.filter(c=>c[0]==='detach').length,detachCount,'stale attach failure must not remove a newer view');
 pendingSourceRead=new Promise(resolve=>{releaseSourceRead=resolve;});
 const coveredAttach=host.applicationView(view,owner),coveredResult=assert.rejects(coveredAttach,/withdrawn/);
 await flush();await host.applicationView({workerEnvironmentId:'room',surfaceId:'desktop',runId:'run',viewId:'view',operation:'hide'},owner);
 const coveredEmbedCount=calls.filter(c=>c[0]==='embed').length;
 releaseSourceRead();await coveredResult;
 assert.equal(calls.filter(c=>c[0]==='embed').length,coveredEmbedCount,'hide must invalidate a pending native embed');
 assert.equal(calls.filter(c=>c[0]==='detach').length,detachCount,'stale attach must preserve the hidden view');
 pendingSourceRead=new Promise(resolve=>{releaseSourceRead=resolve;});
 const oldResize=host.applicationView({...view,rect:{...view.rect,width:600}},owner),oldResizeResult=assert.rejects(oldResize,/withdrawn/);
 await flush();await host.applicationView({...view,rect:{...view.rect,width:700}},owner);
 const resizeEmbedCount=calls.filter(c=>c[0]==='embed').length;
 releaseSourceRead();await oldResizeResult;
 assert.equal(calls.filter(c=>c[0]==='embed').length,resizeEmbedCount);
 assert.equal(calls.at(-1)[1].rect.width,700,'late old resize must not overwrite the newest native rectangle');
 assert.equal(calls.filter(c=>c[0]==='detach').length,detachCount);
 await host.applicationView(view,owner);
 await host.reconcile(candidates,[{...turn,turn_state:'waiting'}]);assert.equal(calls.filter(c=>c[0]==='complete').length,0);assert.equal(calls.filter(c=>c[0]==='cancel').length,0);
 const lost=calls.find(c=>c[0]==='begin')[1].requestId;
 event({type:'source-disconnected',requestId:lost,receiverNonce:nonces.get(lost)});assert.equal(host.snapshot()[0].phase,'reconnecting');
 assert.equal(calls.at(-1)[0],'detach','withdrawal immediately returns the exact embedded layer to native cleanup');
 await host.reconcile(candidates,[turn]);await flush();assert.equal(calls.filter(c=>c[0]==='install').length,3);
 const replacement=calls.filter(c=>c[0]==='begin').at(-1)[1].requestId;assert.notEqual(replacement,lost);assert.notEqual(nonces.get(replacement),nonces.get(lost));assert(buffers.every(b=>b.every(v=>v===0)));
 event({type:'source-permission-required',requestId:replacement,receiverNonce:nonces.get(replacement)});
 assert.match(host.snapshot().find(value=>value.source_window_id==='w1').error,/Automation permission/);
 await host.reconcile(candidates,[turn]);await flush();assert.equal(calls.filter(c=>c[0]==='install').length,3);assert.equal(calls.filter(c=>c[0]==='stop').length,0);
 const permissions=await host.permissionSources({workerEnvironmentId:'room',runId:'run'},helper);assert.equal(permissions.length,1);
 assert.equal((await permissions[0].exchange('status')).status,'not_determined');
 const settingsAbort=new AbortController(),settingsWait=permissions[0].exchange('open_settings',settingsAbort.signal);
 const cancelledSettings=assert.rejects(settingsWait,/closed/);await flush();assert(pendingSettings);settingsAbort.abort(new Error('Permission window closed'));await cancelledSettings;
 assert.deepEqual(calls.filter(c=>c[0]==='cancel-settings').map(c=>c[1]),[{requestId:pendingSettings.requestId,operationId:pendingSettings.operationId}]);
 event({type:'automation-permission-result',requestId:pendingSettings.requestId,operationId:pendingSettings.operationId,requirement:{...permission(),status:'granted',can_request:false},error:null});
 assert.equal(calls.filter(c=>c[0]==='install').length,3);assert.equal((await permissions[0].exchange('request')).status,'granted');
 await permissions[0].resume();await flush();assert.equal(calls.filter(c=>c[0]==='install').length,4);
 const afterPermission=calls.filter(c=>c[0]==='begin').at(-1)[1].requestId;assert.notEqual(afterPermission,replacement);assert.notEqual(nonces.get(afterPermission),nonces.get(replacement));
 assert(calls.filter(c=>c[0]==='permission').every(c=>Object.keys(c[1]).sort().join(',')==='operation,operationId,requestId,timeoutMs'));
 assert(buffers.every(b=>b.every(v=>v===0)));
 event({type:'source-withdrawn',requestId:afterPermission,receiverNonce:nonces.get(afterPermission)});
 await host.reconcile(candidates,[turn]);await flush();assert.equal(calls.filter(c=>c[0]==='install').length,4);
 await host.reconcile(candidates,[{...turn,turn_open:false,turn_state:'idle'}]);assert.deepEqual(calls.filter(c=>c[0]==='complete'),[['complete','run']]);
 await flush();assert.equal(calls.filter(c=>c[0]==='complete-source').length,1);assert.equal(calls.find(c=>c[0]==='complete-source')[2].receiverInstanceNonce,'b'.repeat(64));
 await host.reconcile([candidates[1]],[{...turn,turn_open:false,turn_state:'idle'}]);assert.deepEqual(calls.filter(c=>c[0]==='cancel').map(c=>c[1]),[lost,replacement,afterPermission]);assert.equal(host.snapshot().length,1);assert.equal(calls.filter(c=>c[0]==='stop').length,0);
 allowed=false;await assert.rejects(host.reconcile(candidates,[turn]),/locked/);host.revokeAll();assert.equal(host.snapshot().length,0);host.stop();assert.equal(calls.filter(c=>c[0]==='stop').length,0);
});

test('browser-only Home uses one receipted client presentation factory without installing a desktop driver (addon injected)', async () => {
 const syncFs=require('node:fs'),vm=require('node:vm'),ts=require('typescript');
 const {createClientPresentationHost,verifyClientPresentationArtifact}=require('../electron/nativePresentation.cjs');
 const {publishClientPresentation,sealClientPresentation}=require('./client-presentation-artifact.cjs');
 const root=await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(),'octopus-client-presentation-')));
 try {
  const source=path.resolve(__dirname,'../../registry/worker_environment_kind/desktop_session/packages/octopus-system-gui-env/native/pip');
  const manifestBytes=await fs.readFile(path.join(source,'manifest.json')),manifest=JSON.parse(manifestBytes),sha=b=>createHash('sha256').update(b).digest('hex');
  const clientRuntime=require('../native/runtime.cjs').clientRuntime();
  const resources=path.join(root,'resources'),home=path.join(root,'browser-only-home');
  await fs.mkdir(path.join(resources,'app'),{recursive:true});await fs.mkdir(path.join(home,'registry/worker_environment_kind/browser_session'),{recursive:true});
  const output=path.join(root,'injected-addon-bytes');await fs.writeFile(output,'fixture bytes: never loaded');
  const compiled={schema_version:2,target:'addon',architecture:'arm64',compiled:true,nativeExecution:false,header_version:clientRuntime.node,electron_version:clientRuntime.electron,
   manifest_sha256:sha(manifestBytes),source_sha256:Object.fromEntries(manifest.files.map(v=>[v.path,v.sha256])),output,output_sha256:sha(await fs.readFile(output)),
   stack_sha256:Object.fromEntries(await Promise.all(['stack.h','stack.mm','assets/pop-out-window-egg@3x.png',...Object.keys(manifest.settings_resources)].map(async name=>[name,sha(await fs.readFile(path.join(source,name)))])))};
  compiled.swift={source:'settings-visuals.swift',source_sha256:compiled.source_sha256['settings-visuals.swift'],compiler_sha256:sha(Buffer.from('injected compiler')),object_sha256:sha(Buffer.from('injected object')),header_sha256:sha(Buffer.from('injected generated header'))};
  const directory=path.join(resources,'native-presentation');publishClientPresentation({sourceRoot:source,buildReceipt:compiled,destination:directory});
  const artifact={resourcesRoot:resources,appRoot:path.join(resources,'app'),isPackaged:true,architecture:'arm64',nodeVersion:clientRuntime.node,electronVersion:clientRuntime.electron};
  assert.throws(()=>verifyClientPresentationArtifact(artifact),/signing/);sealClientPresentation(directory);
  assert.equal(verifyClientPresentationArtifact(artifact).modulePath,path.join(directory,manifest.module));
  assert.throws(()=>verifyClientPresentationArtifact({...artifact,isPackaged:false}),/packaged/);
  assert.throws(()=>verifyClientPresentationArtifact({...artifact,nodeVersion:'24.16.0'}),/runtime/);
  const calls=[],events=[];let currentHome=home,event;
  const native=Object.fromEntries(['registerHost','removeHost','setSelection','upsertBrowserImage','invalidateBrowserImage','beginSource','installSource','cancelSource','embedSource','detachEmbeddedSource','hideEmbeddedSource','completeGroup','clearCompletedGroup','stop'].map(name=>[name,value=>calls.push([name,value])]));
  const main=ts.createSourceFile('main.cjs',await fs.readFile(path.join(__dirname,'../electron/main.cjs'),'utf8'),ts.ScriptTarget.ES2022,true,ts.ScriptKind.JS);
  const names=new Set(['getNativeClientPresentations','getNativeBrowserPresentations','getNativeDesktopPresentations']);
  const functions=main.statements.filter(node=>ts.isFunctionDeclaration(node)&&names.has(node.name?.text)).map(node=>node.getText(main));assert.equal(functions.length,3);
  const context=vm.createContext({path,process:{resourcesPath:resources,arch:'arm64',versions:{node:clientRuntime.node,electron:clientRuntime.electron}},
   app:{isPackaged:true,getAppPath:()=>path.join(resources,'app')},nativeClientPresentations:null,nativeBrowserPresentations:null,nativeDesktopPresentations:null,nativeDesktopPermissions:null,
   resolveOctopusHome:()=>currentHome,requireNativeDesktopAccess:()=>{},syncNativeDesktopHosts:()=>calls.push(['sync-hosts']),
   createClientPresentationHost:options=>createClientPresentationHost({...options,loadAddon:file=>{calls.push(['load',file]);return{createPiPHost:config=>{event=config.onEvent;calls.push(['factory']);return native;}};}}),
   createBrowserPresentationHost:options=>({native:options.getNativeHost,onEvent:async value=>events.push(value)}),
   createDesktopPresentationHost:options=>({native:options.getNativeHost,onEvent:value=>events.push(value)}),
   currentNativeBrowserPreviewTarget:()=>{},readNativeBrowserToolDescriptor:()=>{throw new Error('Factory-only fixture must not read browser tool descriptors');},getDesktopSurfaceHost:()=>({preparePresentationSource(){throw new Error('No CU source in browser-only fixture');}}),
   projectionStores:new Map(),appendClientLog:()=>{},scheduleNativeDesktopPresentations:()=>{},
   clientSetupPackageRoots:()=>{throw new Error('Presentation must not inspect installed driver state');},
   withLocalPackageInstallation:()=>{throw new Error('Presentation must not require/install driver.desktop_session');}});
  vm.runInContext(functions.join('\n'),context);
  const browser=vm.runInContext('getNativeBrowserPresentations()',context);assert.equal(await browser.native(),native);
  assert.equal(context.nativeDesktopPresentations,null);assert.equal(syncFs.existsSync(path.join(home,'registry/worker_environment_kind/desktop_session')),false);
  const desktop=vm.runInContext('getNativeDesktopPresentations()',context);assert.equal(await desktop.native(),native);assert.equal(calls.filter(v=>v[0]==='factory').length,1);
  const shared=context.nativeClientPresentations;shared.sync([{run_id:'browser-run',turn_open:true,turn_opened_movement_seq:10}]);
  event({type:'hide-groups',groups:['browser-run']});shared.sync([{run_id:'browser-run',turn_open:true,turn_opened_movement_seq:10}]);
  assert.deepEqual(calls.filter(v=>v[0]==='setSelection').at(-1)[1].suppressedGroupIds,['browser-run']);
  shared.sync([{run_id:'browser-run',turn_open:true,turn_opened_movement_seq:20}]);assert.deepEqual(calls.filter(v=>v[0]==='setSelection').at(-1)[1].suppressedGroupIds,[]);
  currentHome=path.join(root,'other-home');await assert.rejects(browser.native(),/Home changed/);shared.stop();shared.stop();assert.equal(calls.filter(v=>v[0]==='stop').length,1);
  const modulePath=path.join(directory,manifest.module);await fs.appendFile(modulePath,'tampered');
  assert.throws(()=>verifyClientPresentationArtifact(artifact),/bytes differ/);assert.equal(calls.filter(v=>v[0]==='load').length,1);
 } finally {await fs.rm(root,{recursive:true,force:true});}
});


test('native presentation reconciliation isolates one invalid source or browser without skipping unrelated withdrawal', async () => {
 const vm=require('node:vm'),ts=require('typescript'),calls=[],errors=[];
 const browser=id=>({worker_environment_id:id,run_id:id,lifecycle:'active',worker_environment_kind:'browser_session',formation_pin:'pin',
  client_surfaces:[{id:`${id}:page`,protocol:'browser.native.v1',mode:'direct',data:{engine_handle:id,host_generation:'generation',browser_context_id:id}},...(id==='good-browser'?[{id:`${id}:history`,protocol:'browser.native.downloads.v1',mode:'direct'}]:[])]});
 const desktop=id=>({worker_environment_id:id,run_id:id,lifecycle:'active',worker_environment_kind:'desktop_session',formation_pin:'pin',
  client_surfaces:[{id:`${id}:desktop`,protocol:'desktop_session',mode:'direct',data:{desktop:{applications:[{application:{app:{app_instance_id:id}},windows:[{window_id:id,source_image:{}}]}]}}}]});
 const rooms=[desktop('invalid-source'),desktop('good-source'),browser('stale-browser'),browser('good-browser')];
 const turns=rooms.map(room=>({run_id:room.run_id,turn_open:false,turn_opened_movement_seq:10,last_movement_seq:20}));
 let authorized=true;
 const desktopHost={snapshot:()=>[],reconcile:async values=>calls.push(['desktop',values.map(value=>value.input.sourceWindowId)]),revokeAll:()=>calls.push(['revoke-desktop'])};
 const browserHost={snapshot:()=>[],reconcile:async values=>{calls.push(['browser',values.map(value=>value.room.worker_environment_id)]);throw new Error('image artifact unavailable');},revokeAll:()=>calls.push(['revoke-browser'])};
 const history={reconcile:async values=>calls.push(['history',values.map(value=>value.room.worker_environment_id)]),revokeAll:()=>calls.push(['revoke-history'])};
 const context=vm.createContext({process:{platform:'darwin'},quitCleanupStarted:false,nativeDesktopRefreshScheduled:false,nativeDesktopRefreshRunning:false,nativeDesktopRefreshAgain:false,
  queueMicrotask,nativeBrowserHost:{getEngineInfo:()=>({capabilities:{persistentDownloadHistory:true}})},nativeBrowserHome:'/home',resolveOctopusHome:()=>'/home',nativeDesktopAccessEntries:()=>authorized?[{}]:[],
  requireNativeDesktopAccess:()=>{if(!authorized)throw new Error('access withdrawn');},requireNativeBrowserHome:()=>{},
  getProjectionStore:()=>({read:async request=>request.kind==='projection.worker_environments.list'?{worker_environments:rooms}:{chats:turns}}),
  projectionReadRequest:(kind,args)=>({kind,args}),presentationSource:(_descriptor,input)=>{if(input.sourceWindowId==='invalid-source')throw new Error('invalid exact source');return{source:{},identity:{}};},
  nativeDesktopPresentations:desktopHost,nativeBrowserPresentations:browserHost,nativeBrowserDownloadHistory:history,
  getNativeDesktopPresentations:()=>desktopHost,getNativeBrowserPresentations:()=>browserHost,getNativeBrowserDownloadHistory:()=>history,
  requireNativeBrowserHost:()=>({syncTurn:async projection=>{calls.push(['turn',projection.worker_environment_id]);if(projection.worker_environment_id==='stale-browser')throw new Error('stale generation');}}),
  nativeClientPresentations:{sync:()=>calls.push(['selection']),home:()=>'/home'},
  appendClientLog:(_level,label,detail)=>errors.push([label,detail.message]),
 });
 const source=ts.createSourceFile('main.cjs',await fs.readFile(path.join(__dirname,'../electron/main.cjs'),'utf8'),ts.ScriptTarget.ES2022,true,ts.ScriptKind.JS);
 const declaration=source.statements.find(node=>ts.isFunctionDeclaration(node)&&node.name?.text==='scheduleNativeDesktopPresentations');assert.ok(declaration);
 vm.runInContext(declaration.getText(source),context);
 vm.runInContext('scheduleNativeDesktopPresentations()',context);await new Promise(setImmediate);
 assert.equal(context.nativeDesktopRefreshRunning,false);
 assert.deepEqual(JSON.parse(JSON.stringify(calls)),[['turn','stale-browser'],['turn','good-browser'],['history',['good-browser']],['browser',['good-browser']],['desktop',['good-source']],['selection']]);
 assert.deepEqual(errors.map(value=>value[0]),['desktop.native_presentation.source_unavailable','browser.native_presentation.room_unavailable','browser.native_presentation.unavailable']);
 authorized=false;vm.runInContext('scheduleNativeDesktopPresentations()',context);await new Promise(setImmediate);
 assert.deepEqual(calls.slice(-3),[['revoke-browser'],['revoke-history'],['revoke-desktop']]);
 assert.equal(context.nativeDesktopRefreshRunning,false);
});


test('native permission UI binds human callbacks, fresh helper status and uncertain-request refresh without app/input grants', async () => {
 const {createDesktopPermissionHost,validatePermissionSnapshot,validatePermissionSettingsSource}=require('../electron/desktopSurface.cjs');const f=await fixture(1000,true);
 const row={id:'screen',kind:'screen_capture',title:'Screen Recording',status:'not_granted',can_request:true,can_open_settings:true,provenance:{process_id:77,api:'CGPreflightScreenCaptureAccess'}};
 const snapshot={schema_version:1,host_generation:f.manifest.host_generation,helper:{executable_path:'/test/bin/octopus-desktop-native',display_name:'octopus-desktop-native',process_id:77,bundle:null,responsible_app:null},permissions:[row]};
 const calls=[];let changedHome=f.root,allowed=true,uncertain=false;
 assert.deepEqual(validatePermissionSnapshot(snapshot,f.manifest.host_generation).helper,snapshot.helper);
 for(const altered of [
  {...snapshot,helper:{...snapshot.helper,executable_path:'helper.app'}},
  {...snapshot,helper:{...snapshot.helper,bundle:{identifier:'invented.app'}}},
  {...snapshot,permissions:[{...row,provenance:{process_id:78,api:'CGPreflightScreenCaptureAccess'}}]},
  {...snapshot,permissions:[{...row,kind:'automation',provenance:{process_id:77,api:'AEDeterminePermissionToAutomateTarget'}}]},
 ])assert.throws(()=>validatePermissionSnapshot(altered,f.manifest.host_generation),/Invalid native permission/);
 f.setNext(body=>{
  assert.equal(body.tool,'operator.desktop');assert.equal(body.action.type,'permissions');
  if(body.action.operation==='request' && uncertain)throw new Error('outcome uncertain');
  return new Response(JSON.stringify({schema_version:2,worker_environment_id:body.worker_environment_id,request_id:body.request_id,operation:'permissions',accepted:true,permission_result:snapshot}));
 });
 let sourceUnavailable;
 const native={showPermissions:value=>calls.push(['show',value]),updatePermissions:value=>calls.push(['update',value]),updatePermissionSettings:value=>{calls.push(['settings',value]);if(value.source===null)sourceUnavailable?.();},closePermissions:value=>calls.push(['close',value])};
 let automationRequests=0,automationResumes=0,automationGranted=false;
 const automationRow=()=>({id:'native-client-recipient',kind:'automation',title:'Native presentation connection',status:automationGranted?'granted':'not_determined',can_request:!automationGranted,can_open_settings:true,detail:'Native observed fixture',
  provenance:{api:'AEDeterminePermissionToAutomateTarget',event_class:'OcPi',event_id:'BtV1',sender:{...snapshot.helper,process_id:process.pid,executable_path:process.execPath,start_seconds:'1',start_microseconds:'0'},recipient:{...snapshot.helper,start_seconds:'2',start_microseconds:'0'}}});
 const ref={id:'native-client-recipient',current:async()=>{},exchange:async operation=>{if(operation==='request'){automationRequests++;automationGranted=true;}return automationRow();},resume:async()=>{automationResumes++;}};
 const host=createDesktopPermissionHost({prepareSource:input=>f.host.preparePermissionSource(input),automationSources:async()=>[ref],getNativeHost:async()=>native,homePath:()=>changedHome});
 const owner={id:44,current:()=>{if(!allowed)throw new Error('access withdrawn');},windowHandle:()=>Buffer.from('actual handle injected; not dereferenced')};
 const request={workerEnvironmentId:'room-a',surfaceId:'desktop',runId:'run-a'};
 try {
  assert.deepEqual(await host.open(request,owner),{opened:true});assert.deepEqual(f.calls.map(c=>c.action.operation),['status']);
  const identity=calls.find(c=>c[0]==='show')[1];assert.equal(identity.snapshot.automation[0].provenance.sender.process_id,process.pid);assert.equal(automationRequests,0);
  const event=(operation,sequence,permission=null)=>({type:'permission-action',presentationId:identity.presentationId,generation:identity.generation,operation,sequence,permission});
  uncertain=true;await host.onEvent(event('request',1,'screen'));
  assert.deepEqual(f.calls.map(c=>c.action.operation),['status','status','request']);assert.match(calls.at(-1)[1].error,/uncertain/);
  snapshot.helper=Object.fromEntries(Object.entries(snapshot.helper).reverse());
  await host.onEvent(event('status',2));assert.equal(f.calls.filter(c=>c.action.operation==='request').length,1);
  assert.equal(calls.at(-1)[1].error,null);assert.equal(calls.at(-1)[1].snapshot.permissions[0].status,'not_granted');
  await host.onEvent({...event('request',3,'screen'),generation:'stale'});assert.equal(f.calls.length,4);
  let original,finishSettings,reads=0,sourceController;
  const hidden={schema_version:1,host_generation:f.manifest.host_generation,permission_request_id:'bound at original request',permission:'screen',state:'waiting',settings:null,window:null};
  const actual={...snapshot.helper,process_id:88,executable_path:'/test/settings/native',start_seconds:'123',start_microseconds:'5'};
  const reply=body=>({schema_version:2,worker_environment_id:body.worker_environment_id,request_id:body.request_id,operation:'permissions',accepted:true,permission_result:snapshot});
  f.setNext(body=>{
   if(body.action.type==='permission_settings_source'){
    assert.equal(body.action.permission_request_id,original.request_id);assert.equal(body.worker_environment_id,original.worker_environment_id);
    const source=++reads===1||reads===3?{...hidden}:{...hidden,state:'observed',settings:actual,window:{id:reads===5?12:11,observation_sequence:reads===2?1:2,bounds:{x:0,y:0,width:800,height:500}}};
    return new Response(JSON.stringify({schema_version:2,worker_environment_id:body.worker_environment_id,request_id:body.request_id,operation:'permission_settings_source',accepted:true,permission_settings_source:source}));
   }
   if(body.action.operation==='open_settings'){
    original=body;hidden.permission_request_id=body.request_id;
    return new Response(new ReadableStream({start(controller){sourceController=controller;finishSettings=()=>{controller.enqueue(Buffer.from(JSON.stringify(reply(body))));controller.close();};}}));
   }
   return new Response(JSON.stringify(reply(body)));
  });
  const unavailable=new Promise(resolve=>{sourceUnavailable=resolve;});
  const settingsOperation=host.onEvent(event('open_settings',3,'screen'));await unavailable;
  assert.equal(reads,5);assert.equal(f.calls.filter(c=>c.action.operation==='open_settings').length,1);
  const presented=calls.filter(c=>c[0]==='settings').map(c=>c[1].source);
  assert.deepEqual(presented.map(v=>v?.state||'unavailable'),['waiting','observed','waiting','observed','unavailable']);
  assert(presented.filter(v=>v?.window).every(v=>v.window.id===11));
  assert.throws(()=>validatePermissionSettingsSource({...hidden,state:'observed'},f.manifest.host_generation,original.request_id,'screen'),/process/);
  assert.throws(()=>validatePermissionSettingsSource({...hidden,permission_request_id:randomUUID()},f.manifest.host_generation,original.request_id,'screen'),/request/);
  finishSettings();await settingsOperation;assert.match(calls.at(-1)[1].error,/Settings accessory unavailable/);
  assert.equal(calls.at(-1)[1].snapshot.permissions[0].status,'not_granted');
  assert.equal(reads,5,'bad source closes presentation observation without replaying or cancelling the original human Settings action');
  assert(f.calls.every(c=>!('scope' in c.action)&&!('control_generation' in c.action)&&!('bundle_id' in c.action)));
  await host.onEvent(event('request',4,'native-client-recipient'));assert.equal(automationRequests,1);assert.equal(automationResumes,1);assert(f.calls.every(c=>c.action.permission!=='native-client-recipient'));
  allowed=false;const before=f.calls.length;await host.onEvent(event('request',5,'screen'));assert.equal(f.calls.length,before);assert.equal(calls.at(-1)[0],'close');
  allowed=true;await host.open(request,owner);const second=calls.findLast(c=>c[0]==='show')[1];assert.notEqual(second.presentationId,identity.presentationId);
  changedHome=f.root+'/other';await host.onEvent({type:'permission-action',presentationId:second.presentationId,generation:second.generation,sequence:1,operation:'request',permission:'screen'});
  assert.equal(calls.at(-1)[0],'close');assert.equal(f.calls.filter(c=>c.action.operation==='request').length,1);
  assert.equal(new Set(f.calls.map(c=>c.request_id)).size,f.calls.length);host.stop();
 } finally {host.stop();await f.close();}
});
