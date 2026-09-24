/* Layered booth renderer. Consumes host activity; never starts or schedules audio.

   Drawn in a 1728x1152 scene space onto a bitmap sized to the element (CSS box
   x devicePixelRatio), so the art is resampled once, smoothly, instead of the
   browser shrinking a full-size bitmap every frame. The layers that never move
   (the room behind, the mugs and microphones in front, each host with their
   headphones) are composited once into offscreen canvases; a frame only
   repaints the regions whose state changed, from those. What moves and when
   is studio-motion.js, the same machinery the desktop booth runs. */
(() => {
  'use strict';
  const $=id=>document.getElementById(id);
  const M=window.StudioMotion;
  const canvas=$('studio-scene'), ctx=canvas.getContext('2d');
  const W=1728,H=1152,art={};
  const version=(/[?&]v=([^&]+)/.exec(document.currentScript?.src||'')||[])[1]||'';
  const tag=version?`?v=${version}`:'';
  const spriteUrl=name=>`/static/studio-v2/web/${name}.webp${tag}`;
  // Built by tools/build-web-art.py (the layers) and tools/build-studio-frames.py
  // (the atlas of everything that moves, and frames.json saying where).
  const sprites=['background','microphones','mav','rue','mav-phones','rue-phones','black-mug','white-mug','atlas'];
  const hosts=[
    {name:'mav',at:[101,183,781,861],phones:[383,185,328,296],neck:545,shoulders:610,desk:1005},
    {name:'rue',at:[910,213,681,824],phones:[1077,216,304,305],neck:575,shoulders:650,desk:1010}
  ];
  const CAT=[49,251,264,137], MUGS=[['black-mug',[416,892,164,175]],['white-mug',[1095,921,184,175]]];
  // Regions a frame may need to repaint, in scene space.
  const REGION={window:[455,0,663,614],sign:[1226,62,282,152],lamp:[0,0,300,400],cat:[0,150,360,250],
    steam0:[455,780,110,140],steam1:[1120,810,110,150]};
  hosts.forEach(h=>{h.region=[Math.min(h.at[0],h.phones[0])-4,Math.min(h.at[1],h.phones[1])-8,h.at[2]+8,h.at[3]+12];});
  let frames=null,clock=0,energy=0,beat=0,onAir=false,visible=true,ready=false;
  let scale=1,layers=null,previous={},lastRender=0,forceFull=true;
  let seed=(Math.random()*4294967295)>>>0;
  const life={};
  function reseed(){
    life.lips=[new M.Lips(),new M.Lips()];
    life.eyes=[new M.Eyes(seed*3,true),new M.Eyes(seed*5,false)];
    life.looks=[{lid:0,glance:0},{lid:0,glance:0}];
    life.cat=new M.Cat(seed*7,clock);life.pose={frame:0,asleep:true};life.snoring=1;
    life.rain=new M.Rain(seed*11);life.city=new M.Lights(seed*13,frames?frames.lights.length:0);
    life.steam=new M.Steam(seed*17);life.sign=new M.Sign();life.both=false;
  }
  reseed();
  const scratch=[0,0,0,0];

  function union(a,b){const x=Math.min(a[0],b[0]),y=Math.min(a[1],b[1]);return [x,y,Math.max(a[0]+a[2],b[0]+b[2])-x,Math.max(a[1]+a[3],b[1]+b[3])-y];}
  function draw(g,name,rect){if(art[name])g.drawImage(art[name],...rect);}
  function offscreen(w,h){const c=document.createElement('canvas');c.width=Math.max(1,Math.ceil(w));c.height=Math.max(1,Math.ceil(h));return c;}
  function frame(item,x,y,w,h){
    if(!item||!art.atlas)return;const s=item.src,a=item.at;
    ctx.drawImage(art.atlas,s[0],s[1],s[2],s[3],x??a[0],y??a[1],w??a[2],h??a[3]);
  }
  const reduced=()=>$('reduced').checked;
  const enabled=id=>$(id)?.checked!==false;

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

  /* Move everything on by dt seconds. */
  function step(dt,voice){
    clock+=dt;const t=clock,calm=reduced();
    for(let i=0;i<2;i++)life.lips[i].step(dt,voice.levels[i],voice.tones[i],calm);
    const talking=[life.lips[0].talking(),life.lips[1].talking()];
    for(let i=0;i<2;i++){const l=life.eyes[i].step(t,talking[i],talking[1-i],calm);life.looks[i].lid=l.lid;life.looks[i].glance=l.glance;}
    const both=life.lips[0].viseme!==M.REST&&life.lips[1].viseme!==M.REST;
    if(both&&!life.both&&enabled('cat-enabled')&&!calm)life.cat.crowd(t);
    life.both=both;
    const pose=life.cat.step(t,enabled('cat-enabled'),calm);life.pose.frame=pose.frame;life.pose.asleep=pose.asleep;
    life.snoring+=Math.max(-dt/0.5,Math.min(dt/0.5,(pose.asleep?1:0)-life.snoring));
    if(!calm){
      life.rain.step(dt,t,energy);life.rain.storm(t,enabled('lightning-enabled')&&enabled('rain-enabled'));
      life.city.step(dt,t);life.steam.step(dt);
    }
    life.sign.step(t,onAir,calm);
  }

  /* Everything that can change from frame to frame, reduced to what it looks
     like. A region repaints only when its entry differs from the last paint. */
  function state(){
    const calm=reduced(),t=clock,rain=enabled('rain-enabled'),lights=enabled('lights-enabled');
    const heads=hosts.map((host,i)=>{
      if(calm)return [0,0];
      const lips=life.lips[i],chest=M.breath(t,i),bob=lips.talking()?0:0.9*beat;
      return [Math.round(chest*4)/4,Math.round((chest+lips.nod()+bob)*scale)/scale];
    });
    const burn=life.sign.level*life.sign.pulse(t,calm);
    const next={
      window:calm||(!rain&&!lights)?'still':[rain&&Math.round(t*30),lights&&Math.round(t*8),Math.round(energy*20)].join(),
      sign:Math.round(burn*60),
      lamp:calm?'still':Math.round(life.city.lamp(t)*100),
      cat:[life.pose.frame,calm?'still':Math.round(t*12),Math.round(life.snoring*10)].join(),
      steam0:calm?'still':Math.round(t*15),steam1:calm?'still':Math.round(t*15),
    };
    hosts.forEach((host,i)=>{const l=life.looks[i];next[host.name]=[...heads[i],life.lips[i].viseme,l.lid,l.glance].join();});
    return {next,calm,rain,lights,t,heads,burn};
  }

  function paintRain(){
    const rain=life.rain,weight=0.85+0.4*rain.weight;
    ctx.save();ctx.beginPath();ctx.rect(...M.WINDOW);ctx.clip();
    for(let layer=0;layer<3;layer++){
      const lean=rain.lean(layer),[, , , alpha,width]=M.LAYERS[layer];
      // Each streak in two halves: faint tail, brighter head.
      for(const half of [0,1]){
        ctx.beginPath();
        for(const d of rain.drops){
          if(d.layer!==layer)continue;
          const k0=half?0.5:1,k1=half?0:0.5;
          ctx.moveTo(d.x-lean*d.len*k0,d.y-d.len*k0);ctx.lineTo(d.x-lean*d.len*k1,d.y-d.len*k1);
        }
        const a=(alpha[0]+alpha[1])/2*weight*(half?1:0.4);
        ctx.lineWidth=width;ctx.strokeStyle=`rgba(173,192,220,${Math.min(1,a).toFixed(3)})`;ctx.stroke();
      }
    }
    ctx.lineWidth=1.2;
    for(const s of rain.splashes){
      if(s.age>=M.SPLASH_LIFE)continue;const k=s.age/M.SPLASH_LIFE;
      ctx.strokeStyle=`rgba(173,192,220,${((1-k)*0.6).toFixed(3)})`;ctx.beginPath();
      for(const side of [-1,1]){const x=s.x+side*(2+6*k)*s.size,y=604-7*Math.sin(Math.PI*k)*s.size;ctx.moveTo(x,y+1.2);ctx.lineTo(x,y-1.2);}
      ctx.stroke();
    }
    for(const b of rain.beads){
      if(b.state===0)continue;const r=b.r*1.7;
      ctx.globalAlpha=0.47;frame(frames.soft,b.x-r,b.y-r,r*2,r*2);
      if(b.state===2&&b.y-b.top>2){ctx.globalAlpha=0.27;frame(frames.soft,b.x-0.6,b.top,1.2,b.y-b.top);}
    }
    ctx.globalAlpha=1;ctx.restore();
  }

  function paintRegion(r,s){
    ctx.save();ctx.beginPath();ctx.rect(...r);ctx.clip();
    blit(layers.back,r);
    const t=s.t,f=frames;
    if(f&&!s.calm){
      if(s.lights){
        for(let i=0;i<life.city.count;i++){
          const L=f.lights[i],off=1-life.city.windows[i].on;
          if(off>0.01){ctx.globalAlpha=off*0.92;frame(L.dim);}
          ctx.globalCompositeOperation='lighter';ctx.globalAlpha=Math.min(1,life.city.glow(i,t,energy));frame(L.lit);
          ctx.globalCompositeOperation='source-over';
        }
        ctx.globalAlpha=1;
      }
      if(s.rain){
        const flash=life.rain.flash(t);
        if(flash>0){ctx.globalCompositeOperation='lighter';ctx.globalAlpha=Math.min(1,0.9*flash);frame(f.flash);ctx.globalCompositeOperation='source-over';ctx.globalAlpha=1;}
        paintRain();
        frame(f.window);
      }
    }
    if(f){
      // The sign: lit only while the station is, warming up with a flicker.
      const dark=Math.max(0,Math.min(1,1-s.burn));
      if(dark>0.004){ctx.globalAlpha=dark;frame(f.sign_off);ctx.globalAlpha=1;}
      if(s.burn>1){ctx.globalCompositeOperation='lighter';ctx.globalAlpha=Math.min(1,(s.burn-1)*3);frame(f.sign_glow);ctx.globalCompositeOperation='source-over';ctx.globalAlpha=1;}
      if(!s.calm){
        const lamp=life.city.lamp(t)-1,[lx,ly]=f.lamp;
        if(lamp>0){ctx.globalCompositeOperation='lighter';ctx.globalAlpha=Math.min(1,lamp*1.6);}
        else{ctx.filter='brightness(0)';ctx.globalAlpha=Math.min(1,-lamp*1.2);}
        frame(f.soft,lx-165,ly+30-150,330,300);
        ctx.globalCompositeOperation='source-over';ctx.globalAlpha=1;ctx.filter='none';
      }
      frame(f.cat[life.pose.frame]);
      if(life.snoring>0.01){
        const zs=s.calm?[0.45]:[0,1/3,2/3];
        for(const phase of zs){
          const k=s.calm?phase:((t/3.6+phase)%1+1)%1,[dx,dy,size,alpha]=M.zAt(k,scratch);
          ctx.globalAlpha=s.calm?0.5:alpha*life.snoring;const w=18*size;
          frame(f.z,f.snore[0]+dx-w/2,f.snore[1]+dy-w/2,w,w);
        }
        ctx.globalAlpha=1;
      }
    }
    hosts.forEach((host,i)=>{
      const person=layers.people[i],box=person.box,[chest,head]=s.heads[i];
      // Breathing from the chest; the head rides rigidly on top; arms on the desk stay put.
      const rows=[[box[1],head],[host.neck,head],[host.shoulders,chest],[host.desk,0],[box[1]+box[3],0]];
      for(let k=0;k<rows.length-1;k++){
        const [y0,d0]=rows[k],[y1,d1]=rows[k+1];if(y1<=y0)continue;
        ctx.drawImage(person.layer,0,(y0-box[1])*scale,box[2]*scale,(y1-y0)*scale,box[0],y0+d0,box[2],y1+d1-y0-d0);
      }
      if(!f)return;
      const face=item=>{if(item)frame(item,item.at[0],item.at[1]+head);};
      const lips=life.lips[i],look=life.looks[i],faces=f.faces[host.name];
      if(lips.viseme!==M.REST)face(faces.mouths[lips.viseme-1]);
      if(look.glance)face(faces.looks[look.glance-1]);
      if(look.lid)face(faces.lids[look.lid-1]);
    });
    blit(layers.front,r);
    if(f&&!s.calm){
      // Steam off both mugs, curling as it rises; in front of the mugs' rims.
      life.steam.puffs.forEach((puffs,m)=>{
        for(const p of puffs){
          const [x,y,rad,alpha]=M.puffAt(p,scratch),cx=f.mugs[m][0]+x,cy=f.mugs[m][1]+y;
          ctx.globalAlpha=alpha;frame(f.soft,cx-rad,cy-rad*1.2,rad*2,rad*2.4);
        }
      });
      ctx.globalAlpha=1;
    }
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
    canvas.dataset.ready='true';canvas.dataset.pose=M.CAT_FRAMES[life.pose.frame];
    canvas.dataset.speaking=hosts.filter((h,i)=>life.lips[i].viseme!==M.REST).map(h=>h.name).join(',');
  }

  const observer=typeof IntersectionObserver==='function'
    ?new IntersectionObserver(entries=>{visible=entries[0].isIntersecting;if(visible)render();}):null;
  observer?.observe(canvas);
  const quiet={levels:[0,0],tones:[0,0]};
  let pending=0;
  window.StudioScene={
    get ready(){return ready;},
    layout:{W,H,hosts,cat:CAT,mugs:MUGS,sprites},
    life,
    /* About 30 frames a second on air, 12 off air: the room keeps raining
       either way. A mouth changing shape paints straight away. */
    update(dt,activity,level=0,running=false,voice=quiet,pulse=0){
      onAir=running;
      if(document.hidden||!visible)return;
      const calm=reduced();
      energy=calm||!running?0:Math.max(0,Math.min(1,level||0));beat=calm||!running?0:Math.max(0,Math.min(1,pulse||0));
      const shapes=life.lips.map(l=>l.viseme).join();
      pending+=Math.max(0,Math.min(dt,.1));
      step(pending,voice);pending=0;
      const now=performance.now(),talking=shapes!==life.lips.map(l=>l.viseme).join();
      if(!talking&&now-lastRender<(onAir?31:78))return;
      lastRender=now;render();
    },
    /* The cat, for the buttons beside the booth. */
    catPlay(name){const c=M.CLIPS.findIndex(c=>c[0]===name);if(c>=0&&!reduced()&&enabled('cat-enabled'))life.cat.play(c,clock);},
    catPoke(){if(!reduced()&&enabled('cat-enabled'))life.cat.poke(clock);},
    catClip(){const c=life.cat.playing();return c===null?'':M.CLIPS[c][0];},
  };
  for(const id of ['rain-enabled','lights-enabled','cat-enabled','lightning-enabled','reduced'])$(id)?.addEventListener('change',()=>render(true));
  // Each layer loads on its own; whatever arrives is drawn. A missing mug is
  // not a reason to show nobody at the desk.
  const manifest=fetch(`/static/studio-v2/web/frames.json${tag}`).then(r=>r.ok?r.json():Promise.reject(Error('Studio frames unavailable'))).then(json=>{
    frames=json;life.city.count=Math.min(json.lights.length,64);
  });
  Promise.allSettled([manifest,...sprites.map(name=>new Promise((resolve,reject)=>{
    const image=new Image();image.decoding='async';
    image.onload=()=>{art[name]=image;resolve();};image.onerror=()=>reject(Error('Studio artwork unavailable: '+name));image.src=spriteUrl(name);
  }))]).then(results=>{
    const failed=results.filter(r=>r.status==='rejected');
    failed.forEach(r=>console.warn(r.reason));
    if(failed.length>=sprites.length){$('studio-status').textContent='Studio artwork unavailable. Audio is unaffected.';return;}
    if(!art.atlas)frames=null;
    ready=true;layers=null;render(true);
  });
  window.addEventListener('pagehide',()=>observer?.disconnect(),{once:true});
})();
