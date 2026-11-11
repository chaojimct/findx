import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useState, type ReactNode } from "react";
import "./findx/design-tokens.css";
import FindXSearchApp from "./findx/FindXSearchApp";
import SettingsWindow from "./findx/SettingsWindow";

function isUpdateFixture(): boolean {
  try {
    return new URLSearchParams(window.location.search).get("fxFixture") === "update";
  } catch {
    return false;
  }
}

function detectWindowLabel(): string {
  try {
    return getCurrentWindow().label;
  } catch {
    return "main";
  }
}

export default function App() {
  const [winLabel] = useState(() => detectWindowLabel());
  const [fixture, setFixture] = useState<ReactNode>(null);
  const showFixture = import.meta.env.DEV && isUpdateFixture();
  useEffect(() => {
    if (!showFixture) return;
    let alive = true;
    void import("./findx/dev/UpdateLayoutFixture").then((mod) => {
      if (alive) setFixture(<mod.UpdateLayoutFixture />);
    });
    return () => {
      alive = false;
    };
  }, [showFixture]);
  if (showFixture) return <>{fixture}</>;
  if (winLabel === "settings") {
    return <SettingsWindow />;
  }
  return <FindXSearchApp />;
}
