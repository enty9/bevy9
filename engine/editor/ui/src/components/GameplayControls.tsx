import { useCallback, useEffect, useRef, useState } from "react";
import { RotateCcw, SlidersHorizontal, Volume2, VolumeX } from "lucide-react";
import type { BrpClient } from "../protocol/brpClient";

type InputSource =
  | { Key: string }
  | { Mouse: string | object }
  | { GamepadButton: string | object }
  | { GamepadAxis: string | object };

interface Bindings {
  status: string | null;
  sources?: InputSource[];
  bindings: Array<{
    id: string;
    context: string;
    domain: string;
    action: string;
    source: InputSource;
    default_source: InputSource;
    overridden: boolean;
  }>;
}

const buses = ["master", "sfx", "music", "ui", "voice"] as const;
type Bus = (typeof buses)[number];
type BusSettings = { gain: number; low_pass_hz: number; reverb: number };
type Mixer = { available: boolean; buses: Record<Bus, BusSettings> };

function sourceName(source: InputSource): string {
  const entries = Object.entries(source);
  if (entries.length === 0) return "Unbound";
  const [device, input] = entries[0];
  const label = typeof input === "string" ? input : JSON.stringify(input);
  return device === "Key" ? label : `${device}: ${label}`;
}

const MIN_LP_HZ = 20;
const MAX_LP_HZ = 20000;
const LOG_LP_RATIO = Math.log(MAX_LP_HZ / MIN_LP_HZ);

function hzToSlider(hz: number): number {
  const clamped = Math.max(MIN_LP_HZ, Math.min(MAX_LP_HZ, hz));
  return (Math.log(clamped / MIN_LP_HZ) / LOG_LP_RATIO) * 100;
}

function sliderToHz(val: number): number {
  if (val >= 99.5) return MAX_LP_HZ;
  if (val <= 0.5) return MIN_LP_HZ;
  return Math.round(MIN_LP_HZ * Math.exp((val / 100) * LOG_LP_RATIO));
}

function formatHz(hz: number): string {
  if (hz >= 19950) return "Bypass";
  if (hz >= 1000) return `${(hz / 1000).toFixed(1)} kHz`;
  return `${Math.round(hz)} Hz`;
}

interface GameplayControlsProps {
  client: BrpClient;
  physicsDebug: boolean;
  onTogglePhysicsDebug: (enabled: boolean) => Promise<void>;
}

export function GameplayControls({
  client,
  physicsDebug,
  onTogglePhysicsDebug,
}: GameplayControlsProps) {
  const [open, setOpen] = useState(false);
  const [bindings, setBindings] = useState<Bindings | null>(null);
  const [mixer, setMixer] = useState<Mixer | null>(null);
  const [pending, setPending] = useState(false);
  const [capture, setCapture] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);

  const pendingBusRef = useRef<Map<Bus, BusSettings>>(new Map());
  const activeBusRef = useRef<Set<Bus>>(new Set());
  const busTimerRef = useRef<Map<Bus, number>>(new Map());
  const restoredGainRef = useRef<Partial<Record<Bus, number>>>({});

  const flushBus = useCallback(
    async (bus: Bus) => {
      if (activeBusRef.current.has(bus)) {
        return;
      }
      activeBusRef.current.add(bus);
      try {
        while (pendingBusRef.current.has(bus)) {
          const next = pendingBusRef.current.get(bus)!;
          pendingBusRef.current.delete(bus);
          const updated = await client.call<Mixer>("engine.audio.set_bus", {
            bus,
            settings: next,
          });
          if (pendingBusRef.current.has(bus)) continue;
          setMixer((prev) => {
            if (!prev) return updated;
            // Requests for different buses may finish out of order. A response
            // is only authoritative for the bus it changed.
            return {
              available: updated.available,
              buses: { ...prev.buses, [bus]: updated.buses[bus] },
            };
          });
        }
      } catch (failure: unknown) {
        setError(String(failure));
      } finally {
        activeBusRef.current.delete(bus);
      }
    },
    [client],
  );

  useEffect(() => {
    const timers = busTimerRef.current;
    return () => {
      for (const timer of timers.values()) {
        window.clearTimeout(timer);
      }
      timers.clear();
    };
  }, []);

  useEffect(() => {
    if (!open) return;
    let active = true;
    setError(null);
    Promise.all([
      client.call<Bindings>("engine.input.bindings"),
      client.call<Mixer>("engine.audio.mixer"),
    ])
      .then(([nextBindings, nextMixer]) => {
        if (!active) return;
        setBindings(nextBindings);
        setMixer(nextMixer);
      })
      .catch((failure: unknown) => {
        if (active) setError(String(failure));
      });
    const dismiss = (event: PointerEvent) => {
      if (
        event.target instanceof Node &&
        !root.current?.contains(event.target)
      ) {
        setOpen(false);
        setCapture(null);
      }
    };
    document.addEventListener("pointerdown", dismiss);
    return () => {
      active = false;
      document.removeEventListener("pointerdown", dismiss);
    };
  }, [open, client]);

  const bind = useCallback(
    async (id: string, source: InputSource | null) => {
      setCapture(null);
      setPending(true);
      setError(null);
      try {
        setBindings(
          await client.call<Bindings>("engine.input.rebind", { id, source }),
        );
      } catch (failure: unknown) {
        setError(String(failure));
      } finally {
        setPending(false);
      }
    },
    [client],
  );

  useEffect(() => {
    if (!capture) return;
    const bindingId = capture;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.code === "Tab") return;
      if (e.target instanceof HTMLSelectElement) {
        if (e.code === "Escape") setCapture(null);
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      if (e.code === "Escape") {
        setCapture(null);
        return;
      }
      if (!e.repeat && e.code && e.code !== "Unidentified") {
        const code = e.code.startsWith("Meta")
          ? e.code.replace("Meta", "Super")
          : e.code;
        void bind(bindingId, { Key: code });
      }
    };
    const cancelCapture = () => setCapture(null);
    window.addEventListener("keydown", onKeyDown, { capture: true });
    window.addEventListener("blur", cancelCapture);
    return () => {
      window.removeEventListener("keydown", onKeyDown, { capture: true });
      window.removeEventListener("blur", cancelCapture);
    };
  }, [capture, bind]);

  function scheduleBusUpdate(bus: Bus, nextSettings: BusSettings, immediate = false) {
    setMixer((prev) =>
      prev
        ? {
            ...prev,
            buses: {
              ...prev.buses,
              [bus]: nextSettings,
            },
          }
        : prev,
    );
    pendingBusRef.current.set(bus, nextSettings);
    const prevTimer = busTimerRef.current.get(bus);
    if (prevTimer !== undefined) {
      window.clearTimeout(prevTimer);
      busTimerRef.current.delete(bus);
    }
    if (immediate) {
      void flushBus(bus);
    } else {
      const timer = window.setTimeout(() => {
        busTimerRef.current.delete(bus);
        void flushBus(bus);
      }, 45);
      busTimerRef.current.set(bus, timer);
    }
  }

  return (
    <div className="popover-anchor" ref={root}>
      <button
        type="button"
        className={open || physicsDebug ? "primary" : ""}
        onClick={() => {
          setOpen((v) => !v);
          setCapture(null);
        }}
        aria-expanded={open}
        title="Input bindings, physics wireframes, and audio mixer"
      >
        <SlidersHorizontal size={14} /> Gameplay
      </button>

      {open && (
        <section
          className="popover-card gameplay-popover"
          aria-label="Gameplay settings"
        >
          <div className="gameplay-section">
            <div className="popover-title">Physics Wireframes</div>
            <label className="gameplay-toggle-row">
              <input
                type="checkbox"
                checked={physicsDebug}
                onChange={(e) => {
                  const enabled = e.currentTarget.checked;
                  void onTogglePhysicsDebug(enabled).catch((err: unknown) =>
                    setError(String(err)),
                  );
                }}
              />
              <span>Show collider outlines in Viewport</span>
            </label>
            <div className="physics-legend">
              <span>
                <i className="legend-dot dynamic" /> Dynamic
              </span>
              <span>
                <i className="legend-dot static" /> Static
              </span>
              <span>
                <i className="legend-dot kinematic" /> Kinematic
              </span>
              <span>
                <i className="legend-dot trigger" /> Trigger
              </span>
            </div>
          </div>

          <div className="gameplay-section">
            <div className="popover-title">Input Bindings</div>
            {bindings?.status && <div className="muted">{bindings.status}</div>}
            {bindings?.bindings.map((row) => {
              const isCapturing = capture === row.id;
              return (
                <div
                  key={row.id}
                  onBlur={(event) => {
                    if (isCapturing && !event.currentTarget.contains(event.relatedTarget)) {
                      setCapture(null);
                    }
                  }}
                >
                  <div className="gameplay-binding">
                    <div
                      className="gameplay-binding-info"
                      title={`${row.domain} / ${row.context} / ${row.id}`}
                    >
                      <span className="truncate">
                        <strong>{row.action}</strong>
                        {row.id !== row.action && (
                          <span className="gameplay-binding-id"> · {row.id}</span>
                        )}
                      </span>
                      <small className="muted">
                        {row.domain} / {row.context}
                      </small>
                    </div>
                    <button
                      type="button"
                      className={isCapturing ? "primary" : ""}
                      disabled={pending}
                      onClick={() =>
                        setCapture((prev) => (prev === row.id ? null : row.id))
                      }
                    >
                      {isCapturing ? "Press a key…" : sourceName(row.source)}
                    </button>
                    <button
                      type="button"
                      className="ghost-btn icon-mini"
                      disabled={!row.overridden || pending}
                      title={`Restore ${sourceName(row.default_source)}`}
                      onClick={() => void bind(row.id, null)}
                    >
                      <RotateCcw size={12} />
                    </button>
                  </div>
                  {isCapturing && !!bindings.sources?.length && (
                    <select
                      aria-label={`Mouse or gamepad source for ${row.action}`}
                      value=""
                      onChange={(event) => {
                        const index = event.currentTarget.value;
                        const source = bindings.sources?.[Number(index)];
                        if (index !== "" && source) void bind(row.id, source);
                      }}
                    >
                      <option value="" disabled>Or choose mouse / gamepad…</option>
                      {bindings.sources.map((source, index) => (
                        <option key={sourceName(source)} value={index}>
                          {sourceName(source)}
                        </option>
                      ))}
                    </select>
                  )}
                </div>
              );
            })}
          </div>

          <div className="gameplay-section">
            <div className="popover-title">Audio Mixer</div>
            {mixer && !mixer.available && (
              <div className="muted">Audio output device unavailable.</div>
            )}
            {mixer && (
              <div className="gameplay-mixer-list">
                {buses.map((bus) => {
                  const settings = mixer.buses[bus];
                  const muted = settings.gain <= 0.001;
                  return (
                    <div className="gameplay-bus-card" key={bus}>
                      <div className="gameplay-bus-header">
                        <strong>{bus.toUpperCase()}</strong>
                        <div className="panel-actions">
                          <button
                            type="button"
                            className="ghost-btn icon-mini"
                            title={muted ? `Unmute ${bus}` : `Mute ${bus}`}
                            onClick={() => {
                              if (muted) {
                                const restore =
                                  restoredGainRef.current[bus] &&
                                  restoredGainRef.current[bus]! > 0.01
                                    ? restoredGainRef.current[bus]!
                                    : 1.0;
                                scheduleBusUpdate(
                                  bus,
                                  { ...settings, gain: restore },
                                  true,
                                );
                              } else {
                                restoredGainRef.current[bus] = settings.gain;
                                scheduleBusUpdate(
                                  bus,
                                  { ...settings, gain: 0 },
                                  true,
                                );
                              }
                            }}
                          >
                            {muted ? (
                              <VolumeX size={13} />
                            ) : (
                              <Volume2 size={13} />
                            )}
                          </button>
                          <button
                            type="button"
                            className="ghost-btn icon-mini"
                            title={`Reset ${bus} bus`}
                            onClick={() =>
                              scheduleBusUpdate(
                                bus,
                                { gain: 1, low_pass_hz: 20000, reverb: 0 },
                                true,
                              )
                            }
                          >
                            <RotateCcw size={11} />
                          </button>
                        </div>
                      </div>
                      <label className="gameplay-slider-row">
                        <span className="muted">Gain</span>
                        <input
                          type="range"
                          className="gameplay-slider"
                          min={0}
                          max={1}
                          step={0.01}
                          value={settings.gain}
                          aria-label={`${bus} gain`}
                          onChange={(e) =>
                            scheduleBusUpdate(bus, {
                              ...settings,
                              gain: e.currentTarget.valueAsNumber,
                            })
                          }
                        />
                        <span className="gameplay-slider-val">
                          {Math.round(settings.gain * 100)}%
                        </span>
                      </label>
                      <label className="gameplay-slider-row">
                        <span className="muted">Low-pass</span>
                        <input
                          type="range"
                          className="gameplay-slider"
                          min={0}
                          max={100}
                          step={0.5}
                          value={hzToSlider(settings.low_pass_hz)}
                          aria-label={`${bus} low_pass_hz`}
                          onChange={(e) =>
                            scheduleBusUpdate(bus, {
                              ...settings,
                              low_pass_hz: sliderToHz(
                                e.currentTarget.valueAsNumber,
                              ),
                            })
                          }
                        />
                        <span className="gameplay-slider-val">
                          {formatHz(settings.low_pass_hz)}
                        </span>
                      </label>
                      <label className="gameplay-slider-row">
                        <span className="muted">Reverb</span>
                        <input
                          type="range"
                          className="gameplay-slider"
                          min={0}
                          max={1}
                          step={0.01}
                          value={settings.reverb}
                          aria-label={`${bus} reverb`}
                          onChange={(e) =>
                            scheduleBusUpdate(bus, {
                              ...settings,
                              reverb: e.currentTarget.valueAsNumber,
                            })
                          }
                        />
                        <span className="gameplay-slider-val">
                          {Math.round(settings.reverb * 100)}%
                        </span>
                      </label>
                    </div>
                  );
                })}
              </div>
            )}
          </div>

          {error && (
            <div role="alert" className="badge crashed">
              {error}
            </div>
          )}
        </section>
      )}
    </div>
  );
}
