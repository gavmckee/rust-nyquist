const { chromium } = require("playwright");
const fs = require("fs");
const os = require("os");
const path = require("path");
const baseUrl = process.env.EXPLORER_URL || "http://127.0.0.1:9101";
const fixtures = process.env.EXPLORER_FIXTURES || "/tmp/nyquist-explorer-demo";
const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), "nyquist-browser-"));
console.log("Browser artifacts:", artifacts);
(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({
    viewport: { width: 1440, height: 1100 },
  });
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto(baseUrl);
  await page.waitForFunction(
    () => document.querySelector("#server-file").options.length > 1,
  );
  await page.screenshot({
    path: path.join(artifacts, "empty.png"),
    fullPage: true,
  });
  await page.selectOption("#server-file", { index: 1 });
  await page.click("#open-server");
  await page.waitForFunction(
    () => document.querySelector("#row-count").textContent === "480",
  );
  await page
    .getByRole("button", { name: "network/receive/bytes", exact: true })
    .click();
  await page.screenshot({
    path: path.join(artifacts, "loaded.png"),
    fullPage: true,
  });
  await page.selectOption('select[data-key="iface"]', JSON.stringify("eth1"));
  await page.waitForFunction(
    () => document.querySelector("#filtered-count").textContent === "120 rows",
  );
  await page.selectOption("#measure", "rate");
  await page.waitForFunction(() =>
    document.querySelector("#plot-note").textContent.includes("Resets"),
  );
  await page.fill("#from", "2023-11-14T22:14:20");
  await page.fill("#to", "2023-11-14T22:15:20");
  await page.locator("#to").blur();
  await page.waitForFunction(
    () => document.querySelector("#filtered-count").textContent === "7 rows",
  );
  await page.click("#reset-range");
  await page.waitForFunction(
    () => document.querySelector("#filtered-count").textContent === "120 rows",
  );
  await page.locator("#legend button").click();
  await page.waitForFunction(
    () => document.querySelector("#filtered-count").textContent === "0 rows",
  );
  await page.locator("#legend button").click();
  const download = page.waitForEvent("download");
  await page.click("#export");
  const d = await download;
  await d.saveAs(path.join(artifacts, "selection.csv"));
  const csv = fs.readFileSync(path.join(artifacts, "selection.csv"), "utf8");
  if (csv.split("\r\n").length !== 121) throw Error("Wrong CSV row count");
  await page.click("#next");
  if (!(await page.textContent("#page")).includes("Page 2"))
    throw Error("Pagination failed");
  const fixture = fs.readdirSync(fixtures).find((n) => n.endsWith(".parquet"));
  await page.setInputFiles("#upload", path.join(fixtures, fixture));
  await page.waitForFunction(
    () => document.querySelector("#file-chips").children.length === 2,
  );
  if ((await page.textContent("#row-count")) !== "480")
    throw Error("Duplicate observations not deduplicated");
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({
    path: path.join(artifacts, "mobile.png"),
    fullPage: true,
  });
  if (
    await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)
  )
    throw Error("Mobile horizontal overflow");
  await page.click("#clear");
  if (!(await page.isVisible("#empty"))) throw Error("Clear failed");
  if (await page.locator("#metric-list button").count())
    throw Error("Clear left stale metric navigation");
  await page.setInputFiles("#upload", {
    name: "bad.parquet",
    mimeType: "application/octet-stream",
    buffer: Buffer.from("bad parquet"),
  });
  await page.waitForFunction(() =>
    document.querySelector("#status").classList.contains("error"),
  );
  if (errors.length) throw Error(errors.join("\n"));
  console.log(
    "PASS: server load, chart/filter/rate, CSV, pagination, upload, deduplication, mobile layout, clear, invalid file",
  );
  await browser.close();
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
