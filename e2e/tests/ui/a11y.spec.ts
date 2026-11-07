/** Accessibility (axe-core) and screenshots of all main views, desktop, light + dark. */
import { test } from "@playwright/test";
import { tour } from "../tour";

test("all main views are accessible in light and dark mode (desktop)", async ({ page }, info) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1366, height: 900 });
  await tour(page, info, "desktop", true);
});
