import { test, expect } from "@playwright/test";

/* Phase three of the assistant: durable notes about a target, visible token
 * accounting, and a report you can hand to someone else. */
test.beforeEach(async ({ page }) => {
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      "\nwindow.__test = { state, setConnected };" });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__test?.state.term);
  await page.evaluate(() => {
    const { state, setConnected } = window.__test;
    state.mode = "ble";
    state.mgmtReady = true;
    state.mgmtCapabilities = 1 | 2 | (1 << 4);
    state.device = { id: "dev-1" };
    setConnected(true);
  });
  await page.locator("#agentButton").click();
  await page.locator("#agentSettingsButton").click();
  await page.locator("#agentEndpoint").fill("https://agent.test/v1");
  await page.locator("#agentModel").fill("test-model");
  await page.locator("#agentApiKey").fill("test-device-key");
  await page.locator("#agentSettingsSave").click();
  await expect(page.locator("#agentSettingsStatus")).toContainText("saved");
  await page.locator("#drawerClose").click();
  // Prices are display-only, stored apart from the model configuration, and the
  // settings form above has just written them as "not set".
  await page.evaluate(() => localStorage.setItem("linkr-agent-pricing-v1", JSON.stringify({ input: 2, output: 8 })));
});

function sse({ text, tool, usage }) {
  const chunk = (delta, finish) => JSON.stringify({ id: "chatcmpl-test", object: "chat.completion.chunk",
    created: 1, model: "test-model", choices: [{ index: 0, delta, finish_reason: finish }] });
  const delta = tool ? { role: "assistant", tool_calls: [{ index: 0, id: "call_1", type: "function",
    function: { name: tool.name, arguments: JSON.stringify(tool.args) } }] }
    : { role: "assistant", content: text };
  const parts = [`data: ${chunk(delta, null)}`, `data: ${chunk({}, tool ? "tool_calls" : "stop")}`];
  if (usage) {
    parts.push(`data: ${JSON.stringify({ id: "chatcmpl-test", object: "chat.completion.chunk", created: 1,
      model: "test-model", choices: [], usage })}`);
  }
  parts.push("data: [DONE]", "", "");
  return parts.join("\n\n");
}

async function mockModel(page, respond) {
  const bodies = [];
  await page.route("https://agent.test/v1/chat/completions", async (route) => {
    const headers = { "access-control-allow-origin": "*", "access-control-allow-headers": "*" };
    if (route.request().method() === "OPTIONS") { await route.fulfill({ status: 204, headers }); return; }
    bodies.push(route.request().postDataJSON());
    await route.fulfill({ status: 200, headers, contentType: "text/event-stream", body: sse(respond(bodies.length)) });
  });
  return bodies;
}

async function ask(page, question) {
  await page.locator("#agentQuestion").fill(question);
  await page.locator("#agentAsk").click();
}

test("a remembered fact is stored, editable, and returned by get_device_status", async ({ page }) => {
  const bodies = await mockModel(page, (turn) => turn === 1
    ? { tool: { name: "remember_target_note", args: { text: "Bootloader needs raw Enter", evidence: "U-Boot prompt seen at 115200" } } }
    : turn === 2
      ? { tool: { name: "get_device_status", args: {} } }
      : { text: "Noted and re-read.", usage: { prompt_tokens: 1000, completion_tokens: 200, total_tokens: 1200 } });
  await ask(page, "Remember that this bootloader needs raw Enter");
  await expect(page.locator("#agentMessages")).toContainText("Noted and re-read.");

  // The note reaches the model through the status it reads.
  const statusResult = bodies.at(-1).messages.at(-1).content;
  expect(JSON.parse(statusResult).notes).toEqual([
    { id: expect.any(String), text: "Bootloader needs raw Enter", evidence: "U-Boot prompt seen at 115200", createdAt: expect.any(Number) },
  ]);

  // It is listed for the user, and can be deleted.
  await page.locator("#agentHistory > summary").click();
  const note = page.locator("#agentNotes .agent-note");
  await expect(note).toContainText("Bootloader needs raw Enter");
  await note.getByRole("button", { name: /Delete|删除/ }).click();
  await expect(page.locator("#agentNotes .agent-note")).toHaveCount(0);
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem("linkr-agent-notes-v1") || "[]"))).toHaveLength(0);
});

test("token use and the estimated cost are shown for the conversation", async ({ page }) => {
  await mockModel(page, () => ({ text: "Done.", usage: { prompt_tokens: 1_000_000, completion_tokens: 500_000, total_tokens: 1_500_000 } }));
  await ask(page, "Check the target");
  await expect(page.locator("#agentMessages")).toContainText("Done.");
  const usage = page.locator("#agentUsage");
  await expect(usage).toContainText("1,500,000");
  await expect(usage).toContainText("↑1,000,000 ↓500,000");
  // 1M input at 2/M plus 0.5M output at 8/M.
  await expect(usage).toContainText("$6.00");
});

test("prices are validated in the settings form and survive a reload", async ({ page }) => {
  await page.locator("#agentSettingsButton").click();
  await page.locator("#agentPriceInput").fill("0.27");
  await page.locator("#agentPriceOutput").fill("1.1");
  await page.locator("#agentSettingsSave").click();
  await expect(page.locator("#agentSettingsStatus")).toContainText("saved");
  await page.reload();
  await expect(page.locator("#agentButton")).toBeVisible();
  await page.locator("#agentButton").click();
  await page.locator("#agentSettingsButton").click();
  await expect(page.locator("#agentPriceInput")).toHaveValue("0.27");
  await expect(page.locator("#agentPriceOutput")).toHaveValue("1.1");
  await page.locator("#agentPriceInput").fill("-1");
  await page.locator("#agentSettingsSave").click();
  await expect(page.locator("#agentSettingsStatus")).toContainText(/Prices must be numbers|单价必须是/);
});

test("the report export contains the goal, notes and the evidence caution", async ({ page }) => {
  await mockModel(page, (turn) => turn === 1
    ? { tool: { name: "remember_target_note", args: { text: "SD slot is loose", evidence: "reseating fixes boot" } } }
    : { text: "Recorded.", usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } });
  await ask(page, "The board fails to boot from SD");
  await expect(page.locator("#agentMessages")).toContainText("Recorded.");

  await page.locator("#agentHistory > summary").click();
  const download = page.waitForEvent("download");
  await page.locator("#agentExport").click();
  const file = await download;
  expect(file.suggestedFilename()).toMatch(/^linkr-agent-.*\.md$/);
  const stream = await file.createReadStream();
  const chunks = [];
  for await (const chunk of stream) chunks.push(chunk);
  const report = Buffer.concat(chunks).toString("utf8");
  expect(report).toContain("# Linkr Bee diagnostic report");
  expect(report).toContain("The board fails to boot from SD");
  expect(report).toContain("SD slot is loose");
  expect(report).toContain("not proof that the goal was met");
});
