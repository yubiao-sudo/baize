import { useCallback, useEffect, useRef, useState } from "react";
import * as THREE from "three";
import { OrbitControls } from "three/examples/jsm/controls/OrbitControls.js";
import * as d3 from "d3";
import { getMemoryGraph, onMemoryRecall } from "../api";
import { useChat } from "../stores/chat";
import { derive } from "./AiActivity";

/**
 * 意识网络 —— 3D DNA 记忆链（Three.js 渲染 + d3 力导向算位置）
 * 记忆节点排成 3D 双螺旋（两条链相位差 π，绕 Y 轴缠绕），d3 力把节点拉向螺旋目标位置。
 * 支持 OrbitControls 旋转/缩放视角、点击节点查看详情；记忆索引时节点发光。
 * 链间「碱基对」+ 后端 n-gram 相似边作为关联。
 */

const COLOR = 0x60a5fa;
const SIMILAR_COLOR = 0x22d3ee;
const TURNS = 3;
const RADIUS = 55;
const HEIGHT = 160;

interface DnaNode extends d3.SimulationNodeDatum {
  id: string;
  content: string;
  salience: number;
  strand: number;
  pair: number;
  ambient: boolean;
  targetX: number;
  targetY: number;
  targetZ: number;
  z?: number;
  mesh?: THREE.Mesh;
  mat?: THREE.MeshStandardMaterial;
}

interface DnaLink extends d3.SimulationLinkDatum<DnaNode> {
  type: "rung" | "similar";
  weight: number;
}

interface LinkLine {
  line: THREE.Line;
  s: DnaNode;
  t: DnaNode;
}

// =====================================================================
// 【已注释停用 · 旧版】3D DNA 双螺旋记忆链 —— 按需求改为「蠕动记忆水球」
// 保留备用：如需恢复，将底部 default 导出替换为 DnaMemoryChainLegacy 即可
// =====================================================================
export function DnaMemoryChainLegacy() {
  const containerRef = useRef<HTMLDivElement>(null);
  const [nodeCount, setNodeCount] = useState(0);
  const [selected, setSelected] = useState<DnaNode | null>(null);

  const simRef = useRef<d3.Simulation<DnaNode, DnaLink> | null>(null);
  const nodesRef = useRef<DnaNode[]>([]);
  const linesRef = useRef<LinkLine[]>([]);
  const rotationRef = useRef(0);
  const threeRef = useRef<{
    renderer: THREE.WebGLRenderer;
    scene: THREE.Scene;
    camera: THREE.PerspectiveCamera;
    controls: OrbitControls;
    nodesGroup: THREE.Group;
    linksGroup: THREE.Group;
    raycaster: THREE.Raycaster;
    pointer: THREE.Vector2;
  } | null>(null);

  const computeTargets = (ns: DnaNode[], rot: number) => {
    const pairCount = Math.max(1, Math.ceil(ns.length / 2));
    for (const n of ns) {
      const t = pairCount > 1 ? n.pair / (pairCount - 1) : 0;
      const angle = t * Math.PI * 2 * TURNS + rot + n.strand * Math.PI;
      n.targetX = Math.cos(angle) * RADIUS;
      n.targetY = (t * 2 - 1) * (HEIGHT / 2);
      n.targetZ = Math.sin(angle) * RADIUS;
    }
  };

  // 更新连线几何（碱基对 / 相似边）
  const updateLinkPositions = () => {
    for (const ll of linesRef.current) {
      const pos = ll.line.geometry.getAttribute("position") as THREE.BufferAttribute;
      pos.setXYZ(0, ll.s.x ?? 0, ll.s.y ?? 0, ll.s.z ?? 0);
      pos.setXYZ(1, ll.t.x ?? 0, ll.t.y ?? 0, ll.t.z ?? 0);
      pos.needsUpdate = true;
      ll.line.geometry.computeBoundingSphere();
    }
  };

  // 初始化 three + d3 力导向（一次）
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const w = el.clientWidth || 300;
    const h = el.clientHeight || 240;

    const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    renderer.setSize(w, h);
    renderer.domElement.style.display = "block";
    el.appendChild(renderer.domElement);

    const scene = new THREE.Scene();
    const camera = new THREE.PerspectiveCamera(50, w / h, 0.1, 2000);
    camera.position.set(0, 0, 320);

    scene.add(new THREE.AmbientLight(0x445577, 1.4));
    const d1 = new THREE.DirectionalLight(0xffffff, 1.6);
    d1.position.set(6, 10, 8);
    scene.add(d1);
    const d2 = new THREE.DirectionalLight(0x88aaff, 0.6);
    d2.position.set(-6, -4, -8);
    scene.add(d2);

    const controls = new OrbitControls(camera, renderer.domElement);
    controls.enableDamping = true;
    controls.dampingFactor = 0.08;
    controls.autoRotate = true;
    controls.autoRotateSpeed = 0.7;
    controls.minDistance = 130;
    controls.maxDistance = 700;
    controls.enablePan = false;

    const nodesGroup = new THREE.Group();
    const linksGroup = new THREE.Group();
    scene.add(nodesGroup);
    scene.add(linksGroup);

    const raycaster = new THREE.Raycaster();
    const pointer = new THREE.Vector2();

    threeRef.current = { renderer, scene, camera, controls, nodesGroup, linksGroup, raycaster, pointer };

    const sim = d3
      .forceSimulation<DnaNode, DnaLink>([])
      .force("x", d3.forceX<DnaNode>((d) => d.targetX).strength(0.5))
      .force("y", d3.forceY<DnaNode>((d) => d.targetY).strength(0.5))
      .force(
        "link",
        d3
          .forceLink<DnaNode, DnaLink>([])
          .id((d) => d.id)
          .distance((l) => (l.type === "rung" ? 26 : 48))
          .strength((l) => (l.type === "rung" ? 0.5 : 0.15)),
      )
      .force("charge", d3.forceManyBody<DnaNode>().strength(-30))
      .alphaTarget(0.15)
      .on("tick", () => {
        for (const n of nodesRef.current) {
          // d3-force 无 forceZ：z 用简单弹簧朝 targetZ 收敛
          if (n.z == null) n.z = n.targetZ;
          n.z += (n.targetZ - n.z) * 0.12;
          if (n.mesh) n.mesh.position.set(n.x ?? 0, n.y ?? 0, n.z);
        }
        updateLinkPositions();
      });
    simRef.current = sim;

    let raf = 0;
    const loop = () => {
      controls.update();
      renderer.render(scene, camera);
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);

    const ro = new ResizeObserver(() => {
      const rw = el.clientWidth;
      const rh = el.clientHeight;
      if (rw < 10 || rh < 10) return;
      camera.aspect = rw / rh;
      camera.updateProjectionMatrix();
      renderer.setSize(rw, rh);
    });
    ro.observe(el);

    const onMove = (e: PointerEvent) => {
      const t = threeRef.current;
      if (!t) return;
      const rect = renderer.domElement.getBoundingClientRect();
      t.pointer.x = ((e.clientX - rect.left) / rect.width) * 2 - 1;
      t.pointer.y = -((e.clientY - rect.top) / rect.height) * 2 + 1;
    };
    const onClick = () => {
      const t = threeRef.current;
      if (!t) return;
      t.raycaster.setFromCamera(t.pointer, camera);
      const meshes = t.nodesGroup.children.filter((c) => (c as THREE.Mesh).isMesh) as THREE.Mesh[];
      const hits = t.raycaster.intersectObjects(meshes, false);
      if (hits.length > 0) {
        const node = nodesRef.current.find((n) => n.mesh === hits[0].object);
        if (node && !node.ambient) setSelected(node);
      }
    };
    renderer.domElement.addEventListener("pointermove", onMove);
    renderer.domElement.addEventListener("click", onClick);

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      renderer.domElement.removeEventListener("pointermove", onMove);
      renderer.domElement.removeEventListener("click", onClick);
      controls.dispose();
      sim.stop();
      renderer.dispose();
      if (renderer.domElement.parentElement === el) el.removeChild(renderer.domElement);
      threeRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 加载记忆并重建 3D 节点/连线
  const loadData = useCallback(() => {
    return getMemoryGraph()
      .then((g) => {
          const t = threeRef.current;
          const target = Math.max(12, g.nodes.length);
          const pairCount = Math.ceil(target / 2);
          const old = new Map(nodesRef.current.map((n) => [n.id, n]));

          // 清理旧对象
          for (const n of nodesRef.current) {
            if (n.mesh) {
              t?.nodesGroup.remove(n.mesh);
              n.mesh.geometry.dispose();
              n.mat?.dispose();
            }
          }
          for (const ll of linesRef.current) {
            t?.linksGroup.remove(ll.line);
            ll.line.geometry.dispose();
            (ll.line.material as THREE.Material).dispose();
          }
          linesRef.current = [];

          const newNodes: DnaNode[] = [];
          for (let i = 0; i < target; i++) {
            const strand = i % 2;
            const pair = Math.floor(i / 2);
            const m = g.nodes[i];
            const id = m ? m.mem_id : `ambient-${i}`;
            const prev = old.get(id);
            const node: DnaNode = {
              id,
              content: m ? m.content : "",
              salience: m ? m.salience : 0,
              strand,
              pair,
              ambient: !m,
              targetX: 0,
              targetY: 0,
              targetZ: 0,
              x: prev?.x,
              y: prev?.y,
              z: prev?.z,
            };

            if (t) {
              const r = 4 + Math.min(6, (m ? m.salience : 0) * 0.8);
              const geo = new THREE.SphereGeometry(r, 20, 20);
              // 多样配色：链 A 青蓝渐变，链 B 紫粉渐变，沿螺旋逐渐过渡
              const hue =
                node.strand === 0
                  ? 185 + (node.pair / Math.max(1, pairCount)) * 50
                  : 260 + (node.pair / Math.max(1, pairCount)) * 50;
              const nodeColor = new THREE.Color().setHSL(hue / 360, 0.72, 0.6);
              const mat = new THREE.MeshStandardMaterial({
                color: nodeColor,
                transparent: true,
                opacity: node.ambient ? 0.25 : 0.55,
                roughness: 0.15,
                metalness: 0.1,
              });
              const mesh = new THREE.Mesh(geo, mat);
              mesh.position.set(node.x ?? 0, node.y ?? 0, node.z ?? 0);
              t.nodesGroup.add(mesh);
              node.mesh = mesh;
              node.mat = mat;
            }
            newNodes.push(node);
          }

          // 连线：碱基对 + 相似边
          const newLinks: DnaLink[] = [];
          for (let p = 0; p < pairCount; p++) {
            const a = newNodes.find((n) => n.pair === p && n.strand === 0);
            const b = newNodes.find((n) => n.pair === p && n.strand === 1);
            if (a && b) newLinks.push({ source: a.id, target: b.id, type: "rung", weight: 1 });
          }
          for (const e of g.edges) {
            const a = newNodes.find((n) => n.id === e.from);
            const b = newNodes.find((n) => n.id === e.to);
            if (a && b) newLinks.push({ source: e.from, target: e.to, type: "similar", weight: e.weight });
          }

          if (t) {
            for (const l of newLinks) {
              const s = newNodes.find((n) => n.id === (l.source as string));
              const tt = newNodes.find((n) => n.id === (l.target as string));
              if (!s || !tt) continue;
              const color = l.type === "similar" ? SIMILAR_COLOR : COLOR;
              const opacity = l.type === "similar" ? 0.3 : 0.25;
              const geo = new THREE.BufferGeometry();
              const arr = new Float32Array(6);
              geo.setAttribute("position", new THREE.BufferAttribute(arr, 3));
              const mat = new THREE.LineBasicMaterial({ color, transparent: true, opacity });
              const line = new THREE.Line(geo, mat);
              t.linksGroup.add(line);
              linesRef.current.push({ line, s, t: tt });
            }
          }

          computeTargets(newNodes, rotationRef.current);
          nodesRef.current = newNodes;
          setNodeCount(g.nodes.length);

          const sim = simRef.current;
          if (sim) {
            sim.nodes(newNodes);
            (sim.force("link") as d3.ForceLink<DnaNode, DnaLink>).links(newLinks);
            sim.alpha(0.8).restart();
          }
          updateLinkPositions();
        })
        .catch(() => {});
  }, []);

  // 数据加载 + 5 秒轮询
  useEffect(() => {
    loadData();
    const interval = setInterval(loadData, 5000);
    return () => clearInterval(interval);
  }, [loadData]);

  // 螺旋缓慢旋转（更新 3D 目标位置）
  useEffect(() => {
    let raf = 0;
    let last = performance.now();
    const loop = (now: number) => {
      const dt = Math.min(0.05, (now - last) / 1000);
      last = now;
      rotationRef.current += dt * 0.4;
      if (nodesRef.current.length) {
        computeTargets(nodesRef.current, rotationRef.current);
      }
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(raf);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 涟漪：从命中球体向外扩散的光环
  const spawnRipple = (pos: THREE.Vector3) => {
    const t = threeRef.current;
    if (!t) return;
    const geo = new THREE.TorusGeometry(6, 1.4, 12, 48);
    const mat = new THREE.MeshBasicMaterial({
      color: 0xffffff,
      transparent: true,
      opacity: 0.8,
    });
    const ring = new THREE.Mesh(geo, mat);
    ring.position.copy(pos);
    ring.lookAt(t.camera.position);
    t.scene.add(ring);
    const start = performance.now();
    const duration = 900;
    const tick = () => {
      const k = Math.min(1, (performance.now() - start) / duration);
      ring.scale.setScalar(1 + k * 3.2);
      mat.opacity = 0.8 * (1 - k);
      if (k < 1) {
        requestAnimationFrame(tick);
      } else {
        t.scene.remove(ring);
        geo.dispose();
        mat.dispose();
      }
    };
    tick();
  };

  // 记忆召回 → 先刷新（召回记忆的 last_access 已更新，会进入列表）再精确高亮 + 涟漪
  useEffect(() => {
    let disposed = false;
    onMemoryRecall((ids) => {
      if (disposed) return;
      loadData().then(() => {
        if (disposed) return;
        const targets = nodesRef.current.filter(
          (n) => ids.includes(n.id) && !n.ambient && n.mesh && n.mat,
        );
        for (const n of targets) {
          n.mat!.emissive = new THREE.Color(0xffffff);
          n.mat!.emissiveIntensity = 1.2;
          const base = n.mesh!.scale.x;
          n.mesh!.scale.setScalar(base * 1.4);
          window.setTimeout(() => {
            if (n.mat) n.mat.emissiveIntensity = 0;
            n.mesh!.scale.setScalar(base);
          }, 2000);
          spawnRipple(n.mesh!.position.clone());
        }
      });
    });
    return () => {
      disposed = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadData]);

  return (
    <div className="panel-block mind">
      <div className="panel-head">
        意识网络 <span className="tag">记忆链 · {nodeCount}</span>
      </div>
      <div className="mind-canvas" ref={containerRef}>
        {selected && !selected.ambient && (
          <div className="mind-detail">
            <div className="mind-detail-text">{selected.content}</div>
            <button className="mind-detail-close" onClick={() => setSelected(null)}>
              ×
            </button>
          </div>
        )}
        {nodeCount === 0 && (
          <div className="mind-empty">暂无记忆 · 对话后记忆链会生长成双螺旋</div>
        )}
      </div>
    </div>
  );
}

// ============================================================
// 「意识网络 —— 蠕动记忆水球」（CSS 液态球渲染，与启动动画同款）
// 一颗大水球：border-radius 液态形变 + 内部流光 + 双层发光晕圈。
// 白泽检索/使用记忆（memory-recall）时，水球心跳式搏动（2 跳）、
// 晕圈发光爆发，并从球面向外扩散两圈涟漪，约 2.6 秒平滑回归常态。
// ============================================================

// 模块级标记：启动交接（splash 水球飞入）只需在应用首次挂载时等待一次；
// 之后面板切换导致的重挂载直接显示，避免水球白等 15 秒兜底而「消失」
let blobRevealed = false;

export default function ConsciousnessNetwork() {
  const orbRef = useRef<HTMLDivElement>(null);
  const pulseTimer = useRef(0);
  const voicePulseTimer = useRef(0);
  const [nodeCount, setNodeCount] = useState(0);

  // TTS 语音律动：白泽说话时水球「张嘴」——晕圈加速呼吸，每个词边界触发一次
  // 平滑脉冲缩放（speechSynthesis 无音频流，用逐词 onboundary 事件近似节奏）
  // 性能：缩放收敛后挂起 rAF 循环，由 pulse/speaking 事件再唤醒——空闲时零帧开销
  useEffect(() => {
    const el = orbRef.current;
    if (!el) return;
    let scale = 1;
    let target = 1;
    let raf = 0;
    let running = false;
    const step = () => {
      scale += (target - scale) * 0.18;
      if (Math.abs(target - scale) < 0.0005) {
        scale = target;
        el.style.setProperty("--voice-scale", scale.toFixed(4));
        running = false; // 已收敛：挂起，事件到来再唤醒
        return;
      }
      el.style.setProperty("--voice-scale", scale.toFixed(4));
      raf = requestAnimationFrame(step);
    };
    const wake = () => {
      if (!running) {
        running = true;
        raf = requestAnimationFrame(step);
      }
    };
    const onState = (e: Event) => {
      const speaking = (e as CustomEvent<{ speaking: boolean }>).detail.speaking;
      el.classList.toggle("speaking", speaking);
      if (!speaking) {
        target = 1;
        wake();
      }
    };
    const onPulse = (e: Event) => {
      const { energy } = (e as CustomEvent<{ energy: number }>).detail;
      target = 1 + 0.07 * energy;
      wake();
      // 短暂保持后回落，下一个词边界会再次抬升
      window.clearTimeout(voicePulseTimer.current);
      voicePulseTimer.current = window.setTimeout(() => {
        target = 1;
        wake();
      }, 180);
    };
    window.addEventListener("baize:tts-state", onState);
    window.addEventListener("baize:tts-pulse", onPulse);
    return () => {
      window.removeEventListener("baize:tts-state", onState);
      window.removeEventListener("baize:tts-pulse", onPulse);
      cancelAnimationFrame(raf);
      window.clearTimeout(voicePulseTimer.current);
      el.classList.remove("speaking");
      el.style.removeProperty("--voice-scale");
    };
  }, []);

  // 启动交接：主页水球初始透明，等启动动画的水球飞入 .mind-canvas 落位后淡入；
  // 与启动水球同一套 CSS（视觉 1:1），衔接处肉眼无感。15s 兜底强制显示。
  useEffect(() => {
    const el = orbRef.current;
    if (!el) return;
    if (blobRevealed) {
      el.style.opacity = "1";
      return;
    }
    el.style.opacity = "0";
    el.style.transition = "opacity 0.5s ease";
    const reveal = () => {
      blobRevealed = true;
      el.style.opacity = "1";
    };
    window.addEventListener("baize:blob-handoff", reveal, { once: true });
    const fallback = window.setTimeout(reveal, 15000);
    return () => {
      window.removeEventListener("baize:blob-handoff", reveal);
      window.clearTimeout(fallback);
    };
  }, []);

  // 连续语音对话形态：待唤醒=缓呼吸 + 青色晕圈（voice-standby），
  // 聆听指令/等待插话=快速脉动 + 晕圈扩张（voice-listening）
  useEffect(() => {
    const el = orbRef.current;
    if (!el) return;
    const onMode = (e: Event) => {
      const mode = (e as CustomEvent<{ mode: string }>).detail.mode;
      el.classList.toggle("voice-standby", mode === "standby");
      el.classList.toggle("voice-listening", mode === "listening");
    };
    window.addEventListener("baize:voice-mode", onMode);
    return () => {
      window.removeEventListener("baize:voice-mode", onMode);
      el.classList.remove("voice-standby", "voice-listening");
    };
  }, []);

  // 任务形变：白泽不同行为 → 水球不同形态
  // 思考中=深潜（慢速大幅蠕动、色偏靛紫）| 调用工具=干练（快速摆动、色偏青绿）
  // 生成中=涌动（高频微颤、色相流转加速）| 空闲=默认呼吸；说话/记忆召回为事件类单独控制
  const busy = useChat((s) => s.busy);
  const streaming = useChat((s) => s.streaming);
  const thoughts = useChat((s) => s.thoughts);
  const activity = derive(thoughts, busy, streaming);
  useEffect(() => {
    const el = orbRef.current;
    if (!el) return;
    const isTool = activity.tone === "tool";
    el.classList.toggle("thinking", busy && !streaming && !isTool);
    el.classList.toggle("working", busy && !streaming && isTool);
    el.classList.toggle("generating", !!streaming);
  }, [busy, streaming, activity.tone]);

  // 加载记忆数量用于展示
  const loadData = useCallback(() => {
    return getMemoryGraph()
      .then((g) => {
        setNodeCount(g.nodes.length);
      })
      .catch(() => {});
  }, []);

  // 数据加载 + 5 秒轮询
  useEffect(() => {
    loadData();
    const interval = setInterval(loadData, 5000);
    return () => clearInterval(interval);
  }, [loadData]);

  // 记忆召回 → 水球反馈：心跳搏动 + 发光爆发 + 涟漪扩散（CSS 类触发，约 2.6 秒回归常态）
  useEffect(() => {
    let disposed = false;
    let unRecall: (() => void) | null = null;
    onMemoryRecall((ids) => {
      if (disposed || ids.length === 0) return;
      const el = orbRef.current;
      if (el) {
        el.classList.remove("recalling");
        void el.offsetWidth; // 强制 reflow 以重播动画
        el.classList.add("recalling");
        window.clearTimeout(pulseTimer.current);
        pulseTimer.current = window.setTimeout(() => el.classList.remove("recalling"), 2600);
      }
      // 刷新计数（命中记忆 last_access 已更新）
      loadData();
    }).then((f) => {
      // 卸载早于注册完成则立即反注册，防止 listener 泄漏
      if (disposed) f();
      else unRecall = f;
    });
    return () => {
      disposed = true;
      unRecall?.();
      window.clearTimeout(pulseTimer.current);
    };
  }, [loadData]);

  return (
    <div className="panel-block mind">
      <div className="panel-head">
        意识网络 <span className="tag">记忆水球 · 最近 {nodeCount} 条</span>
      </div>
      <div className="mind-canvas">
        <div
          className="mind-orb clickable"
          ref={orbRef}
          onClick={() => window.dispatchEvent(new CustomEvent("baize:open-galaxy"))}
          title="点击展开记忆星图"
        >
          <div className="halo" />
          <div className="halo2" />
          <div className="orb-live">
            <div className="orb" />
          </div>
          <div className="ripple-ring" />
        </div>
        {nodeCount === 0 && (
          <div className="mind-empty">暂无记忆 · 对话后水球会随检索泛起涟漪</div>
        )}
      </div>
    </div>
  );
}
