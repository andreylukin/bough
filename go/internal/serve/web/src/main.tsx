import { createRoot } from "react-dom/client";
import App from "./app";

const el = document.getElementById("root");
if (!el) throw new Error("bough: control room: #root missing from the page shell");
createRoot(el).render(<App />);
