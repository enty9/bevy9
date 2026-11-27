import { useEffect, useRef, useState } from "react";
import { SlidersHorizontal } from "lucide-react";
import type { BrpClient } from "../protocol/brpClient";

type InputSource = { Key: string } | { Mouse: string | object } |
  { GamepadButton: string | object } | { GamepadAxis: string | object };

interface Bindings {
  status: string | null;
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
type DebugState = { enabled: boolean };

function sourceName(source: InputSource): string {
  const [device, input] = Object.entries(source)[0];
  const label = typeof input === "string" ? input : JSON.stringify(input);
  return device === "Key" ? label : `${device}: ${label}`;
}

export function GameplayControls({ client }: { client: BrpClient }) {
  const [open, setOpen] = useState(false);
  const [bindings, setBindings] = useState<Bindings | null>(null);
  const [mixer, setMixer] = useState<Mixer | null>(null);
  const [pending, setPending] = useState(false);
  const [capture, setCapture] = useState<string | null>(null);
  const [debug, setDebug] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    let active = true;
    setError(null);
    Promise.all([
      client.call<Bindings>("engine.input.bindings"),
      client.call<Mixer>("engine.audio.mixer"),
      client.call<DebugState>("engine.physics.debug"),
    ]).then(([nextBindings, nextMixer, nextDebug]) => {
      if (!active) return;
      setBindings(nextBindings);
      setMixer(nextMixer);
      setDebug(nextDebug.enabled);
    }).catch((failure: unknown) => {
      if (active) setError(String(failure));
    });
    const dismiss = (event: PointerEvent) => {
      if (event.target instanceof Node && !root.current?.contains(event.target)) {
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

  async function change<T>(rpc: string, params: unknown, apply: (value: T) => void) {
    setPending(true);
    setError(null);
    try { apply(await client.call<T>(rpc, params)); }
    catch (failure: unknown) { setError(String(failure)); }
    finally { setPending(false); }
  }

  function bind(id: string, source: InputSource | null) {
    setCapture(null);
    void change("engine.input.rebind", { id, source }, setBindings);
  }

  function changeBus(bus: Bus, field: keyof BusSettings, value: number) {
    if (!mixer) return;
    void change("engine.audio.set_bus", { bus, settings: { ...mixer.buses[bus], [field]: value } }, setMixer);
  }

  return (
    <div className="popover-anchor" ref={root}>
      <button type="button" onClick={() => { setOpen(!open); setCapture(null); }} aria-expanded={open} title="Input, physics and audio settings">
        <SlidersHorizontal size={14} /> Gameplay
      </button>
      {open && (
        <section className="popover-card gameplay-popover" aria-label="Gameplay settings">
          <label><input type="checkbox" checked={debug} disabled={pending} onChange={(e) => {
            void change<DebugState>("engine.physics.debug", { enabled: e.currentTarget.checked }, (state) => setDebug(state.enabled));
          }} /> Viewport collider outlines</label>
          <div className="popover-title">Input bindings</div>
          {bindings?.status && <div className="muted">{bindings.status}</div>}
          {bindings?.bindings.map((row) => (
            <div className="gameplay-binding" key={row.id}>
              <span title={`${row.domain} / ${row.context} / ${row.id}`}>{row.action} <small className="muted">{row.context}</small></span>
              <button type="button" disabled={pending} onBlur={() => setCapture(null)} onClick={() => setCapture(row.id)} onKeyDown={(e) => {
                if (capture !== row.id) return;
                e.preventDefault();
                e.stopPropagation();
                if (e.code === "Escape" || e.code === "Tab") {
                  setCapture(null);
                } else if (!e.repeat && e.code && e.code !== "Unidentified") {
                  const code = e.code.startsWith("Meta") ? e.code.replace("Meta", "Super") : e.code;
                  bind(row.id, { Key: code });
                }
              }}>{capture === row.id ? "Press a key…" : sourceName(row.source)}</button>
              <button type="button" disabled={!row.overridden || pending} title={`Restore ${sourceName(row.default_source)}`} onClick={() => bind(row.id, null)}>Reset</button>
            </div>
          ))}
          <div className="popover-title">Audio mixer</div>
          {mixer && !mixer.available && <div className="muted">Audio device unavailable.</div>}
          {mixer && (
            <div className="gameplay-mixer">
              <span /> <span>Gain</span> <span>Low-pass (Hz)</span> <span>Reverb</span>
              {buses.map((bus) => (
                <div className="gameplay-bus" key={bus}>
                  <b>{bus.toUpperCase()}</b>
                  {(["gain", "low_pass_hz", "reverb"] as const).map((field) => (
                    <input key={`${field}-${mixer.buses[bus][field]}`} className="text-input" type="number"
                      aria-label={`${bus} ${field}`} defaultValue={mixer.buses[bus][field]} disabled={pending}
                      min={field === "low_pass_hz" ? 20 : 0} max={field === "low_pass_hz" ? 20000 : 1}
                      step={field === "low_pass_hz" ? 100 : 0.05} onBlur={(e) => {
                        const value = e.currentTarget.valueAsNumber;
                        if (Number.isFinite(value) && value !== mixer.buses[bus][field]) changeBus(bus, field, value);
                      }} onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }} />
                  ))}
                </div>
              ))}
            </div>
          )}
          {error && <div role="alert" className="muted">{error}</div>}
        </section>
      )}
    </div>
  );
}
