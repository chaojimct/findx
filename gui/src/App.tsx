import { getCurrentWindow } from "@tauri-apps/api/window";
import { useState } from "react";
import "./findx/design-tokens.css";
import FindXSearchApp from "./findx/FindXSearchApp";
import SettingsWindow from "./findx/SettingsWindow";

function detectWindowLabel(): string {
  try {
    return getCurrentWindow().label;
  } catch {
    return "main";
  }
}

export default function App() {
  const [winLabel] = useState(() => detectWindowLabel());
  if (winLabel === "settings") {
    return <SettingsWindow />;
  }
  return <FindXSearchApp />;
}
