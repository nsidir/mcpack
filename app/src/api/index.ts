import { mockBackend } from "./mock";
import { nativeBackend } from "./native";
import type { Backend } from "./types";

export * from "./types";

/** Running inside the Tauri shell (as opposed to a plain browser tab)? */
export const isDesktop = typeof window !== "undefined" && "__TAURI__" in window;

// Inside the desktop shell the real launcher core is used; a plain browser tab (pnpm dev)
// runs on the simulated backend. Force the simulation in the shell with VITE_MCPACK_BACKEND=mock.
export const backend: Backend =
  isDesktop && import.meta.env.VITE_MCPACK_BACKEND !== "mock" ? nativeBackend : mockBackend;
