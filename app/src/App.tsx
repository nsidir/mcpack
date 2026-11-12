import { Package } from "lucide-react";
import { useEffect } from "react";
import { AddContentDialog, CommandPalette, ExportPackDialog, NewPackDialog } from "./components/Dialogs";
import { Sidebar, StatusBar, Titlebar, Toasts } from "./components/Shell";
import { Button, Empty } from "./components/ui";
import { importPack, launch, type PackTab, usePack, useStore } from "./store";
import { PackView } from "./views/PackView";
import { ActivityPage, SettingsPage } from "./views/Pages";

const TABS: PackTab[] = ["overview", "content", "console", "agent"];

function useTheme() {
  const theme = useStore((s) => s.theme);
  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: light)");
    const apply = () => {
      document.documentElement.dataset.theme = theme === "system" ? (mq.matches ? "light" : "dark") : theme;
    };
    apply();
    mq.addEventListener("change", apply);
    return () => mq.removeEventListener("change", apply);
  }, [theme]);
}

function useShortcuts() {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
      const s = useStore.getState();
      const pack = s.packs.find((p) => p.id === s.packId);
      if (e.key === "k") {
        e.preventDefault();
        s.open(s.dialog === "palette" ? null : "palette");
      } else if (s.dialog) {
        return;
      } else if (e.key === ",") {
        e.preventDefault();
        s.setView("settings");
      } else if (e.key === "Enter" && pack) {
        e.preventDefault();
        launch(pack);
      } else if (e.key >= "1" && e.key <= "4" && pack) {
        e.preventDefault();
        s.setTab(TABS[Number(e.key) - 1]);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

export function App() {
  const ready = useStore((s) => s.ready);
  const view = useStore((s) => s.view);
  const open = useStore((s) => s.open);
  const pack = usePack();
  useTheme();
  useShortcuts();

  return (
    <div className="app">
      <Titlebar />
      <Sidebar />
      <main className="main">
        {!ready ? null : view === "activity" ? (
          <ActivityPage />
        ) : view === "settings" ? (
          <SettingsPage />
        ) : pack ? (
          <PackView key={pack.id} pack={pack} />
        ) : (
          <Empty
            icon={<Package size={28} strokeWidth={1.5} />}
            title="No packs yet"
            action={
              <div style={{ display: "flex", gap: 8 }}>
                <Button variant="primary" onClick={() => open("new-pack")}>
                  New pack
                </Button>
                <Button onClick={() => void importPack()}>Import modpack…</Button>
              </div>
            }
          >
            A pack is a folder with a pack.toml. Create one here, import a .mrpack or CurseForge zip, or let an agent do it.
          </Empty>
        )}
      </main>
      <StatusBar />
      <CommandPalette />
      <AddContentDialog />
      <NewPackDialog />
      <ExportPackDialog />
      <Toasts />
    </div>
  );
}
