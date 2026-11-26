import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles.css";

// Disable browser/WebView page zooming (Ctrl+Wheel, Ctrl+/- /0, trackpad pinch)
// without stopping propagation so the Viewport canvas camera zoom keeps working.
window.addEventListener(
  "wheel",
  (event) => {
    if (event.ctrlKey || event.metaKey) {
      event.preventDefault();
    }
  },
  { passive: false },
);

window.addEventListener(
  "keydown",
  (event) => {
    if (
      (event.ctrlKey || event.metaKey) &&
      (["+", "=", "-", "_", "0"].includes(event.key) ||
        [
          "Equal",
          "Minus",
          "Digit0",
          "NumpadAdd",
          "NumpadSubtract",
          "Numpad0",
        ].includes(event.code))
    ) {
      event.preventDefault();
    }
  },
  { passive: false },
);

for (const gestureEvent of ["gesturestart", "gesturechange", "gestureend"]) {
  window.addEventListener(
    gestureEvent,
    (event) => {
      event.preventDefault();
    },
    { passive: false },
  );
}

const rootElement = document.getElementById("root");
if (!rootElement) {
  throw new Error("Root container '#root' not found");
}

createRoot(rootElement).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
