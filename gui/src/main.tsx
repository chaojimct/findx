import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

/** 首屏前同步主题，避免深色用户白屏闪一下（与 FindXSearchApp 内逻辑一致） */
function syncFxThemeFromStorage() {
  try {
    const t = localStorage.getItem("findx2_ui_theme");
    let eff: "light" | "dark" = "light";
    if (t === "dark") eff = "dark";
    else if (t === "system") {
      eff = window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
    }
    document.documentElement.setAttribute("data-fx-theme", eff);
  } catch {
    document.documentElement.setAttribute("data-fx-theme", "light");
  }
}
syncFxThemeFromStorage();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
