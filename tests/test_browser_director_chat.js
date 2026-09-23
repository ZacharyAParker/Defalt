const vm=require('node:vm'),assert=require('node:assert/strict');
const {read,element}=require('./browser_harness');
const elements={};
for(const id of ['director-chat','director-chat-form','director-chat-input','director-chat-log','director-chat-status','director-chat-send','director-chat-save','director-chat-share','director-chat-undo','director-chat-open','director-chat-close','director-chat-direction','director-chat-count']) {
  const node=element();node.scrollHeight=10;node.scrollTop=0;node.clientHeight=100;
  node.show=()=>{node.open=true;};node.close=()=>{node.open=false;};
  node.requestSubmit=()=>node.handlers.submit({preventDefault(){}});
  elements[id]=node;
}
const calls=[],timers=[];let fail=false,id=0;
const documentHandlers={};
const document={hidden:false,getElementById:id=>elements[id],createElement:()=>element(),
  addEventListener:(name,fn)=>{documentHandlers[name]=fn;}};
const context={document,crypto:{randomUUID:()=>`message-${++id}`},AbortController,
  setTimeout:(fn,ms)=>{timers.push({fn,ms});return timers.length;},clearTimeout(){},
  fetch:async(path,opts)=>{calls.push({path,body:opts?.body && JSON.parse(opts.body)});if(fail)throw Error('Network unavailable');return{ok:true,json:async()=>({busy:false,messages:[]})};}};
vm.runInNewContext(read('web/static/director-chat.js'),context);
(async()=>{
  const form=elements['director-chat-form'],input=elements['director-chat-input'];
  input.value='Keep this energy';fail=true;
  await form.requestSubmit();
  assert.equal(input.value,'Keep this energy','failed submission preserves draft');
  assert.match(elements['director-chat-status'].textContent,/Network unavailable/);
  const firstId=calls[0].body.id;fail=false;await form.requestSubmit();
  assert.equal(calls[1].body.id,firstId,'uncertain retry uses same message ID');
  assert.equal(input.value,'');
  assert.match(elements['director-chat-count'].textContent,/0 \/ 12,000 characters/);
  input.value='Hello Mav';elements['director-chat-share'].checked=true;
  await form.requestSubmit();
  assert.equal(calls[2].body.share,true);assert.equal(elements['director-chat-share'].checked,false);
  input.value='x'.repeat(241);elements['director-chat-share'].checked=true;
  await form.requestSubmit();assert.equal(calls.length,3,'oversized on-air message never sent');
  input.handlers.input();assert.equal(elements['director-chat-count'].classList.contains('over-limit'),true);
  assert.equal(elements['director-chat-send'].disabled,true);

  // Undo is its own message and leaves the draft (and its options) alone.
  input.value='half-written thought';elements['director-chat-share'].checked=false;elements['director-chat-save'].checked=true;
  await elements['director-chat-undo'].handlers.click();
  assert.equal(calls.length,4);
  assert.deepEqual({...calls[3].body,id:undefined},{message:'Undo last change',save:false,share:false,id:undefined});
  assert.equal(input.value,'half-written thought','undo must not overwrite the draft');
  assert.equal(elements['director-chat-save'].checked,true,'undo must not change the draft options');

  // Polling: only while open, slow when the director is idle.
  timers.length=0;
  elements['director-chat-open'].handlers.click();
  assert.equal(elements['director-chat'].open,true);
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(calls.at(-1).body,undefined,'opening fetches the conversation');
  assert.equal(timers.at(-1).ms,8000,'an idle conversation polls slowly');
  elements['director-chat'].handlers.keydown({key:'Escape',preventDefault(){},stopPropagation(){}});
  assert.equal(elements['director-chat'].open,false);
  const before=calls.length;await timers.at(-1).fn();
  assert.equal(calls.length,before,'a closed panel stops polling');
  console.log('Director chat browser: private default, drafts, idempotent retry, explicit sharing, limits, undo, polling and close passed');
})().catch(error=>{console.error(error);process.exitCode=1;});
