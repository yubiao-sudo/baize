/**
 * 语音光环球：参考 cyber-companion 的语音交互视觉（glow 光晕 + ring 光环 + core 核心 + 麦克风图标 + 音频条），
 * 独立实现。audioLevel（0-1）驱动发光强度，state 驱动动画。
 */

interface VoiceOrbProps {
  size?: number;
  audioLevel?: number;
  state?: "idle" | "listening" | "thinking";
  color?: string;
}

export default function VoiceOrb({
  size = 100,
  audioLevel = 0,
  state = "idle",
  color = "#22d3ee",
}: VoiceOrbProps) {
  const active = state !== "idle";
  const glow = 30 + audioLevel * 70;

  return (
    <div className={`voice-orb ${active ? "active" : ""}`} style={{ width: size, height: size }}>
      {/* 光晕 */}
      <div
        className="voice-orb-glow"
        style={{
          background: `radial-gradient(circle, ${color}44, transparent 70%)`,
          opacity: active ? 0.5 + audioLevel * 0.5 : 0.2,
        }}
      />
      {/* 光环 */}
      <div className="voice-orb-ring" style={{ borderColor: `${color}33` }} />
      {/* 核心 */}
      <div
        className="voice-orb-core"
        style={{
          borderColor: `${color}4d`,
          background: `linear-gradient(135deg, ${color}22, ${color}11)`,
          boxShadow: `0 0 ${glow}px ${color}55`,
        }}
      >
        <svg
          className="voice-icon"
          style={{ color }}
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
          <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
          <line x1="12" y1="19" x2="12" y2="23" />
          <line x1="8" y1="23" x2="16" y2="23" />
        </svg>
      </div>
      {/* 音频条 */}
      <div className="audio-bars">
        {Array.from({ length: 7 }).map((_, i) => (
          <div
            key={i}
            className="audio-bar"
            style={{ animationDelay: `${i * 0.08}s` }}
          />
        ))}
      </div>
    </div>
  );
}
