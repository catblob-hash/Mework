import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "katex/dist/katex.min.css";
import "./styles.css";
import { installExternalLinkInterceptor } from "./lib/externalLinks";
import { startScrollChaining } from "./lib/scrollChaining";
import { startApplicationAppearance } from "./theme";

startApplicationAppearance();
startScrollChaining();
installExternalLinkInterceptor();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>
);
