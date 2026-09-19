// Real ad controls and gain updates, with audio/network replaced at the boundary.
const fs = require('node:fs'), vm = require('node:vm'), assert = require('node:assert/strict');
const source = fs.readFileSync('web/static/radio.js', 'utf8');
const functions = source.slice(source.indexOf('function refreshScheduledGains()'), source.indexOf('function stopAll()'));
const calls = [], handlers = {};
const context = {
  running: true, ctx: {currentTime: 100}, clockOffset: 80,
  stationNow: () => 20, items: [], scheduled: new Map(),
  applyEnvelope: (...args) => calls.push(args),
  ui: {adNext: {addEventListener: (_,f) => handlers.next=f}, adNow: {addEventListener: (_,f) => handlers.now=f}, adStatus: {}},
  api: async (path, options) => {
    calls.push([path,JSON.parse(options.body)]);
    return {ad:{state:'preparing',busy:true,message:'Preparing'}};
  },
};
vm.createContext(context); vm.runInContext(functions,context);
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
  console.log('Browser ad controls and live ducking checks passed.');
})().catch(error=>{console.error(error);process.exitCode=1});
