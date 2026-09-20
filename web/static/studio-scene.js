/* Layered booth renderer. Consumes host activity; never starts or schedules audio. */
(() => {
  'use strict';
  const $=id=>document.getElementById(id);
  const canvas=$('studio-scene'), ctx=canvas.getContext('2d');
  const W=1728,H=1152,art={};
  const files={background:'background.png',mav:'man.png',rue:'woman.png',mavPhones:'headphones-mav.png',ruePhones:'headphones-rue.png',
    microphones:'microphones.png',blackMug:'black-mug.png',whiteMug:'white-mug.png',sleep:'sleeping-cat.png',awake:'cat-awake.png',yawn:'cat-yawn.png',groom:'cat-groom.png',speaking:'speaking.jpg',blink:'blink.png'};
  const hosts=[
    {name:'mav',at:[101,183,781,861],phones:[383,185,328,296],anchor:[485,1005],mouth:[585,463,89,51],eyes:[[550,374,64,40],[634,372,48,41]]},
    {name:'rue',at:[910,213,681,824],phones:[1077,216,304,305],anchor:[1250,1010],mouth:[1252,477,84,55],eyes:[[1224,391,71,47],[1323,405,54,48]]}
  ];
  const catCrops={sleep:[1,3,264,137],awake:[58,33,1635,848],yawn:[19,10,1678,888],groom:[40,14,1641,878]};
  let clock=0,speaking={mav:false,rue:false},visible=true,ready=false,lastPaint='';
  function draw(image,rect){ctx.drawImage(image,...rect);}
  function facePatch(image,r){
    ctx.save();ctx.beginPath();ctx.ellipse(r[0]+r[2]/2,r[1]+r[3]/2,r[2]/2,r[3]/2,0,0,Math.PI*2);ctx.clip();
    ctx.drawImage(image,r[0]*image.naturalWidth/W,r[1]*image.naturalHeight/H,r[2]*image.naturalWidth/W,r[3]*image.naturalHeight/H,...r);ctx.restore();
  }
  function render(){
    if(!ready||document.hidden||!visible)return;
    const reduced=$('reduced').checked,t=clock;
    const signature=JSON.stringify([reduced?0:t,reduced,speaking.mav,speaking.rue,$('cat-life').dataset.pose,$('cat-enabled').checked,$('rain-enabled').checked,$('lights-enabled').checked]);
    if(signature===lastPaint)return;
    lastPaint=signature;
    ctx.clearRect(0,0,W,H);draw(art.background,[0,0,W,H]);
    if(!reduced){
      ctx.save();ctx.beginPath();ctx.rect(466,0,243,618);ctx.rect(726,0,405,618);ctx.clip();
      if($('lights-enabled').checked){
        [[771,389],[818,474],[905,383],[987,430],[1000,514],[748,436]].forEach(([x,y],i)=>{
          ctx.fillStyle=`rgba(255,194,109,${.06+.10*(.5+.5*Math.sin(t/(5+i*.4)+i))})`;ctx.fillRect(x,y,9,14);
        });
      }
      if($('rain-enabled').checked){ctx.lineWidth=1.1;ctx.strokeStyle='rgba(173,192,220,.19)';
        for(let i=0;i<64;i++){const x=468+(i*79.73)%660,y=(i*49.17+t*(42+i%6*7))%650-25;ctx.beginPath();ctx.moveTo(x,y);ctx.lineTo(x-2,y+11+i%9);ctx.stroke();}
      }ctx.restore();
    }
    const requested=$('cat-life').dataset.pose;
    const pose=reduced||!$('cat-enabled').checked||!catCrops[requested]?'sleep':requested;
    const breath=reduced?0:.7*(1-Math.cos(t*Math.PI*2/4.8));
    ctx.drawImage(art[pose],...catCrops[pose],49,251-breath,264,137+breath);
    if(pose==='sleep'){
      ctx.font='18px monospace';
      for(const phase of [0,2]){const k=reduced?.4:(t+phase)%4/4;ctx.fillStyle=`rgba(204,191,200,${.6*Math.sin(Math.PI*k)})`;ctx.fillText('z',155+k*9,261-k*32);if(reduced)break;}
    }
    hosts.forEach((host,i)=>{
      ctx.save();const breathe=reduced?0:.0016*(1-Math.cos(t*Math.PI*2/(5.4+i*.6)+i));
      ctx.translate(...host.anchor);ctx.scale(1,1+breathe);ctx.translate(-host.anchor[0],-host.anchor[1]);
      draw(art[host.name],host.at);draw(art[host.name+'Phones'],host.phones);
      if(speaking[host.name])facePatch(art.speaking,host.mouth);
      if(!reduced&&(t+(i?1.7:0))%(i?6.7:5.1)<.14)host.eyes.forEach(r=>facePatch(art.blink,r));
      ctx.restore();
    });
    draw(art.blackMug,[416,892,164,175]);draw(art.whiteMug,[1095,921,184,175]);draw(art.microphones,[0,0,W,H]);
    canvas.dataset.ready='true';canvas.dataset.pose=pose;
    canvas.dataset.speaking=['mav','rue'].filter(h=>speaking[h]).join(',');
  }
  const observer=new IntersectionObserver(entries=>{visible=entries[0].isIntersecting;if(visible)render();});
  observer.observe(canvas);
  window.StudioScene={get ready(){return ready;},update(dt,activity){
    speaking=activity;
    if(document.hidden||!visible)return;
    if(!$('reduced').checked)clock+=Math.max(0,Math.min(dt,.1));
    render();
  }};
  for(const id of ['rain-enabled','lights-enabled','cat-enabled','reduced'])$(id).addEventListener('change',render);
  Promise.all(Object.entries(files).map(([key,file])=>new Promise((resolve,reject)=>{
    const image=new Image();image.onload=()=>{art[key]=image;resolve();};image.onerror=()=>reject(Error('Studio artwork unavailable: '+file));image.src='/static/studio-v2/'+file;
  }))).then(()=>{ready=true;render();}).catch(error=>{$('studio-status').textContent='Studio artwork unavailable. Audio is unaffected.';console.error(error);});
  window.addEventListener('pagehide',()=>observer.disconnect(),{once:true});
})();
