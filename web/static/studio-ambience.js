/* Decorative studio life. Local assets only; never touches audio playback. */
(() => {
  'use strict';
  const $ = id => document.getElementById(id);
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
  const motionAllowed = () => !$('reduced').checked && $('cat-enabled').checked;
  function rest() { sequence = null; cat.dataset.pose = 'sleep'; }
  function begin(name) {
    if (!motionAllowed() || !window.StudioScene?.ready) return;
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
