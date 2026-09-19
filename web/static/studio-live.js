/* The browser booth reads playout state; it never schedules audio. */
(() => {
  'use strict';
  const $=id=>document.getElementById(id);
  const preference=matchMedia('(prefers-reduced-motion: reduce)');
  for (const id of ['rain-enabled','lights-enabled','cat-enabled','reduced']) {
    let saved=null;try {saved=localStorage.getItem(`defalt.studio.${id}`);}catch{}
    $(id).checked=saved===null ? (id==='reduced' ? preference.matches : true) : saved==='true';
    $(id).addEventListener('change',()=>{try{localStorage.setItem(`defalt.studio.${id}`,String($(id).checked));}catch{} sync();});
  }
  function sync(){document.body.classList.toggle('reduced',$('reduced').checked);}
  preference.addEventListener('change',()=>{$('reduced').checked=preference.matches;sync();});sync();
  let last=0,time=0,key='',generation=0;
  const holds={mav:0,rue:0};
  window.LiveStudio={update(levels,music){
    const now=performance.now();if(now-last<32)return;
    const dt=last ? Math.min(.1,(now-last)/1000):0;last=now;
    if(!$('reduced').checked)time+=dt;
    for(const [index,host] of ['mav','rue'].entries()){
      if(levels[host]>.018)holds[host]=now+75;
      const speaking=now<holds[host];
      document.querySelector(`.${host}-mouth`).classList.toggle('open',speaking);
      document.querySelector(`.${host}-eyes`).classList.toggle('blink',!$('reduced').checked&&(time+index*1.7)%(index?6.7:5.1)<.14);
      $(`${host}-tag`).classList.toggle('speaking',speaking);
    }
    const both=now<holds.mav&&now<holds.rue;
    $('studio-status').textContent=both?'Both mics open':now<holds.mav?'Mav has the mic':now<holds.rue?'Rue has the mic':music?'On air':'Between records';
    window.StudioAmbience?.tick(dt,both);
    $('studio-vinyl').classList.toggle('playing',!!music);
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
