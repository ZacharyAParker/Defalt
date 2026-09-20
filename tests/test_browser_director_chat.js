const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
class Element {
  constructor(){this.handlers={};this.value='';this.checked=false;this.children=[];this.scrollHeight=10;this.scrollTop=0;this.clientHeight=100;}
  addEventListener(name,fn){this.handlers[name]=fn;}
  append(...children){this.children.push(...children);}
  replaceChildren(){this.children=[];}
  focus(){}
  show(){this.open=true;}
  close(){this.open=false;}
  requestSubmit(){return this.handlers.submit({preventDefault(){}});}
}
const elements={};
for(const id of ['director-chat','director-chat-form','director-chat-input','director-chat-log','director-chat-status','director-chat-send','director-chat-save','director-chat-share','director-chat-undo','director-chat-open','director-chat-close','director-chat-direction']) elements[id]=new Element();
const calls=[];let fail=false,id=0;
const document={hidden:false,getElementById:id=>elements[id],createElement:()=>new Element()};
const context={document,crypto:{randomUUID:()=>`message-${++id}`},AbortController,setTimeout:()=>1,clearTimeout(){},setInterval:()=>1,clearInterval(){},
  fetch:async(path,opts)=>{calls.push({path,body:opts?.body && JSON.parse(opts.body)});if(fail)throw Error('Network unavailable');return{ok:true,json:async()=>({busy:false,messages:[]})};}};
vm.runInNewContext(fs.readFileSync('web/static/director-chat.js','utf8'),context);
(async()=>{
  const form=elements['director-chat-form'],input=elements['director-chat-input'];
  input.value='Keep this energy';fail=true;
  await form.requestSubmit();
  assert.equal(input.value,'Keep this energy','failed submission preserves draft');
  assert.match(elements['director-chat-status'].textContent,/Network unavailable/);
  const firstId=calls[0].body.id;fail=false;await form.requestSubmit();
  assert.equal(calls[1].body.id,firstId,'uncertain retry uses same message ID');
  assert.equal(input.value,'');
  input.value='Hello Mav';elements['director-chat-share'].checked=true;
  await form.requestSubmit();
  assert.equal(calls[2].body.share,true);assert.equal(elements['director-chat-share'].checked,false);
  input.value='x'.repeat(241);elements['director-chat-share'].checked=true;
  await form.requestSubmit();assert.equal(calls.length,3,'oversized on-air message never sent');
  elements['director-chat'].open=true;
  elements['director-chat'].handlers.keydown({key:'Escape',preventDefault(){},stopPropagation(){}});
  assert.equal(elements['director-chat'].open,false);
  console.log('Director chat browser: private default, drafts, idempotent retry, explicit sharing, limits and close passed');
})().catch(error=>{console.error(error);process.exitCode=1;});
