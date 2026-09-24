/* The browser booth reads playout state; it never schedules audio. */
(() => {
  'use strict';
  const $=id=>document.getElementById(id);
  const preference=matchMedia('(prefers-reduced-motion: reduce)');
  for (const id of ['rain-enabled','lights-enabled','cat-enabled','lightning-enabled','reduced']) {
    let saved=null;try {saved=localStorage.getItem(`defalt.studio.${id}`);}catch{}
    $(id).checked=saved===null ? (id==='reduced' ? preference.matches : true) : saved==='true';
    $(id).addEventListener('change',()=>{try{localStorage.setItem(`defalt.studio.${id}`,String($(id).checked));}catch{} sync();});
  }
  function sync(){document.body.classList.toggle('reduced',$('reduced').checked);}
  preference.addEventListener('change',()=>{$('reduced').checked=preference.matches;sync();});sync();
  let last=0,key='',generation=0,bass=0,beat=0;
  const holds={mav:0,rue:0};
  window.LiveStudio={update(levels,music,energy=0,running=!!music,tones={mav:0,rue:0}){
    const now=performance.now();if(now-last<32)return;
    const elapsed=last ? (now-last)/1000:0;last=now;
    const dt=elapsed<.25 ? elapsed:0;
    // A kick is the bass jumping clear of its own running level; the hosts
    // nod along to it, a little.
    beat*=Math.exp(-dt/0.18);
    if(energy>bass*1.35+0.04)beat=1;
    bass+=(energy-bass)*(1-Math.exp(-dt/0.6));
    for(const [index,host] of ['mav','rue'].entries()){
      if(levels[host]>.018)holds[host]=now+75;
      const speaking=now<holds[host];
      const tag=$(`${host}-tag`);if(tag.classList.contains('speaking')!==speaking)tag.classList.toggle('speaking',speaking);
    }
    const both=now<holds.mav&&now<holds.rue;
    const status=both?'Both mics open':now<holds.mav?'Mav has the mic':now<holds.rue?'Rue has the mic':music?'On air':'Between records';
    if($('studio-status').textContent!==status)$('studio-status').textContent=status;
    window.StudioAmbience?.tick(dt,both);
    window.StudioScene?.update(dt,{mav:now<holds.mav,rue:now<holds.rue},energy,running,
      {levels:[levels.mav||0,levels.rue||0],tones:[tones.mav||0,tones.rue||0]},beat);
    if($('studio-vinyl').classList.contains('playing')!==!!music)$('studio-vinyl').classList.toggle('playing',!!music);
    const next=music?.meta?.key||'';
    if(next!==key){
      key=next;const token=++generation;
      $('studio-cover').hidden=true;$('studio-cover-label').hidden=false;
      if(key){
        const image=new Image();
        image.onload=()=>{if(token!==generation)return;$('studio-cover').src=image.src;$('studio-cover').hidden=false;$('studio-cover-label').hidden=true;};
        image.src=`/api/artwork?key=${encodeURIComponent(key)}`;
      }
    }
  }};
})();
