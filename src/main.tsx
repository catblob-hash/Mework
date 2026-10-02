import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles.css";
import { installExternalLinkInterceptor } from "./lib/externalLinks";
import { installPathLinkInterceptor } from "./lib/pathLinks";
import { startScrollChaining } from "./lib/scrollChaining";
import { installSelectionRegions } from "./lib/selectionRegions";
import { startApplicationAppearance } from "./theme";

startApplicationAppearance();
startScrollChaining();
installExternalLinkInterceptor();
installPathLinkInterceptor();
installSelectionRegions();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>
);
