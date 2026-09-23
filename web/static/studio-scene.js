/* Layered booth renderer. Consumes host activity; never starts or schedules audio.

   Drawn in a 1728x1152 scene space onto a bitmap sized to the element (CSS box
   x devicePixelRatio), so the art is resampled once, smoothly, instead of the
   browser shrinking a full-size bitmap every frame. The layers that never move
   (the room behind, the mugs and microphones in front, each host with their
   headphones) are composited once into offscreen canvases; a frame only
   repaints the regions whose state changed, from those. */
(() => {
  'use strict';
  const $=id=>document.getElementById(id);
  const canvas=$('studio-scene'), ctx=canvas.getContext('2d');
  const W=1728,H=1152,art={};
  const version=(/[?&]v=([^&]+)/.exec(document.currentScript?.src||'')||[])[1]||'';
  const spriteUrl=name=>`/static/studio-v2/web/${name}.webp${version?`?v=${version}`:''}`;
  // Built by tools/build-web-art.py from the PNG layers the desktop app embeds.
  const sprites=['background','microphones','mav','rue','mav-phones','rue-phones','black-mug','white-mug',
    'mav-mouth','rue-mouth','mav-eye-0','mav-eye-1','rue-eye-0','rue-eye-1','cat-sleep','cat-awake','cat-yawn','cat-groom'];
  const hosts=[
    {name:'mav',at:[101,183,781,861],phones:[383,185,328,296],anchor:[485,1005],mouth:[585,463,89,51],eyes:[[550,374,64,40],[634,372,48,41]]},
    {name:'rue',at:[910,213,681,824],phones:[1077,216,304,305],anchor:[1250,1010],mouth:[1252,477,84,55],eyes:[[1224,391,71,47],[1323,405,54,48]]}
  ];
  const CAT=[49,251,264,137], MUGS=[['black-mug',[416,892,164,175]],['white-mug',[1095,921,184,175]]];
  const WINDOWS=[[466,0,243,618],[726,0,405,618]];
  const LIGHTS=[[771,389],[818,474],[905,383],[987,430],[1000,514],[748,436]];
  // Regions a frame may need to repaint, in scene space.
  const REGION={window:[466,0,665,618],cat:[40,196,290,200]};
  hosts.forEach(h=>{h.region=pad(h.at,6);});
  let clock=0,rainClock=0,energy=0,onAir=false,speaking={mav:false,rue:false},visible=true,ready=false;
  let scale=1,layers=null,previous={},lastRender=0,forceFull=true;

  function pad(r,n){return [r[0]-n,r[1]-n,r[2]+2*n,r[3]+2*n];}
  function union(a,b){const x=Math.min(a[0],b[0]),y=Math.min(a[1],b[1]);return [x,y,Math.max(a[0]+a[2],b[0]+b[2])-x,Math.max(a[1]+a[3],b[1]+b[3])-y];}
  function draw(g,name,rect){if(art[name])g.drawImage(art[name],...rect);}
  function offscreen(w,h){const c=document.createElement('canvas');c.width=Math.max(1,Math.ceil(w));c.height=Math.max(1,Math.ceil(h));return c;}

  /* Match the bitmap to the element. Capped at the art's own scene size. */
  function fit(width){
    const dpr=Math.min(window.devicePixelRatio||1,2);
    const target=Math.max(2,Math.min(W,Math.round(width*dpr)));
    if(target===canvas.width)return;
    canvas.width=target;canvas.height=Math.round(target*H/W);
    layers=null;forceFull=true;render(true);
  }
  if(typeof ResizeObserver==='function'){
    new ResizeObserver(entries=>{const width=entries[0].contentRect.width;if(width>0)fit(width);}).observe(canvas);
  }

  /* The static layers, once per bitmap size (and again when late art arrives). */
  function build(){
    scale=canvas.width/W;
    const back=offscreen(canvas.width,canvas.height),b=back.getContext('2d');
    b.setTransform(scale,0,0,scale,0,0);
    if(art.background)draw(b,'background',[0,0,W,H]);else{b.fillStyle='#272235';b.fillRect(0,0,W,H);}
    const front=offscreen(canvas.width,canvas.height),f=front.getContext('2d');
    f.setTransform(scale,0,0,scale,0,0);
    for(const [name,rect] of MUGS)draw(f,name,rect);
    draw(f,'microphones',[0,0,W,H]);
    const people=hosts.map(host=>{
      const box=union(host.at,host.phones),layer=offscreen(box[2]*scale,box[3]*scale),g=layer.getContext('2d');
      g.setTransform(scale,0,0,scale,-box[0]*scale,-box[1]*scale);
      draw(g,host.name,host.at);draw(g,host.name+'-phones',host.phones);
      return {layer,box};
    });
    layers={back,front,people};
  }

  /* Blit the matching part of a full-canvas layer. */
  function blit(layer,r){
    const x=Math.max(0,r[0]),y=Math.max(0,r[1]),w=Math.min(W,r[0]+r[2])-x,h=Math.min(H,r[1]+r[3])-y;
    if(w>0&&h>0)ctx.drawImage(layer,x*scale,y*scale,w*scale,h*scale,x,y,w,h);
  }
  function facePatch(name,r){
    if(!art[name])return;
    ctx.save();ctx.beginPath();ctx.ellipse(r[0]+r[2]/2,r[1]+r[3]/2,r[2]/2,r[3]/2,0,0,Math.PI*2);ctx.clip();
    ctx.drawImage(art[name],...r);ctx.restore();
  }

  /* Everything that can change from frame to frame, reduced to what it looks
     like. A region repaints only when its entry differs from the last paint. */
  function state(){
    const reduced=$('reduced').checked,t=clock,rain=$('rain-enabled').checked,lights=$('lights-enabled').checked;
    const requested=$('cat-life').dataset.pose;
    const pose=reduced||!$('cat-enabled').checked||!art['cat-'+requested]?'sleep':requested;
    const breath=reduced?0:Math.round(4*.7*(1-Math.cos(t*Math.PI*2/4.8)))/4;
    const zzz=pose==='sleep'?(reduced?'still':Math.round((t%4)*8)):'';
    const next={
      window:reduced||(!rain&&!lights)?'still':[rain&&Math.round(rainClock*15),lights&&Math.round(t*4),Math.round(energy*20)].join(),
      cat:[pose,breath,zzz].join(),
    };
    hosts.forEach((host,i)=>{
      const breathe=reduced?0:Math.round(861*.0016*(1-Math.cos(t*Math.PI*2/(5.4+i*.6)+i))*4)/4;
      const blink=!reduced&&(t+(i?1.7:0))%(i?6.7:5.1)<.14;
      next[host.name]=[breathe,speaking[host.name],blink].join();
    });
    return {next,reduced,rain,lights,pose,t};
  }

  function paintRegion(r,s){
    ctx.save();ctx.beginPath();ctx.rect(...r);ctx.clip();
    blit(layers.back,r);
    if(!s.reduced&&(s.rain||s.lights)){
      ctx.save();ctx.beginPath();for(const w of WINDOWS)ctx.rect(...w);ctx.clip();
      // The music leans on the room a little: the city brightens and the
      // rain thickens with the bass. Never with reduced motion (none of this
      // is drawn then).
      if(s.lights){
        LIGHTS.forEach(([x,y],i)=>{
          ctx.fillStyle=`rgba(255,194,109,${.06+.10*(.5+.5*Math.sin(s.t/(5+i*.4)+i))+.07*energy})`;ctx.fillRect(x,y,9,14);
        });
      }
      if(s.rain){
        ctx.lineWidth=1.1;ctx.strokeStyle=`rgba(173,192,220,${(.19+.08*energy).toFixed(3)})`;
        const drops=64+Math.round(20*energy);
        for(let i=0;i<drops;i++){const x=468+(i*79.73)%660,y=(i*49.17+rainClock*(42+i%6*7))%650-25;ctx.beginPath();ctx.moveTo(x,y);ctx.lineTo(x-2,y+11+i%9);ctx.stroke();}
      }
      ctx.restore();
    }
    const breath=s.reduced?0:.7*(1-Math.cos(s.t*Math.PI*2/4.8));
    draw(ctx,'cat-'+s.pose,[CAT[0],CAT[1]-breath,CAT[2],CAT[3]+breath]);
    if(s.pose==='sleep'){
      ctx.font='18px monospace';
      for(const phase of [0,2]){const k=s.reduced?.4:(s.t+phase)%4/4;ctx.fillStyle=`rgba(204,191,200,${.6*Math.sin(Math.PI*k)})`;ctx.fillText('z',155+k*9,261-k*32);if(s.reduced)break;}
    }
    hosts.forEach((host,i)=>{
      ctx.save();const breathe=s.reduced?0:.0016*(1-Math.cos(s.t*Math.PI*2/(5.4+i*.6)+i));
      ctx.translate(...host.anchor);ctx.scale(1,1+breathe);ctx.translate(-host.anchor[0],-host.anchor[1]);
      const person=layers.people[i];ctx.drawImage(person.layer,...person.box);
      if(speaking[host.name])facePatch(host.name+'-mouth',host.mouth);
      if(!s.reduced&&(s.t+(i?1.7:0))%(i?6.7:5.1)<.14)host.eyes.forEach((r,e)=>facePatch(`${host.name}-eye-${e}`,r));
      ctx.restore();
    });
    blit(layers.front,r);
    ctx.restore();
  }

  function render(force=false){
    if(!ready||document.hidden||!visible)return;
    if(!layers)build();
    const s=state(),dirty=[];
    const full=force||forceFull;
    for(const [key,value] of Object.entries(s.next)){
      if(full||previous[key]!==value)dirty.push(REGION[key]||hosts.find(h=>h.name===key).region);
    }
    previous=s.next;forceFull=false;
    if(!dirty.length)return;
    ctx.setTransform(scale,0,0,scale,0,0);
    // Past about half the scene, one full pass is cheaper than overlapping ones.
    const area=dirty.reduce((sum,r)=>sum+r[2]*r[3],0);
    for(const r of full||area>W*H*.5?[[0,0,W,H]]:dirty){
      // Whole device pixels, so neighbouring repaints never leave a seam.
      const x=Math.floor(r[0]*scale)/scale,y=Math.floor(r[1]*scale)/scale;
      paintRegion([x,y,Math.ceil((r[0]+r[2])*scale)/scale-x,Math.ceil((r[1]+r[3])*scale)/scale-y],s);
    }
    canvas.dataset.ready='true';canvas.dataset.pose=s.pose;
    canvas.dataset.speaking=['mav','rue'].filter(h=>speaking[h]).join(',');
  }

  const observer=typeof IntersectionObserver==='function'
    ?new IntersectionObserver(entries=>{visible=entries[0].isIntersecting;if(visible)render();}):null;
  observer?.observe(canvas);
  window.StudioScene={
    get ready(){return ready;},
    layout:{W,H,hosts,cat:CAT,mugs:MUGS,sprites},
    /* About 15 frames a second while on air, 8 while stopped -- and then the
       rain holds still. A mouth opening paints straight away. */
    update(dt,activity,level=0,running=false){
      const talking=activity.mav!==speaking.mav||activity.rue!==speaking.rue;
      speaking=activity;onAir=running;
      if(document.hidden||!visible)return;
      const reduced=$('reduced').checked;
      energy=reduced||!running?0:Math.max(0,Math.min(1,level||0));
      if(!reduced){clock+=Math.max(0,Math.min(dt,.1));if(running)rainClock+=Math.max(0,Math.min(dt,.1));}
      const now=performance.now();
      if(!talking&&now-lastRender<(onAir?66:125))return;
      lastRender=now;render();
    },
  };
  for(const id of ['rain-enabled','lights-enabled','cat-enabled','reduced'])$(id).addEventListener('change',()=>render(true));
  // Each layer loads on its own; whatever arrives is drawn. A missing mug is
  // not a reason to show nobody at the desk.
  Promise.allSettled(sprites.map(name=>new Promise((resolve,reject)=>{
    const image=new Image();image.decoding='async';
    image.onload=()=>{art[name]=image;resolve();};image.onerror=()=>reject(Error('Studio artwork unavailable: '+name));image.src=spriteUrl(name);
  }))).then(results=>{
    const failed=results.filter(r=>r.status==='rejected');
    failed.forEach(r=>console.warn(r.reason));
    if(failed.length===results.length){$('studio-status').textContent='Studio artwork unavailable. Audio is unaffected.';return;}
    ready=true;layers=null;render(true);
  });
  window.addEventListener('pagehide',()=>observer?.disconnect(),{once:true});
})();
