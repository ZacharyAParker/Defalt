// Real ad controls and gain updates, with audio/network replaced at the boundary.
const vm = require('node:vm'), assert = require('node:assert/strict');
const {section} = require('./browser_harness');
const helpers = section('web/static/radio.js', 'const clamp =', 'function playbackCurve(');
const functions = section('web/static/radio.js', 'function refreshScheduledGains()', 'function stopAll()');
const calls = [], handlers = {};
const context = {
  MEMORY_CAP: 400,
  running: true, ctx: {currentTime: 100}, clockOffset: 80,
  stationNow: () => 20, items: [], scheduled: new Map(),
  applyEnvelope: (...args) => calls.push(args),
  ui: {adNext: {addEventListener: (_,f) => handlers.next=f}, adNow: {addEventListener: (_,f) => handlers.now=f}, adStatus: {}},
  api: async (path, options) => {
    calls.push([path,JSON.parse(options.body)]);
    return {ad:{state:'preparing',busy:true,message:'Preparing'}};
  },
};
vm.createContext(context); vm.runInContext(helpers + functions, context);
(async () => {
  await handlers.next();
  assert.equal(calls[0][0],'/api/ads'); assert.equal(calls[0][1].timing,'next_break');
  assert.equal(context.ui.adNow.disabled,true);
  await handlers.now();
  assert.equal(calls[1][1].timing,'now');
  context.running=false; context.updateAdControls();
  assert.equal(context.ui.adNext.disabled,true);
  context.running=true;
  context.api=async()=>{throw Error('Voices unavailable')};
  await context.requestAd('now');
  assert.equal(context.ui.adStatus.textContent,'Voices unavailable');
  assert.equal(context.ui.adNow.disabled,false);
  calls.length=0;
  const old={id:'music', start_at:0, envelope:[[0,1],[100,1]]};
  const next={...old,envelope:[[0,1],[25,1],[26,.1],[35,.1],[36,1],[100,1]]};
  const entry={item:old,gain:{gain:'gain-param'},source:{stop(){throw Error('Music restarted')}}};
  context.items=[next]; context.scheduled.set('music',entry);
  context.refreshScheduledGains();
  assert.equal(calls.length,1); assert.equal(calls[0][0],'gain-param');
  assert.equal(calls[0][2],100); assert.equal(calls[0][3],20);
  context.refreshScheduledGains(); assert.equal(calls.length,1);

  // An item keeps the clock offset it was scheduled with, even after the
  // shared clock is re-anchored for drift.
  calls.length=0;
  entry.offset=78; context.clockOffset=80.4;
  context.items=[{...next,envelope:[[0,1],[40,.5],[100,1]]}];
  context.refreshScheduledGains();
  assert.equal(calls[0][2],100); assert.equal(calls[0][3],22,'offset measured on the entry\'s own clock');

  // trim_db rides on top of the envelope for music only.
  const trimmed={kind:'music',meta:{trim_db:-6},envelope:[[0,0],[10,1]]};
  const env=context.itemEnvelope(trimmed);
  assert.equal(env[0][1],0); assert.ok(Math.abs(env[1][1]-0.501)<0.001);
  assert.equal(context.itemEnvelope({kind:'voice',meta:{trim_db:-6},envelope:[[0,1]]})[0][1],1,'speech is never trimmed');
  assert.equal(context.itemEnvelope({kind:'music',meta:{trim_db:40},envelope:[[0,1]]})[0][1],10**(12/20),'a wild trim is clamped');
  assert.equal(context.itemEnvelope({kind:'music',meta:{},envelope:[[0,1]]})[0][1],1);
  calls.length=0;
  entry.item={...context.items[0]};
  context.items=[{...context.items[0],kind:'music',meta:{trim_db:-3}}];
  context.refreshScheduledGains();
  assert.equal(calls.length,1,'a new trim re-applies the gain automation');
  assert.ok(Math.abs(calls[0][1][0][1]-10**(-3/20))<1e-9);
  console.log('Browser ad controls, live ducking and loudness trim checks passed.');
})().catch(error=>{console.error(error);process.exitCode=1});
