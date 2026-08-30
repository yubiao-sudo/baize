// 冒烟测试：模拟 AudioContext，验证 sound.ts 全部音效配方可无异常调度。
import { playSfx, previewNotify, isSfxEnabled, setSfxEnabled, getSfxVolume, setSfxVolume, getNotifyStyle, setNotifyStyle } from "../src/utils/sound.ts";

function param(initial = 0) {
  return {
    value: initial,
    setValueAtTime() {}, linearRampToValueAtTime() {}, exponentialRampToValueAtTime() {},
    cancelScheduledValues() {},
  };
}
function node(extra = {}) {
  return {
    gain: param(1), frequency: param(440), detune: param(0), Q: param(1),
    pan: param(0), threshold: param(0), knee: param(0), ratio: param(1),
    attack: param(0), release: param(0), delayTime: param(0), type: "",
    connect() {}, disconnect() {}, start() {}, stop() {},
    ...extra,
  };
}
class MockAudioContext {
  constructor() { this.state = "running"; this.currentTime = 0; this.sampleRate = 44100; this.destination = node(); }
  resume() { this.state = "running"; return Promise.resolve(); }
  close() { return Promise.resolve(); }
  createGain() { return node(); }
  createDynamicsCompressor() { return node(); }
  createDelay() { return node(); }
  createBiquadFilter() { return node(); }
  createStereoPanner() { return node(); }
  createOscillator() { return node(); }
  createBufferSource() { return node(); }
  createBuffer(ch, len, sr) { return { getChannelData: () => new Float32Array(len) }; }
}
const store = new Map();
globalThis.window = {
  AudioContext: MockAudioContext,
  localStorage: {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
  },
};
globalThis.localStorage = globalThis.window.localStorage;

const events = ["startup", "notify", "permission", "card-pop", "task-done", "error", "escalation", "message-sent"];
let failed = 0;
for (const ev of events) {
  try {
    for (let i = 0; i < 5; i++) playSfx(ev); // 每种连放 5 次覆盖随机分支
    console.log(`PASS playSfx(${ev})`);
  } catch (e) {
    failed++;
    console.log(`FAIL playSfx(${ev}): ${e && e.stack}`);
  }
}
for (const s of ["windchime", "arp", "soft"]) {
  try {
    previewNotify(s);
    console.log(`PASS previewNotify(${s})`);
  } catch (e) {
    failed++;
    console.log(`FAIL previewNotify(${s}): ${e && e.stack}`);
  }
}
// 设置 API 往返
setSfxEnabled(false); if (isSfxEnabled() !== false) { failed++; console.log("FAIL enable roundtrip"); }
setSfxEnabled(true); if (!isSfxEnabled()) { failed++; console.log("FAIL enable roundtrip 2"); }
setSfxVolume(0.42); if (Math.abs(getSfxVolume() - 0.42) > 1e-9) { failed++; console.log("FAIL volume roundtrip: " + getSfxVolume()); }
setNotifyStyle("soft"); if (getNotifyStyle() !== "soft") { failed++; console.log("FAIL style roundtrip"); }
setNotifyStyle("arp"); if (getNotifyStyle() !== "arp") { failed++; console.log("FAIL style roundtrip 2"); }
setNotifyStyle("windchime"); if (getNotifyStyle() !== "windchime") { failed++; console.log("FAIL style roundtrip 3"); }
// 关闭状态下不应出声也不应抛错
setSfxEnabled(false);
playSfx("task-done");
setSfxEnabled(true);
console.log(failed === 0 ? "ALL SMOKE TESTS PASSED" : `FAILED: ${failed}`);
process.exit(failed === 0 ? 0 : 1);
