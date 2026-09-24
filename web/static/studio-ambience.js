/* Decorative studio life: the settings, the cat's buttons and what the note
   beside them says. The cat itself lives in the booth (studio-scene.js).
   Local assets only; never touches audio playback. */
(() => {
  'use strict';
  const $ = id => document.getElementById(id);
  const cat = $('cat-life');
  let visible = true, bag = [], lastRoutine = '', showing = '';
  const labels = {
    ear: 'Something twitched. Still asleep, allegedly.',
    'ear-back': 'Listening to the show with one ear.',
    tail: 'Dreaming about a station with fewer opinions.',
    wake: 'A very demanding shift of doing absolutely nothing.',
    groom: 'Microphones on. Face wash in progress.',
    stretch: 'Wake up. Stretch. Back to work, apparently.',
    perk: 'You have been noticed. You have been approved.',
  };
  const motionAllowed = () => !$('reduced').checked && $('cat-enabled').checked;
  function choose() {
    if (!bag.length) {
      bag = ['ear','ear-back','tail','wake','groom','stretch'];
      for (let i=bag.length-1;i>0;i--) { const j=Math.floor(Math.random()*(i+1)); [bag[i],bag[j]]=[bag[j],bag[i]]; }
      if (bag[bag.length-1] === lastRoutine) [bag[0],bag[bag.length-1]]=[bag[bag.length-1],bag[0]];
    }
    return (lastRoutine = bag.pop());
  }
  function note() {
    if (!motionAllowed()) {
      return $('reduced').checked ? 'Reduced motion is on. Turn it off to watch the studio come alive.' : 'Cat antics paused. A well-earned nap.';
    }
    return labels[window.StudioScene?.catClip?.() || ''] || 'Sleeping on the job.';
  }
  function sync() {
    for (const type of ['rain','lights','cat']) document.body.classList.toggle(`${type}-disabled`, !$(`${type}-enabled`).checked);
    document.body.classList.toggle('scene-paused', document.hidden || !visible);
    $('cat-preview').disabled = !motionAllowed();
    $('cat-pet').disabled = !motionAllowed();
    tell();
  }
  function tell() {
    const text = note();
    if (text !== showing) { showing = text; $('cat-note').textContent = text; }
    const clip = window.StudioScene?.catClip?.() || '';
    if (cat.dataset.pose !== (clip || 'sleep')) cat.dataset.pose = clip || 'sleep';
  }
  for (const id of ['rain-enabled','lights-enabled','cat-enabled','reduced']) $(id).addEventListener('change', sync);
  matchMedia('(prefers-reduced-motion: reduce)').addEventListener('change', sync);
  document.addEventListener('visibilitychange', sync);
  const observer = new IntersectionObserver(entries => { visible=entries[0].isIntersecting; sync(); }, {threshold:0});
  observer.observe(document.querySelector('.studio'));
  $('cat-preview').addEventListener('click', () => {
    if (!motionAllowed() || window.StudioScene?.catClip?.()) return;
    window.StudioScene?.catPlay?.(choose());
    tell();
    document.querySelector('.studio').scrollIntoView({behavior:'instant',block:'start'});
  });
  // Repeated taps never restart or pile up reactions: the booth ignores a
  // poke while the cat is already up to something.
  $('cat-pet').addEventListener('click', () => { window.StudioScene?.catPoke?.(); tell(); });
  window.StudioAmbience = {
    tick() {
      if (document.hidden || !visible) return;
      tell();
    },
  };
  sync();
  window.addEventListener('pagehide', () => observer.disconnect(), {once:true});
})();
