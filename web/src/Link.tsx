/**
 * An in-app link.
 *
 * A real `<a href>`, so it behaves like one: middle-click opens a tab, the status
 * bar shows where it goes, a crawler can follow it, and Cmd-click still works
 * because those arrive as modified clicks we decline to handle. Only a plain
 * left-click is intercepted, and only then to keep the guest alive across the
 * navigation.
 */

import type { ReactNode } from "react";
import { navigate } from "./router";

export function Link({ to, children }: { to: string; children: ReactNode }) {
  return (
    <a
      href={to}
      onClick={(event) => {
        // Let the browser handle anything that is not a plain left-click: a
        // modified click means the reader asked for a new tab or window, and
        // swallowing it would be taking that away from them.
        if (event.defaultPrevented) return;
        if (event.button !== 0) return;
        if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;

        event.preventDefault();
        navigate(to);
      }}
    >
      {children}
    </a>
  );
}
