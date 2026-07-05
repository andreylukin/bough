import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./global.css";
import App from "./App";
import { api } from "./api";
import { applyTheme } from "./theme";

// Saved theme (if any) overrides the :root palette. Fire-and-forget: mock mode
// and a dead backend just keep the defaults.
api.theme().then((r) => applyTheme(r.theme?.colors)).catch(() => {});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>
);
