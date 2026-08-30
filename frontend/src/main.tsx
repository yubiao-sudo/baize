import React from "react";
import ReactDOM from "react-dom/client";
import "./theme/crystal.tokens.css";
import "./theme/workshop.css";
import Root from "./shell/Root";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);