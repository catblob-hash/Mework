import { Pause, Play, RotateCcw, Volume2, VolumeX } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { useI18n } from "../../i18n";
import { IconButton } from "../Common";
import { dataUrlBytes, formatBytes, formatDuration } from "./format";

type AudioContextConstructor = typeof AudioContext;

function audioContextConstructor(): AudioContextConstructor | null {
  if (typeof window === "undefined") return null;
  return window.AudioContext
    ?? (window as unknown as { webkitAudioContext?: AudioContextConstructor }).webkitAudioContext
    ?? null;
}

/** The loudest sample in each of `buckets` equal slices, across every channel. */
function peaks(buffer: AudioBuffer, buckets: number): Float32Array {
  const result = new Float32Array(buckets);
  const length = buffer.length;
  const size = Math.max(1, Math.floor(length / buckets));
  for (let channel = 0; channel < buffer.numberOfChannels; channel += 1) {
    const data = buffer.getChannelData(channel);
    for (let bucket = 0; bucket < buckets; bucket += 1) {
      const start = bucket * size;
      const end = Math.min(length, start + size);
      let peak = result[bucket];
      // Striding keeps a long recording from costing a full pass per redraw;
      // the eye cannot tell a peak found in every fourth sample from the true one.
      for (let index = start; index < end; index += 4) {
        const value = Math.abs(data[index]);
        if (value > peak) peak = value;
      }
      result[bucket] = peak;
    }
  }
  return result;
}

/**
 * An audio file, decoded and played through the Web Audio API.
 *
 * `<audio>` is what anyone would reach for, and the renderer's CSP forbids every
 * media source. Decoding the bytes into a buffer and playing that is not media
 * loading at all, so it is allowed — and it gives the waveform for free.
 */
export function AudioPlayer({ source, name, bytes }: { source: string; name: string; bytes: number | null }) {
  const { t } = useI18n();
  const [buffer, setBuffer] = useState<AudioBuffer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [playing, setPlaying] = useState(false);
  const [position, setPosition] = useState(0);
  const [volume, setVolume] = useState(1);
  const [muted, setMuted] = useState(false);
  const contextRef = useRef<AudioContext | null>(null);
  const gainRef = useRef<GainNode | null>(null);
  const nodeRef = useRef<AudioBufferSourceNode | null>(null);
  /** Where in the buffer playback started, and when on the context's clock it did. */
  const anchorRef = useRef({ offset: 0, startedAt: 0 });
  const frameRef = useRef<number | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  /** The level a freshly made context starts at, so opening another file keeps the reader's volume. */
  const levelRef = useRef(1);
  levelRef.current = muted ? 0 : volume;
  /** Peaks per column count: the scan over the samples runs once per width, not once per frame. */
  const peaksRef = useRef<{ buffer: AudioBuffer; columns: number; values: Float32Array } | null>(null);

  useEffect(() => {
    const Context = audioContextConstructor();
    setBuffer(null);
    setError(null);
    setPlaying(false);
    setPosition(0);
    if (!Context) {
      setError(t("当前界面引擎不支持音频解码。", "This window's engine cannot decode audio."));
      return;
    }
    const context = new Context();
    const gain = context.createGain();
    gain.gain.value = levelRef.current;
    gain.connect(context.destination);
    contextRef.current = context;
    gainRef.current = gain;
    let cancelled = false;
    const data = dataUrlBytes(source);
    void context.decodeAudioData(data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) as ArrayBuffer)
      .then((decoded) => {
        if (!cancelled) setBuffer(decoded);
      })
      .catch(() => {
        if (!cancelled) setError(t("当前界面引擎无法解码这种音频格式。", "This window's engine cannot decode this audio format."));
      });
    return () => {
      cancelled = true;
      if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
      try {
        nodeRef.current?.stop();
      } catch {
        // Stopping a node that already ended is not an error worth reporting.
      }
      nodeRef.current = null;
      void context.close();
      contextRef.current = null;
    };
  }, [source, t]);

  useEffect(() => {
    if (gainRef.current) gainRef.current.gain.value = muted ? 0 : volume;
  }, [muted, volume]);

  const currentPosition = useCallback((): number => {
    const context = contextRef.current;
    if (!context || !nodeRef.current) return anchorRef.current.offset;
    return anchorRef.current.offset + (context.currentTime - anchorRef.current.startedAt);
  }, []);

  const stopNode = useCallback(() => {
    const node = nodeRef.current;
    nodeRef.current = null;
    if (!node) return;
    node.onended = null;
    try {
      node.stop();
    } catch {
      // Already ended.
    }
  }, []);

  const tick = useCallback(() => {
    setPosition(currentPosition());
    frameRef.current = requestAnimationFrame(tick);
  }, [currentPosition]);

  const play = useCallback((from: number) => {
    const context = contextRef.current;
    const gain = gainRef.current;
    if (!context || !gain || !buffer) return;
    stopNode();
    const offset = Math.min(Math.max(0, from), buffer.duration);
    const node = context.createBufferSource();
    node.buffer = buffer;
    node.connect(gain);
    node.onended = () => {
      if (nodeRef.current !== node) return;
      nodeRef.current = null;
      anchorRef.current = { offset: 0, startedAt: 0 };
      setPlaying(false);
      setPosition(buffer.duration);
      if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
    };
    void context.resume();
    node.start(0, offset);
    nodeRef.current = node;
    anchorRef.current = { offset, startedAt: context.currentTime };
    setPlaying(true);
    if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
    frameRef.current = requestAnimationFrame(tick);
  }, [buffer, stopNode, tick]);

  const pause = useCallback(() => {
    const at = currentPosition();
    stopNode();
    anchorRef.current = { offset: at, startedAt: 0 };
    setPosition(at);
    setPlaying(false);
    if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
  }, [currentPosition, stopNode]);

  const seek = useCallback((to: number) => {
    if (!buffer) return;
    const clamped = Math.min(Math.max(0, to), buffer.duration);
    if (playing) play(clamped);
    else {
      anchorRef.current = { offset: clamped, startedAt: 0 };
      setPosition(clamped);
    }
  }, [buffer, play, playing]);

  // The waveform: every column's peak, drawn in the text colour up to the
  // playhead and faint after it.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !buffer) return;
    const draw = () => {
      const width = canvas.clientWidth;
      const height = canvas.clientHeight;
      if (!width || !height) return;
      const ratio = window.devicePixelRatio || 1;
      canvas.width = Math.floor(width * ratio);
      canvas.height = Math.floor(height * ratio);
      const context = canvas.getContext("2d");
      if (!context) return;
      const styles = getComputedStyle(canvas);
      const played = styles.getPropertyValue("--audio-played").trim() || styles.color;
      const rest = styles.getPropertyValue("--audio-rest").trim() || styles.color;
      const columns = Math.max(1, Math.floor(width / 3));
      let cached = peaksRef.current;
      if (!cached || cached.buffer !== buffer || cached.columns !== columns) {
        cached = { buffer, columns, values: peaks(buffer, columns) };
        peaksRef.current = cached;
      }
      const values = cached.values;
      const progress = buffer.duration ? position / buffer.duration : 0;
      context.scale(ratio, ratio);
      for (let column = 0; column < columns; column += 1) {
        const value = Math.max(0.02, Math.min(1, values[column]));
        const bar = value * (height - 2);
        context.fillStyle = column / columns <= progress ? played : rest;
        context.fillRect(column * 3, (height - bar) / 2, 2, bar);
      }
    };
    draw();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(draw);
    observer?.observe(canvas);
    return () => observer?.disconnect();
  }, [buffer, position]);

  const seekFromPointer = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!buffer) return;
    const rect = event.currentTarget.getBoundingClientRect();
    seek(((event.clientX - rect.left) / Math.max(1, rect.width)) * buffer.duration);
  };

  if (error) return <p className="files-pane__notice">{error}</p>;

  return (
    <div className="file-preview file-preview--audio">
      <div className="file-preview__audio-card">
        <div className="file-preview__audio-title" title={name}>{name}</div>
        <canvas
          ref={canvasRef}
          className="file-preview__waveform"
          role="slider"
          tabIndex={0}
          aria-label={t("播放位置", "Playback position")}
          aria-valuemin={0}
          aria-valuemax={Math.round(buffer?.duration ?? 0)}
          aria-valuenow={Math.round(position)}
          aria-valuetext={formatDuration(position)}
          onPointerDown={(event) => {
            event.currentTarget.setPointerCapture(event.pointerId);
            seekFromPointer(event);
          }}
          onPointerMove={(event) => {
            if (event.currentTarget.hasPointerCapture(event.pointerId)) seekFromPointer(event);
          }}
          onKeyDown={(event) => {
            if (event.key === "ArrowLeft") seek(position - 5);
            else if (event.key === "ArrowRight") seek(position + 5);
            else if (event.key === " ") {
              event.preventDefault();
              if (playing) pause();
              else play(position >= (buffer?.duration ?? 0) ? 0 : position);
            }
          }}
        />
        <div className="file-preview__audio-controls">
          <IconButton
            className="file-preview__tool file-preview__play"
            label={playing ? t("暂停", "Pause") : t("播放", "Play")}
            disabled={!buffer}
            onClick={() => (playing ? pause() : play(position >= (buffer?.duration ?? 0) ? 0 : position))}
          >
            {playing ? <Pause size={15} aria-hidden="true" /> : <Play size={15} aria-hidden="true" />}
          </IconButton>
          <IconButton className="file-preview__tool" label={t("从头播放", "Restart")} disabled={!buffer} onClick={() => play(0)}>
            <RotateCcw size={13} aria-hidden="true" />
          </IconButton>
          <span className="file-preview__time">
            {buffer ? `${formatDuration(position)} / ${formatDuration(buffer.duration)}` : t("正在解码…", "Decoding…")}
          </span>
          <span className="file-preview__volume-group">
            <IconButton
              className="file-preview__tool"
              label={muted ? t("取消静音", "Unmute") : t("静音", "Mute")}
              aria-pressed={muted}
              onClick={() => setMuted((current) => !current)}
            >
              {muted ? <VolumeX size={13} aria-hidden="true" /> : <Volume2 size={13} aria-hidden="true" />}
            </IconButton>
            <input
              className="file-preview__volume"
              type="range"
              min={0}
              max={1}
              step={0.01}
              value={muted ? 0 : volume}
              aria-label={t("音量", "Volume")}
              onChange={(event) => {
                setVolume(Number(event.target.value));
                setMuted(false);
              }}
            />
          </span>
        </div>
        {buffer && (
          <div className="file-preview__meta file-preview__audio-meta">
            {/* The buffer's rate is the output device's, not the file's, so it is not shown. */}
            {t("{channels} 声道", "{channels} channel(s)", { channels: buffer.numberOfChannels })}
            {bytes !== null ? ` · ${formatBytes(bytes)}` : ""}
          </div>
        )}
      </div>
    </div>
  );
}
