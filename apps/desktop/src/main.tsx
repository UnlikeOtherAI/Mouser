import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./app";
import { WorkspaceProvider } from "./lib/workspace-provider";
import "./styles/global.css";

const rootEl = document.getElementById("root");
if (!rootEl) {
  throw new Error("Root element #root not found");
}

ReactDOM.createRoot(rootEl).render(
  <React.StrictMode>
    <WorkspaceProvider>
      <App />
    </WorkspaceProvider>
  </React.StrictMode>,
);
