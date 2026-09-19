/* Decorative studio life. Local assets only; never touches audio playback. */
(() => {
  'use strict';
  const $ = id => document.getElementById(id);
  const ns = 'http://www.w3.org/2000/svg';
  const cat = $('cat-life');
  let visible = true, clock = 0, sequenceStart = 0, sequence = null;
  const napLength = () => 90 + Math.random()*90;
  let nextVisit = napLength(), nextReaction = 120, previousBoth = false;
  let bag = [], lastRoutine = '';
  const routines = {
    peek: {label: 'Checking whether either host has snacks.', frames: [[0,'awake'],[2.6,'sleep'],[3.1,'awake'],[4.5,'sleep']], duration: 5},
    yawn: {label: 'A very demanding shift of doing absolutely nothing.', frames: [[0,'awake'],[.7,'yawn'],[2.2,'awake'],[3.2,'sleep']], duration: 4},
    wash: {label: 'Microphones on. Face wash in progress.', frames: [[0,'awake'],[.8,'groom'],[1.6,'awake'],[2,'groom'],[2.8,'awake'],[3.2,'groom'],[4.1,'awake'],[5,'sleep']], duration: 5.5},
    doze: {label: 'Trying very hard to stay awake. Losing.', frames: [[0,'awake'],[1.4,'sleep'],[2.1,'awake'],[3,'sleep'],[3.5,'awake'],[4.1,'sleep']], duration: 5},
    dream: {label: 'Dreaming about a station with fewer opinions.', frames: [[0,'sleep']], duration: 9},
    blink: {label: 'One slow blink. You have been approved.', frames: [[0,'awake'],[1.8,'sleep'],[2.15,'awake'],[3.8,'sleep']], duration: 4.5},
    stretch: {label: 'Wake up. Yawn. Wash. Back to work, apparently.', frames: [[0,'awake'],[1,'yawn'],[2.5,'awake'],[3.3,'groom'],[4.4,'awake'],[5.6,'sleep']], duration: 6},
    reaction: {label: 'Both hosts at once? The cat would like a word.', frames: [[0,'awake'],[2.4,'sleep']], duration: 3},
  };
  const ready = new Set();
  document.querySelectorAll('.cat-frame').forEach(img => {
    const loaded = () => { if (img.naturalWidth) ready.add(img); };
    if (img.complete) loaded(); else img.addEventListener('load', loaded, {once:true});
    img.addEventListener('error', () => { $('cat-note').textContent = 'Cat frames unavailable. The cat is sleeping this one out.'; });
  });

  function rect(parent, x, y, width, height, className, duration, delay) {
    const shape = document.createElementNS(ns, 'rect');
    for (const [key, value] of Object.entries({x,y,width,height,class:className})) shape.setAttribute(key, value);
    shape.style.animationDuration = `${duration}s`;
    shape.style.animationDelay = `${delay}s`;
    parent.append(shape);
  }
  // Fixed seed makes the composition reproducible without synchronized drops.
  let seed = 719;
  function random() { seed = (seed * 16807) % 2147483647; return (seed - 1) / 2147483646; }
  for (let i = 0; i < 66; i++) {
    rect($('window-rain'), 460 + random()*680, 80 + random()*440,
         i % 4 ? 1.5 : 2, 8 + random()*19, 'rain-streak', 1.7 + random()*1.8, -random()*5);
  }
  for (let i = 0; i < 9; i++) {
    rect($('window-rain'), 485 + random()*620, 140 + random()*190,
         2, 5 + random()*11, 'glass-drop', 7 + random()*7, -random()*15);
  }
  [[675,274,7,11],[726,293,9,12],[706,352,13,16],[814,320,11,19],
   [917,315,13,17],[895,229,10,12],[1047,252,7,12],[1110,260,7,10],
   [752,388,9,12],[818,386,8,11],[754,359,6,10],[658,392,5,12],
   [679,318,5,10],[863,395,6,9],[780,386,4,7]].forEach(([x,y,w,h], i) => {
    rect($('city-lights'), x,y,w,h,'city-lamp', 4.5 + random()*8, -i*1.7);
  });
  for (const x of [712,755,817,866]) {
    for (let i = 0; i < 5; i++) rect($('city-lights'), x-6+(i%2)*3,432+i*12,13-i,2,
      'water-light', 4 + random()*4, -random()*8);
  }

  const motionAllowed = () => !$('reduced').checked && $('cat-enabled').checked;
  function rest() { sequence = null; cat.dataset.pose = 'sleep'; }
  function begin(name) {
    if (!motionAllowed() || ready.size < 3) return;
    sequence = routines[name]; sequenceStart = clock; lastRoutine = name;
    nextReaction = clock + 120;
    cat.dataset.pose = sequence.frames[0][1];
    $('cat-note').textContent = sequence.label;
  }
  function choose() {
    if (!bag.length) {
      bag = ['peek','yawn','wash','doze','dream','blink','stretch'];
      for (let i=bag.length-1;i>0;i--) { const j=Math.floor(Math.random()*(i+1)); [bag[i],bag[j]]=[bag[j],bag[i]]; }
      if (bag[bag.length-1] === lastRoutine) [bag[0],bag[bag.length-1]]=[bag[bag.length-1],bag[0]];
    }
    return bag.pop();
  }
  function sync() {
    for (const type of ['rain','lights','cat']) document.body.classList.toggle(`${type}-disabled`, !$(`${type}-enabled`).checked);
    document.body.classList.toggle('scene-paused', document.hidden || !visible);
    $('cat-preview').disabled = !motionAllowed();
    $('cat-pet').disabled = !motionAllowed();
    if (!motionAllowed()) {
      rest(); nextVisit=clock+napLength();
      $('cat-note').textContent = $('reduced').checked ? 'Reduced motion is on. Turn it off to watch the studio come alive.' : 'Cat antics paused. A well-earned nap.';
    } else if (!sequence) $('cat-note').textContent = 'Sleeping on the job.';
  }
  for (const id of ['rain-enabled','lights-enabled','cat-enabled','reduced']) $(id).addEventListener('change', sync);
  matchMedia('(prefers-reduced-motion: reduce)').addEventListener('change', sync);
  document.addEventListener('visibilitychange', sync);
  const observer = new IntersectionObserver(entries => { visible=entries[0].isIntersecting; sync(); }, {threshold:0});
  observer.observe(document.querySelector('.studio'));
  $('cat-preview').addEventListener('click', () => {
    if (sequence) return;
    begin(choose());
    document.querySelector('.studio').scrollIntoView({behavior:'instant',block:'start'});
  });
  $('cat-pet').addEventListener('click', () => {
    if (sequence) return; // Repeated taps never restart or pile up reactions.
    begin('blink');
  });
  window.StudioAmbience = {
    tick(dt, bothTalking) {
      if (document.hidden || !visible || !motionAllowed()) return;
      clock += dt;
      if (bothTalking && !previousBoth && !sequence && clock > nextReaction && clock >= nextVisit) {
        begin('reaction');
      }
      previousBoth=bothTalking;
      if (sequence) {
        const age=clock-sequenceStart;
        if (age >= sequence.duration) {
          rest(); nextVisit=clock+napLength();
          $('cat-note').textContent='Sleeping on the job.';
        } else {
          for (const [at,pose] of sequence.frames) { if (age >= at) cat.dataset.pose=pose; }
        }
      } else if (clock >= nextVisit) {
        begin(choose());
        if (!sequence) nextVisit=clock+5;
      }
    },
  };
  rest(); sync();
  window.addEventListener('pagehide', () => observer.disconnect(), {once:true});
})();
