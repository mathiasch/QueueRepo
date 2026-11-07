/** Accessibility (axe-core) and screenshots on the iPhone, light + dark (portrait: all views, landscape: queue + drawer). */
import { test } from "@playwright/test";
import { simulateInsets } from "../helpers";
import { tour } from "../tour";

test("all main views are accessible in light and dark mode (iPhone)", async ({ page }, info) => {
  test.setTimeout(180_000);
  // Screenshots and checks with Dynamic Island / home indicator insets.
  const landscape = await simulateInsets(page);
  await tour(page, info, landscape ? "iphone-landscape" : "iphone", !landscape);
});
