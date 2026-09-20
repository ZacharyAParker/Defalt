const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const toggle = {checked:true, addEventListener(_, fn) {this.change=fn;}}, panel = {};
const reduced = {checked:false};
const storage = new Map();
const document = {hidden:false, getElementById(id) {return {'visualizer-enabled':toggle,'audio-visualizer':panel,reduced}[id];}};
const window = {devicePixelRatio:1, matchMedia:()=>({matches:false})};
let bars = [], reads = 0;
const g = {setTransform(){},clearRect(){bars=[];},fillRect(...args){bars.push(args);}};
const canvas = {width:0,height:0,getBoundingClientRect:()=>({width:480,height:72}),getContext:()=>g};
const analyser = {context:{sampleRate:48000},fftSize:2048,frequencyBinCount:1024,
  getFloatFrequencyData(data) {reads++;data.fill(-Infinity);data[43]=-10;}};
vm.runInNewContext(fs.readFileSync('web/static/visualizer.js','utf8'), {
  document,window,Float32Array,performance:{now:()=>1000},localStorage:{getItem:k=>storage.get(k),setItem:(k,v)=>storage.set(k,v)}
});
window.RadioVisualizer.draw(canvas,analyser,1000);
assert.equal(reads,1);
assert.ok(bars.some(b=>b[3]>20),'a real frequency peak produces a tall bar');
assert.ok(bars.every(b=>b.every(Number.isFinite)),'silence bins never yield invalid canvas coordinates');
window.RadioVisualizer.draw(canvas,analyser,1010);
assert.equal(reads,1,'spectrum work is limited to 30 Hz');
toggle.checked=false;toggle.change();
assert.equal(panel.hidden,true);assert.equal(storage.get('defalt.visualizer'),'off');
window.RadioVisualizer.draw(canvas,analyser,2000);
assert.equal(reads,1,'disabled visualizer performs no analysis');
toggle.checked=true;toggle.change();document.hidden=true;
window.RadioVisualizer.draw(canvas,analyser,3000);
assert.equal(reads,1,'hidden page performs no analysis');
document.hidden=false;reduced.checked=true;
window.RadioVisualizer.draw(canvas,analyser,4000);
assert.equal(bars.length,48,'reduced motion removes the reflections and peak caps');
window.RadioVisualizer.draw(canvas,analyser,4050);
assert.equal(reads,2,'reduced motion is limited to 10 Hz');
for(let t=5000;t<15000;t+=200) window.RadioVisualizer.draw(canvas,null,t);
assert.ok(bars.every(b=>b[3]===2),'stopped playback settles to silence');
for (const width of [1324,1964]) {
  canvas.getBoundingClientRect=()=>({width,height:120});
  window.RadioVisualizer.draw(canvas,analyser,width+20000);
  assert.ok(bars.every(b=>b[0]>=0 && b[0]+b[2]<=width),'wide display stays within canvas');
  assert.ok(bars[0][2]>width/48*.7,'bars retain their share of the width on large displays');
}
console.log('browser visualizer: audio, silence, toggle, persistence, visibility and reduced motion passed');
