import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// Testing Library only registers its own auto-cleanup when the test framework's
// globals are injected. This project imports `describe`/`it`/`expect` explicitly
// instead, so cleanup has to be wired up here — without it, every `render` leaves its
// DOM behind and queries start matching elements from earlier tests.
afterEach(cleanup);
