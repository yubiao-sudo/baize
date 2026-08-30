// 白泽全局音效引擎：WebAudio 实时合成，零外部音频文件。
// 每种事件一段独立「小乐句」：钟琴/木琴泛音列 + 噪声敲击起音 + 伪混响空间感，
// 并且每次播放都带随机微失谐与节奏抖动（humanize），重复听也不像电子蜂鸣器。
// 设置持久化到 localStorage，供 AcuiCard / App / chat store / 设置面板使用。

export type SfxEvent =
  | "startup" // 应用启动 · 觉醒
  | "notify" // 弹窗通知（主动提醒 / 定时提醒）
  | "permission" // 高危操作审批请求
  | "card-pop" // 受控卡片弹出
  | "task-done" // 任务完成
  | "error" // 出错
  | "escalation" // 通知升级警报
  | "voice-wake" // 语音唤醒 · 进入聆听（我在听）
  | "voice-handoff" // 问句交回 · 白泽提问后自动把话筒交还用户（该你说了）
  | "message-sent"; // 消息已发出（轻触感）

/** 弹窗通知的质感风格（沿用旧设置里的三档口味） */
export type NotifyStyle = "windchime" | "arp" | "soft";

const KEY_ENABLED = "baize.chime.enabled"; // 兼容旧键：曾存 "0"/"1"
const KEY_VOLUME = "baize.sfx.volume";
const KEY_STYLE = "baize.chime.kind"; // 兼容旧键：dingdong/bright/soft

// ---------------- 开关 / 音量 / 风格 ----------------

export function isSfxEnabled(): boolean {
  return localStorage.getItem(KEY_ENABLED) !== "0";
}

export function setSfxEnabled(on: boolean) {
  localStorage.setItem(KEY_ENABLED, on ? "1" : "0");
}

export function getSfxVolume(): number {
  const v = Number(localStorage.getItem(KEY_VOLUME));
  return Number.isFinite(v) && v > 0 ? Math.min(v, 1) : 0.9;
}

export function setSfxVolume(v: number) {
  localStorage.setItem(KEY_VOLUME, String(Math.max(0, Math.min(1, v))));
}

export function getNotifyStyle(): NotifyStyle {
  const v = localStorage.getItem(KEY_STYLE);
  return v === "bright" || v === "arp" ? "arp" : v === "soft" ? "soft" : "windchime";
}

export function setNotifyStyle(s: NotifyStyle) {
  localStorage.setItem(KEY_STYLE, s);
}

// ---------------- 音频图：主链 + 伪混响 ----------------

let ctx: AudioContext | null = null;
let master: GainNode | null = null;
let spaceIn: GainNode | null = null; // 湿声发送总线
let noiseBuf: AudioBuffer | null = null;

function ac(): AudioContext | null {
  try {
    if (!ctx) {
      const AC = window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
      if (!AC) return null;
      ctx = new AC();

      // 主链：master → 轻压限（叠音时防爆音）→ 输出
      master = ctx.createGain();
      master.gain.value = getSfxVolume();
      const comp = ctx.createDynamicsCompressor();
      comp.threshold.value = -16;
      comp.knee.value = 22;
      comp.ratio.value = 5;
      comp.attack.value = 0.004;
      comp.release.value = 0.24;
      master.connect(comp);
      comp.connect(ctx.destination);

      // 伪混响：反馈延迟 + 低频滚降，给声音一点「房间感」，去掉生硬的干
      spaceIn = ctx.createGain();
      spaceIn.gain.value = 1;
      const delay = ctx.createDelay(0.5);
      delay.delayTime.value = 0.107;
      const fb = ctx.createGain();
      fb.gain.value = 0.38;
      const damp = ctx.createBiquadFilter();
      damp.type = "lowpass";
      damp.frequency.value = 2600;
      const wet = ctx.createGain();
      wet.gain.value = 0.24;
      spaceIn.connect(delay);
      delay.connect(damp);
      damp.connect(fb);
      fb.connect(delay);
      damp.connect(wet);
      wet.connect(master);

      // 噪声源缓存（敲击起音用）
      const len = Math.floor(ctx.sampleRate * 0.25);
      noiseBuf = ctx.createBuffer(1, len, ctx.sampleRate);
      const data = noiseBuf.getChannelData(0);
      for (let i = 0; i < len; i++) data[i] = Math.random() * 2 - 1;
    }
    if (ctx.state === "suspended") void ctx.resume().catch(() => {});
    return ctx;
  } catch {
    return null;
  }
}

function out(node: AudioNode, c: AudioContext, space = 0.15, pan = 0) {
  // 干声（可带声像）+ 湿声发送
  let tail: AudioNode = node;
  if (pan !== 0 && c.createStereoPanner) {
    const p = c.createStereoPanner();
    p.pan.value = pan;
    node.connect(p);
    tail = p;
  }
  tail.connect(master!);
  if (space > 0 && spaceIn) {
    const send = c.createGain();
    send.gain.value = space;
    tail.connect(send);
    send.connect(spaceIn);
  }
}

// ---------------- 合成基元 ----------------

/** 人性化：每次播放 ±4 音分失谐、±10ms 起音抖动、±10% 力度，像手敲而非程序播放 */
const humCents = () => (Math.random() * 2 - 1) * 4;
const humTime = () => (Math.random() * 2 - 1) * 0.01;
const humGain = (g: number) => g * (0.9 + Math.random() * 0.2);

interface ToneOpts {
  freq: number;
  /** 相对现在推迟的秒数 */
  t?: number;
  dur?: number;
  gain?: number;
  type?: OscillatorType;
  /** 泛音列 [频率倍率, 增益比]，模拟真实琴体共鸣 */
  partials?: Array<[number, number]>;
  pan?: number;
  attack?: number;
  space?: number;
  /** 低通截止（柔化电子感） */
  lowpass?: number;
  /** 颤音：失谐 LFO */
  vibrato?: { rate: number; depth: number };
}

/** 钟琴音：基音 + 非谐泛音列 + 指数衰减，像玻璃风铃 / 电颤琴 */
function tone(c: AudioContext, o: ToneOpts) {
  const t0 = c.currentTime + (o.t ?? 0) + humTime();
  const dur = o.dur ?? 0.9;
  const peak = humGain(o.gain ?? 0.2);
  const attack = o.attack ?? 0.008;

  const env = c.createGain();
  env.gain.setValueAtTime(0, t0);
  env.gain.linearRampToValueAtTime(peak, t0 + attack);
  env.gain.exponentialRampToValueAtTime(0.0001, t0 + dur);
  if (o.lowpass) {
    const lp = c.createBiquadFilter();
    lp.type = "lowpass";
    lp.frequency.value = o.lowpass;
    env.connect(lp);
    out(lp, c, o.space, o.pan);
  } else {
    out(env, c, o.space, o.pan);
  }

  const layers: Array<[number, number]> = [
    [1, 1],
    ...(o.partials ?? []),
  ];
  for (const [mult, g] of layers) {
    const osc = c.createOscillator();
    osc.type = o.type ?? "sine";
    osc.frequency.value = o.freq * mult;
    osc.detune.value = humCents();
    const lg = c.createGain();
    lg.gain.value = g;
    osc.connect(lg);
    lg.connect(env);
    if (o.vibrato) {
      const lfo = c.createOscillator();
      lfo.frequency.value = o.vibrato.rate;
      const depth = c.createGain();
      depth.gain.value = o.vibrato.depth;
      lfo.connect(depth);
      depth.connect(osc.detune);
      lfo.start(t0);
      lfo.stop(t0 + dur + 0.1);
    }
    osc.start(t0);
    osc.stop(t0 + dur + 0.05);
  }
}

/** 敲击起音：短噪声爆点 + 低频叩击，模拟琴槌 / 指关节接触的一瞬 */
function strike(c: AudioContext, o: { t?: number; freq?: number; gain?: number; q?: number; thump?: number }) {
  const t0 = c.currentTime + (o.t ?? 0);
  const gain = o.gain ?? 0.1;
  if (noiseBuf) {
    const src = c.createBufferSource();
    src.buffer = noiseBuf;
    const bp = c.createBiquadFilter();
    bp.type = "bandpass";
    bp.frequency.value = o.freq ?? 1600;
    bp.Q.value = o.q ?? 1.4;
    const env = c.createGain();
    env.gain.setValueAtTime(gain, t0);
    env.gain.exponentialRampToValueAtTime(0.0001, t0 + 0.055);
    src.connect(bp);
    bp.connect(env);
    out(env, c, 0.08);
    src.start(t0);
    src.stop(t0 + 0.08);
  }
  if (o.thump) {
    const osc = c.createOscillator();
    osc.type = "sine";
    osc.frequency.setValueAtTime(o.thump, t0);
    osc.frequency.exponentialRampToValueAtTime(Math.max(40, o.thump * 0.6), t0 + 0.09);
    const g = c.createGain();
    g.gain.setValueAtTime(gain * 1.2, t0);
    g.gain.exponentialRampToValueAtTime(0.0001, t0 + 0.12);
    osc.connect(g);
    out(g, c, 0.05);
    osc.start(t0);
    osc.stop(t0 + 0.15);
  }
}

/** 滑音泡泡：频率上扫的短促 pop，用于卡片弹出 */
function pop(c: AudioContext, o: { t?: number; from?: number; to?: number; dur?: number; gain?: number }) {
  const t0 = c.currentTime + (o.t ?? 0);
  const dur = o.dur ?? 0.09;
  const osc = c.createOscillator();
  osc.type = "sine";
  osc.frequency.setValueAtTime(o.from ?? 320, t0);
  osc.frequency.exponentialRampToValueAtTime(o.to ?? 660, t0 + dur);
  const env = c.createGain();
  env.gain.setValueAtTime(0, t0);
  env.gain.linearRampToValueAtTime(o.gain ?? 0.12, t0 + 0.012);
  env.gain.exponentialRampToValueAtTime(0.0001, t0 + dur + 0.06);
  osc.connect(env);
  out(env, c, 0.1);
  osc.start(t0);
  osc.stop(t0 + dur + 0.1);
}

/** 暖垫：慢起音的正弦和弦，铺在琶音下面，冲淡「电子提醒」的直白 */
function pad(c: AudioContext, freqs: number[], o: { t?: number; dur?: number; gain?: number }) {
  const t0 = c.currentTime + (o.t ?? 0);
  const dur = o.dur ?? 1.6;
  for (const f of freqs) {
    const osc = c.createOscillator();
    osc.type = "sine";
    osc.frequency.value = f;
    osc.detune.value = humCents() * 2;
    const env = c.createGain();
    env.gain.setValueAtTime(0, t0);
    env.gain.linearRampToValueAtTime((o.gain ?? 0.045) / freqs.length * 2, t0 + dur * 0.3);
    env.gain.linearRampToValueAtTime(0.0001, t0 + dur);
    const lp = c.createBiquadFilter();
    lp.type = "lowpass";
    lp.frequency.value = 1200;
    osc.connect(env);
    env.connect(lp);
    out(lp, c, 0.3);
    osc.start(t0);
    osc.stop(t0 + dur + 0.1);
  }
}

/** 木琴琴键：正弦主体 + 三倍频泛音 + 敲击起音，温润的完成感 */
function marimba(c: AudioContext, freq: number, t: number, gain = 0.22, dur = 1.1, pan = 0) {
  strike(c, { t, freq: freq * 3, gain: gain * 0.5, q: 2 });
  tone(c, {
    freq,
    t,
    dur,
    gain,
    partials: [
      [3.01, 0.18],
      [6.05, 0.05],
    ],
    pan,
    space: 0.2,
  });
}

/** 玻璃风铃：纯正弦基音 + 高频非谐泛音，长衰减带余韵 */
function chime(c: AudioContext, freq: number, t: number, gain = 0.18, dur = 1.5, pan = 0) {
  strike(c, { t, freq: freq * 2.4, gain: gain * 0.35, q: 3 });
  tone(c, {
    freq,
    t,
    dur,
    gain,
    partials: [
      [2.76, 0.22],
      [5.4, 0.07],
    ],
    pan,
    space: 0.28,
  });
}

// ---------------- 各事件的乐句 ----------------

// C 大调琶音频率表
const N = { C5: 523.25, E5: 659.26, G5: 783.99, C6: 1046.5, A4: 440, E6: 1318.5, F4: 349.23, B5: 987.77, E4: 329.63, C4: 261.63, A3: 220, Cs4: 277.18, G4: 392 };

const RECIPES: Record<SfxEvent, (c: AudioContext) => void> = {
  // 任务完成：木琴上行琶音 C5→E5→G5→C6，底下铺一层暖垫，尾音带余韵——
  // 「如释重负」的松弛感，节奏每次微有不同
  "task-done": (c) => {
    const gap = 0.115 + Math.random() * 0.03;
    pad(c, [N.C4, N.G4], { dur: 2.2, gain: 0.04 });
    marimba(c, N.C5, 0, 0.22, 1.0, -0.25);
    marimba(c, N.E5, gap, 0.22, 1.0, -0.08);
    marimba(c, N.G5, gap * 2, 0.22, 1.1, 0.1);
    marimba(c, N.C6, gap * 3, 0.26, 1.8, 0.3);
    chime(c, N.E6, gap * 3 + 0.02, 0.06, 1.6, 0.35);
  },

  // 弹窗通知：按用户选的质感出声（见 playSfx 里的 style 分支）
  notify: (c) => {
    const style = getNotifyStyle();
    if (style === "arp") {
      // 上行琶音：清脆三连
      marimba(c, N.C6, 0, 0.16, 0.8, -0.2);
      marimba(c, N.E6, 0.09, 0.16, 0.9, 0);
      marimba(c, 1567.98, 0.18, 0.18, 1.4, 0.25);
    } else if (style === "soft") {
      // 柔和单音：轻一声
      chime(c, 880, 0, 0.13, 1.1);
    } else {
      // 风铃：玻璃质感的一问一答双音（经典叮咚的圆润版）
      chime(c, N.E6, 0, 0.17, 1.5, -0.15);
      chime(c, N.C6, 0.16, 0.13, 1.8, 0.18);
    }
  },

  // 审批请求：礼貌的「叩叩」+ 一声轻铃——像有人在门口等你确认
  permission: (c) => {
    strike(c, { t: 0, freq: 950, gain: 0.16, q: 1.1, thump: 175 });
    strike(c, { t: 0.17, freq: 900, gain: 0.14, q: 1.1, thump: 165 });
    chime(c, N.B5, 0.42, 0.09, 1.0, 0.2);
  },

  // 卡片弹出：一个轻快的泡泡 pop
  "card-pop": (c) => {
    pop(c, { from: 300 + Math.random() * 60, to: 640 + Math.random() * 120, gain: 0.11 });
  },

  // 出错：低通柔化的下行小三度，带一点犹豫的颤音——提醒但不刺耳
  error: (c) => {
    tone(c, { freq: N.A4, t: 0, dur: 0.5, gain: 0.15, type: "triangle", lowpass: 1100, vibrato: { rate: 6, depth: 7 }, space: 0.12 });
    tone(c, { freq: N.F4, t: 0.19, dur: 0.7, gain: 0.17, type: "triangle", lowpass: 1000, vibrato: { rate: 5.5, depth: 8 }, space: 0.14 });
  },

  // 通知升级：三声渐强的上行纯五度脉冲（A4→E5），带震音与双振荡拍频——
  // 紧迫感来自律动与渐强，而不是刺耳的方波
  escalation: (c) => {
    for (let i = 0; i < 3; i++) {
      const t = i * 0.26;
      const g = 0.12 + i * 0.06;
      for (const [f, pan] of [
        [N.A4, -0.2],
        [N.E5, 0.2],
      ] as Array<[number, number]>) {
        tone(c, {
          freq: f,
          t,
          dur: 0.24,
          gain: g * 0.7,
          partials: [[2.01, 0.1]],
          pan,
          space: 0.2,
          vibrato: { rate: 9, depth: 5 },
        });
      }
      strike(c, { t, freq: 2400, gain: 0.05 });
    }
    // 收尾小铃声提示「升级完成」
    chime(c, 1567.98, 0.82, 0.12, 1.2, 0.3);
  },

  // 启动 · 觉醒：暖垫缓缓浮起 + 两粒星屑铃声，像水面涟漪
  startup: (c) => {
    pad(c, [N.A3, N.Cs4, N.E4], { dur: 2.6, gain: 0.05 });
    chime(c, N.E6, 0.45, 0.07, 1.8, -0.2);
    chime(c, 1975.53, 0.68, 0.05, 1.6, 0.25);
  },

  // 消息发出：极轻的一声触感反馈，像气泡离开指尖
  "message-sent": (c) => {
    pop(c, { from: 520, to: 880, dur: 0.06, gain: 0.055 });
  },

  // 语音唤醒：短促上滑泡泡 + 一粒轻铃——「我在听」
  "voice-wake": (c) => {
    pop(c, { from: 480, to: 940, dur: 0.07, gain: 0.1 });
    chime(c, N.E6, 0.07, 0.07, 0.7, 0.15);
  },

  // 问句交回：下行双音（E6→A5），与唤醒的上行方向相反——「该你说了」
  "voice-handoff": (c) => {
    chime(c, N.E6, 0, 0.11, 0.6, -0.12);
    chime(c, N.A4, 0.13, 0.13, 0.9, 0.15);
  },
};

// ---------------- 对外 API ----------------

/** 播放某事件音效（内部已处理开关与静默失败） */
export function playSfx(event: SfxEvent) {
  if (!isSfxEnabled()) return;
  const c = ac();
  if (!c || c.state === "closed") return;
  try {
    if (c.state === "suspended") {
      // 自动播放策略未解除时挂起，尝试唤醒；唤醒失败就放弃这一次
      void c.resume().then(
        () => {
          try {
            RECIPES[event](c);
          } catch {
            /* 静默忽略 */
          }
        },
        () => {},
      );
      return;
    }
    RECIPES[event](c);
  } catch {
    /* 静默忽略：无音频环境时不报错 */
  }
}

/** 试听某种提醒风格（设置面板选风格时用，不受全局开关限制手感一致） */
export function previewNotify(style: NotifyStyle) {
  const c = ac();
  if (!c) return;
  try {
    if (style === "arp") {
      marimba(c, N.C6, 0, 0.16, 0.8, -0.2);
      marimba(c, N.E6, 0.09, 0.16, 0.9, 0);
      marimba(c, 1567.98, 0.18, 0.18, 1.4, 0.25);
    } else if (style === "soft") {
      chime(c, 880, 0, 0.13, 1.1);
    } else {
      chime(c, N.E6, 0, 0.17, 1.5, -0.15);
      chime(c, N.C6, 0.16, 0.13, 1.8, 0.18);
    }
  } catch {
    /* noop */
  }
}
