import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { Shell } from "./Shell";
import "./index.css";

const root = document.getElementById("root");
if (!root) throw new Error("no #root");

createRoot(root).render(
  <StrictMode>
    {/*
      The emulator is constructed here and handed to the shell, so it sits above
      the route switch: changing chapter must not unmount the guest.
    */}
    <Shell app={<App />} />
  </StrictMode>,
);
