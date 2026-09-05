import { expect, test } from "@playwright/test";

test("cached application shell reloads offline and reports the local service offline", async ({
  context,
  page
}) => {
  await page.goto("/");
  await expect(page.getByText("AUSHADHARTH").first()).toBeVisible();

  await page.evaluate(async () => {
    if (!("serviceWorker" in navigator)) throw new Error("Service workers are unavailable");
    await navigator.serviceWorker.ready;
  });

  // A reload gives the newly activated worker control before network is disabled.
  await page.reload();
  await expect
    .poll(() => page.evaluate(() => Boolean(navigator.serviceWorker.controller)))
    .toBe(true);

  await context.setOffline(true);
  try {
    await page.reload({ waitUntil: "domcontentloaded" });
    await expect(page.getByText("AUSHADHARTH").first()).toBeVisible();
    await expect(page.getByRole("heading", { name: "Local Store Service Unavailable" })).toBeVisible();
  } finally {
    await context.setOffline(false);
  }
});
