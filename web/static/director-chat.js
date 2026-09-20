(() => {
  'use strict';
  const byId = id => document.getElementById(id);
  const panel=byId('director-chat'), form=byId('director-chat-form'), draft=byId('director-chat-input');
  if (!panel || !form) return;
  const log=byId('director-chat-log'), status=byId('director-chat-status'), send=byId('director-chat-send');
  const save=byId('director-chat-save'), share=byId('director-chat-share'), undo=byId('director-chat-undo');
  let busy=false, fetching=false, posting=false, retry=null, lastMessages='', timer=null, generation=0;
  function controls() {
    send.disabled=busy || posting || !draft.value.trim() || (share.checked && draft.value.length>240);
    send.textContent=share.checked?'Send to hosts':'Send';
    undo.disabled=busy || posting;
  }
  async function api(body) {
    const abort=new AbortController(), deadline=setTimeout(()=>abort.abort(),10000);
    try {
      const response=await fetch('/api/director/chat',body?{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body),signal:abort.signal}:{signal:abort.signal});
      let result;
      try {result=await response.json();} catch (_) {throw new Error('Director chat is unavailable. Start Radio or update its backend.');}
      if (!response.ok) throw new Error(result.error || 'The director could not accept that message.');
      return result;
    } finally {clearTimeout(deadline);}
  }
  function paint(state) {
    busy=!!state.busy;
    status.textContent=busy?'Director is replying…':'';
    const direction=state.direction?.description;
    byId('director-chat-direction').textContent=direction?`Music direction: ${direction}`:'Normal rotation · no private direction';
    if (state.quiet_minutes>0) byId('director-chat-direction').textContent+=` · Fewer host breaks for ${Math.ceil(state.quiet_minutes)} min`;
    const messages=JSON.stringify(state.messages || []);
    if(messages!==lastMessages) {
      const follow=log.scrollHeight-log.scrollTop-log.clientHeight<70;
      lastMessages=messages;log.replaceChildren();
      if(!state.messages?.length) {
        const welcome=document.createElement('p');welcome.className='director-chat-welcome';welcome.textContent="Tell me what you're in the mood for.";log.append(welcome);
        for(const example of ['Keep this energy, but less rap.','That last pick was perfect. More like that.','Less talking for twenty minutes.','Why did you choose this song?']) {
          const button=document.createElement('button');button.type='button';button.className='director-chat-example';button.textContent=example;
          button.addEventListener('click',()=>{draft.value=example;draft.focus();controls();});log.append(button);
        }
      }
      for(const item of state.messages || []) {
        const row=document.createElement('div'), author=document.createElement('strong'), text=document.createElement('p');
        row.className='director-chat-message';author.textContent=item.role==='user'?'You':'Director';text.textContent=item.text;
        row.append(author,text);
        if(item.shared) {const note=document.createElement('small');note.textContent='Sent with permission to share on air';row.append(note);}
        log.append(row);
      }
      if(follow) log.scrollTop=log.scrollHeight;
    }
    controls();
  }
  async function poll() {
    if(!panel.open || fetching || posting || document.hidden) return;
    fetching=true;
    const started=generation;
    try {const state=await api();if(started===generation) paint(state);} catch(error) {if(started===generation) status.textContent=error.name==='AbortError'?'The director took too long to respond. Retrying…':error.message;} finally {fetching=false;}
  }
  byId('director-chat-open').addEventListener('click',()=>{
    if(!panel.open) panel.show();
    draft.focus();poll();clearInterval(timer);timer=setInterval(poll,2000);
  });
  function close() {panel.close();clearInterval(timer);byId('director-chat-open').focus();}
  byId('director-chat-close').addEventListener('click',close);
  panel.addEventListener('keydown',event=>{if(event.key==='Escape'){event.preventDefault();event.stopPropagation();close();}});
  for(const element of [draft,save,share]) element.addEventListener('input',controls);
  draft.addEventListener('keydown',event=>{if(event.ctrlKey && event.key==='Enter'){event.preventDefault();form.requestSubmit();}});
  form.addEventListener('submit',async event=>{
    event.preventDefault();if(busy || posting || !draft.value.trim()) return;
    if(share.checked && draft.value.length>240) {status.textContent='Keep on-air messages under 240 characters.';return;}
    const body={message:draft.value.trim(),save:save.checked,share:share.checked};
    if(retry && ['message','save','share'].every(key=>retry[key]===body[key])) body.id=retry.id;
    else body.id=crypto.randomUUID();
    retry=body;posting=true;generation++;controls();
    try {paint(await api(body));draft.value='';save.checked=false;share.checked=false;retry=null;}
    catch(error) {status.textContent=error.name==='AbortError'?'Connection timed out. Your draft is kept; retrying will not send it twice.':error.message;}
    finally {posting=false;controls();}
  });
  undo.addEventListener('click',()=>{draft.value='Undo last change';save.checked=false;share.checked=false;form.requestSubmit();});
})();
