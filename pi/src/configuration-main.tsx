import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { Configuration } from "./Configuration";
import "./config.css";
import "./sdpi.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Configuration />
  </StrictMode>,
);
