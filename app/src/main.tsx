import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { useStore } from "./store";
import "./styles.css";

void useStore.getState().init();

// A desktop app shouldn't offer the browser's page menu (Back, Reload, View source).
window.addEventListener("contextmenu", (e) => {
  const t = e.target as HTMLElement;
  if (!t.closest("input, textarea, .selectable, .console, .repl-out, pre")) e.preventDefault();
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
