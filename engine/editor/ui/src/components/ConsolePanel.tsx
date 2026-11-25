import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowDownToLine, Search, Trash2 } from "lucide-react";
import type { EngineLogMessage } from "../lib/tauriBridge";

interface ConsolePanelProps {
  logs: EngineLogMessage[];
  onClear: () => void;
  onCollapse: () => void;
}

const ALL_LEVELS = ["error", "warn", "info", "debug", "trace"] as const;
type LogFilterLevel = (typeof ALL_LEVELS)[number];

function normalizeLevel(raw: string): LogFilterLevel {
  const lower = raw.toLowerCase();
  if (lower === "error" || lower === "warn" || lower === "info" || lower === "debug" || lower === "trace") {
    return lower;
  }
  return "info";
}

function formatTime(timestampMs: number): string {
  const date = new Date(timestampMs);
  const hh = String(date.getHours()).padStart(2, "0");
  const mm = String(date.getMinutes()).padStart(2, "0");
  const ss = String(date.getSeconds()).padStart(2, "0");
  return `${hh}:${mm}:${ss}`;
}

export function ConsolePanel({ logs, onClear, onCollapse }: ConsolePanelProps) {
  const [enabledLevels, setEnabledLevels] = useState<Record<LogFilterLevel, boolean>>({
    error: true,
    warn: true,
    info: true,
    debug: false,
    trace: false,
  });
  const [search, setSearch] = useState("");
  const [autoScroll, setAutoScroll] = useState(true);
  const listRef = useRef<HTMLDivElement | null>(null);

  const counts = useMemo(() => {
    const map: Record<LogFilterLevel, number> = {
      error: 0,
      warn: 0,
      info: 0,
      debug: 0,
      trace: 0,
    };
    for (const entry of logs) {
      map[normalizeLevel(entry.level)] += 1;
    }
    return map;
  }, [logs]);

  const filteredLogs = useMemo(() => {
    const needle = search.trim().toLowerCase();
    return logs.filter((entry) => {
      const level = normalizeLevel(entry.level);
      if (!enabledLevels[level]) {
        return false;
      }
      if (!needle) {
        return true;
      }
      return (
        entry.message.toLowerCase().includes(needle) ||
        entry.target.toLowerCase().includes(needle)
      );
    });
  }, [logs, enabledLevels, search]);

  useEffect(() => {
    if (autoScroll && listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
  }, [filteredLogs, autoScroll]);

  function toggleLevel(level: LogFilterLevel) {
    setEnabledLevels((prev) => ({ ...prev, [level]: !prev[level] }));
  }

  return (
    <div className="panel">
      <div className="panel-header">
        <div className="panel-actions">
          <span>Console</span>
          <div className="console-filters">
            {ALL_LEVELS.map((level) => (
              <button
                key={level}
                type="button"
                className={`console-level-btn ${level} ${
                  enabledLevels[level] ? "active" : ""
                }`}
                onClick={() => toggleLevel(level)}
              >
                {level.toUpperCase()} ({counts[level]})
              </button>
            ))}
          </div>
        </div>

        <div className="panel-actions">
          <div className="console-search">
            <Search size={12} className="muted" />
            <input
              type="text"
              className="console-search-input"
              value={search}
              placeholder="Filter logs..."
              onChange={(e) => setSearch(e.target.value)}
            />
          </div>

          <button
            type="button"
            className={`ghost-btn ${autoScroll ? "active-toggle" : ""}`}
            title="Toggle auto-scroll"
            onClick={() => setAutoScroll((v) => !v)}
          >
            <ArrowDownToLine size={13} />
          </button>

          <button
            type="button"
            className="ghost-btn"
            title="Clear logs"
            onClick={onClear}
          >
            <Trash2 size={13} />
            Clear
          </button>

          <button
            type="button"
            className="ghost-btn"
            title="Collapse Console"
            onClick={onCollapse}
          >
            ×
          </button>
        </div>
      </div>

      <div ref={listRef} className="panel-body log-list">
        {filteredLogs.length === 0 ? (
          <div className="muted">No log messages matching active filters.</div>
        ) : (
          filteredLogs.map((entry, idx) => {
            const level = normalizeLevel(entry.level);
            return (
              <div key={`${entry.timestamp_ms}-${idx}`} className="log-row">
                <span className="muted">{formatTime(entry.timestamp_ms)}</span>
                <span className={`log-level ${level}`}>
                  {level.toUpperCase()}
                </span>
                <span className="muted">{entry.target}</span>
                <span>{entry.message}</span>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
