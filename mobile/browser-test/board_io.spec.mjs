import { test, expect } from "@playwright/test";

/* The board section drives the Linkr Bee X1.0's own hardware over the
 * management channel: the single-wire RGB status LED, the optocoupler dry
 * contact and the IR remote with its learned-slot table. These tests run the
 * shipped app against a scripted management reply, so they cover the panel,
 * the commands it sends and how ERR/unsupported replies are surfaced. */

async function openApp(page) {
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      "\nwindow.__test = { state, setConnected, readBoardPrompt, setControl: fn => { sendControl = fn; } };" });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__test?.state.term);
}

/* Reply script: `replies[command]` is a string, or a function returning one.
 * The learning script changes state on `@r learn` so the poll loop sees an
 * active capture and then a finished one, exactly like the firmware. */
async function mockBoard(page, { replies = {}, learning = false, names = {}, capabilities = 7 } = {}) {
  await page.evaluate(({ replies, learning, names, capabilities }) => {
    const { state, setConnected } = window.__test;
    window.boardCommands = [];
    window.boardScript = replies;
    window.boardLearn = null;
    window.boardNames = { ...names };
    window.boardPrompt = "";
    window.boardAiconf = "";
    window.boardAiconfStage = null;
    state.mode = "ble";
    state.mgmtReady = true;
    state.mgmtCapabilities = capabilities;
    window.__test.setControl(async (command) => {
      window.boardCommands.push(command);
      /* Explicit per-test replies win over the mock's built-in devices. */
      const scripted = window.boardScript[command];
      if (scripted !== undefined) {
        return typeof scripted === "function" ? scripted(command) : scripted;
      }
      const encodeName = (text) => Array.from(new TextEncoder().encode(text),
        (byte) => byte.toString(16).padStart(2, "0")).join("");
      const decodeName = (hex) => new TextDecoder().decode(new Uint8Array(
        (hex.match(/.{2}/g) || []).map((byte) => parseInt(byte, 16))));
      const setName = command.match(/^@r name (\d+) ([0-9a-f]+)$/i);
      const clearName = command.match(/^@r name (\d+) clear$/);
      const getName = command.match(/^@r name (\d+)$/);
      if (command === "@p?") {
        return `OK prompt=${new TextEncoder().encode(window.boardPrompt).length}`;
      }
      const getPrompt = command.match(/^@p get(?: (\d+))?$/);
      if (getPrompt) {
        const offset = Number(getPrompt[1] || 0);
        const bytes = new TextEncoder().encode(window.boardPrompt || "");
        const chunk = bytes.slice(offset, offset + 48);
        const hex = Array.from(chunk, (b) => b.toString(16).padStart(2, "0")).join("");
        const done = offset + chunk.length >= bytes.length;
        return `OK p off=${offset} hex=${hex} total=${bytes.length} done=${done ? "yes" : "no"}`;
      }
      const setPrompt = command.match(/^@p set ([0-9a-f]+)$/i);
      if (setPrompt) {
        const bytes = new Uint8Array((setPrompt[1].match(/.{2}/g) || []).map((b) => parseInt(b, 16)));
        window.boardPrompt = new TextDecoder().decode(bytes);
        return `OK prompt len=${bytes.length}`;
      }
      if (command === "@p clear") {
        window.boardPrompt = "";
    window.boardAiconf = "";
    window.boardAiconfStage = null;
        return "OK prompt cleared";
      }
      if (command === "@y?") {
        return `OK aiconf bytes=${new TextEncoder().encode(window.boardAiconf).length} max=960`;
      }
      if (command === "@y begin") {
        window.boardAiconfStage = "";
        return "OK aiconf begin";
      }
      const putAiconf = command.match(/^@y put ([0-9a-f]+)$/i);
      if (putAiconf) {
        if (window.boardAiconfStage === null) return "ERR aiconf=not-begun";
        window.boardAiconfStage += putAiconf[1];
        return `OK aiconf stage=${window.boardAiconfStage.length / 2}`;
      }
      if (command === "@y end") {
        if (window.boardAiconfStage === null) return "ERR aiconf=not-begun";
        const bytes = new Uint8Array((window.boardAiconfStage.match(/.{2}/g) || []).map((b) => parseInt(b, 16)));
        window.boardAiconf = new TextDecoder().decode(bytes);
        window.boardAiconfStage = null;
        return `OK aiconf bytes=${new TextEncoder().encode(window.boardAiconf).length}`;
      }
      const getAiconf = command.match(/^@y get(?: (\d+))?$/);
      if (getAiconf) {
        if (window.boardAiconfStage !== null) return "ERR aiconf=staging";
        if (!window.boardAiconf) return "ERR aiconf=empty";
        const offset = Number(getAiconf[1] || 0);
        const bytes = new TextEncoder().encode(window.boardAiconf);
        const chunk = bytes.slice(offset, offset + 48);
        const hex = Array.from(chunk, (b) => b.toString(16).padStart(2, "0")).join("");
        const done = offset + chunk.length >= bytes.length;
        return `OK aiconf off=${offset} hex=${hex} total=${bytes.length} done=${done ? "yes" : "no"}`;
      }
      if (command === "@y clear") {
        window.boardAiconf = "";
        window.boardAiconfStage = null;
        return "OK aiconf cleared";
      }
      if (getName) {
        const name = window.boardNames[getName[1]];
        return name
          ? `OK ir name=${getName[1]} hex=${encodeName(name)}`
          : `OK ir name=${getName[1]} none`;
      }
      if (clearName) {
        delete window.boardNames[clearName[1]];
        return `OK ir name=${clearName[1]} cleared`;
      }
      if (setName) {
        window.boardNames[setName[1]] = decodeName(setName[2]);
        return `OK ir name=${setName[1]} set`;
      }
      if (learning) {
        /* Scripted learning session: the capture starts active, the poll sees
         * it active once, then the test flips it to done and the next poll
         * reports a finished frame with one stored slot. */
        if (command === "@r learn") {
          window.boardLearn = "active";
          return "OK ir learn slot=0 timeout_ms=15000";
        }
        if (command === "@r learn stop") {
          window.boardLearn = "done";
          return "OK ir learn slot=0 stored edges=67";
        }
        if (command === "@r?") {
          if (window.boardLearn === "active") {
            return "OK ir rx=on frames=0 repeats=0 glitches=0 last=none learn=active,slot=0,edges=30";
          }
          if (window.boardLearn === "done") {
            return "OK ir rx=on frames=2 repeats=0 glitches=0 " +
              "last=addr=0x0004,cmd=0x08,ext=no learn=idle,slots=1/8";
          }
          return "OK ir rx=on frames=0 repeats=0 glitches=0 last=none learn=idle,slots=1/8";
        }
        if (command === "@r slots") {
          return window.boardLearn === "done"
            ? "OK ir slots=0:67,1:0,2:0,3:0,4:0,5:0,6:0,7:0 used=1"
            : "OK ir slots=0:0,1:0,2:0,3:0,4:0,5:0,6:0,7:0 used=0";
        }
      }
      return "OK";
    });
    setConnected(true);
  }, { replies, learning, names, capabilities });
}

async function openBoardPage(page) {
  await page.locator("#panelToggle").tap();
  await page.locator('[data-settings-target="board"]').tap();
}

async function openAiPage(page) {
  await page.locator("#panelToggle").tap();
  await page.locator('[data-settings-target="ai"]').tap();
}

const NEC_STATUS =
  "OK ir rx=on frames=2 repeats=0 glitches=0 last=addr=0x0004,cmd=0x08,ext=no learn=idle,slots=1/8";
const SLOTS = "OK ir slots=0:67,1:0,2:0,3:0,4:0,5:0,6:0,7:0 used=1";

test("the board section shows the state the board reports", async ({ page }) => {
  await openApp(page);
  await mockBoard(page, { replies: {
    "@l?": "OK led rgb=001830",
    "@o?": "OK opto=open",
    "@r?": NEC_STATUS,
    "@r slots": SLOTS,
  }, names: { "0": "客厅 电源" } });
  await openBoardPage(page);

  await expect(page.locator('[data-settings-page="board"]')).toHaveCount(3);
  await expect(page.locator('[data-settings-page="board"]:visible')).toHaveCount(3);
  await expect(page.locator('#controlsPanel [data-settings-page="connection"]:visible')).toHaveCount(0);

  await expect(page.locator("#boardLedStatus")).toContainText("001830");
  await expect(page.locator("#boardOptoStatus")).toContainText(/Contact open|触点已断开/);
  await expect(page.locator("#boardIrStatus")).toContainText(/Last frame|最近一帧/);
  await expect(page.locator('#boardSlotList .board-slot-row[data-slot="0"]')).toHaveAttribute("data-edges", "67");
  await expect(page.locator('#boardSlotList .board-slot-row[data-slot="0"] .board-slot-name')).toHaveText("客厅 电源");
  await expect(page.locator('#boardSlotList .board-slot-row[data-slot="0"] [data-board-action="play"]')).toBeEnabled();
  await expect(page.locator('#boardSlotList .board-slot-row[data-slot="1"] [data-board-action="play"]')).toBeDisabled();
  expect(await page.evaluate(() => window.boardCommands))
    .toEqual(expect.arrayContaining(["@l?", "@o?", "@r?", "@r slots", "@r name 0"]));
});

test("LED swatches and a custom colour send @l commands", async ({ page }) => {
  await openApp(page);
  await mockBoard(page, { replies: {
    "@l?": "OK led rgb=001830",
    "@l=255,0,0": "OK led rgb=ff0000",
    "@l=0,255,0": "OK led rgb=00ff00",
  } });
  await openBoardPage(page);

  await page.locator('.board-swatch[data-hex="ff0000"]').tap();
  await expect(page.locator("#boardLedStatus")).toContainText("FF0000");
  expect(await page.evaluate(() => window.boardCommands)).toContain("@l=255,0,0");

  await page.locator("#boardLedColor").evaluate((element) => {
    element.value = "#00ff00";
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await page.locator("#boardLedApply").tap();
  await expect(page.locator("#boardLedStatus")).toContainText("00FF00");
  expect(await page.evaluate(() => window.boardCommands)).toContain("@l=0,255,0");
});

test("the optocoupler closes, opens and pulses", async ({ page }) => {
  await openApp(page);
  await mockBoard(page, { replies: {
    "@o?": "OK opto=open",
    "@o on": "OK opto=closed",
    "@o off": "OK opto=open",
    "@o pulse 250": "OK opto=pulse ms=250",
  } });
  await openBoardPage(page);

  await page.locator("#boardOptoClose").tap();
  await expect(page.locator("#boardOptoStatus")).toContainText(/Contact closed|触点已闭合/);
  await page.locator("#boardOptoOpen").tap();
  await expect(page.locator("#boardOptoStatus")).toContainText(/Contact open|触点已断开/);

  await page.locator("#boardOptoPulseMs").fill("250");
  await page.locator("#boardOptoPulse").tap();
  expect(await page.evaluate(() => window.boardCommands)).toEqual(
    expect.arrayContaining(["@o on", "@o off", "@o pulse 250"]),
  );
});

test("a NEC frame is sent with the validated address, command and repeats", async ({ page }) => {
  await openApp(page);
  await mockBoard(page);
  await openBoardPage(page);

  await page.locator("#boardIrAddress").fill("0x0004");
  await page.locator("#boardIrCommand").fill("0x08");
  await page.locator("#boardIrRepeats").fill("2");
  await page.locator("#boardIrSend").tap();
  expect(await page.evaluate(() => window.boardCommands)).toContain("@r tx 0x0004,0x08,2");

  // Invalid input is refused locally, without a management round trip.
  await page.locator("#boardIrAddress").fill("0xnothex");
  await page.locator("#boardIrSend").tap();
  await expect(page.locator("#boardIrStatus")).toHaveClass(/is-error/);
});

test("learning polls the board and stores the frame in the selected slot", async ({ page }) => {
  await openApp(page);
  await mockBoard(page, {
    learning: true,
    replies: {
      "@r play 0": "OK ir play slot=0 edges=67",
      "@r delete 0": "OK ir slot=0 cleared",
    },
  });
  await openBoardPage(page);

  await page.locator("#boardLearnStart").tap();
  await expect(page.locator("#boardLearnStatus")).toContainText(/Learning|学习中/);
  expect(await page.evaluate(() => window.boardCommands)).toContain("@r learn");

  // The poll loop runs every 500 ms; the scripted board finishes the capture.
  await page.evaluate(() => { window.boardLearn = "done"; });
  await expect(page.locator("#boardLearnStatus")).toContainText(/Saved to slot 0|已保存到槽位 0/, { timeout: 10_000 });
  await expect(page.locator('#boardSlotList .board-slot-row[data-slot="0"]')).toHaveAttribute("data-edges", "67");

  // A saved signal is called straight from its row, without the slot picker.
  await page.locator('#boardSlotList .board-slot-row[data-slot="0"] [data-board-action="play"]').tap();
  await page.locator('#boardSlotList .board-slot-row[data-slot="0"] [data-board-action="delete"]').tap();
  expect(await page.evaluate(() => window.boardCommands))
    .toEqual(expect.arrayContaining(["@r play 0", "@r delete 0"]));
});

test("hardware a board does not have is removed, and prompts live on the AI page", async ({ page }) => {
  await openApp(page);
  await mockBoard(page, { replies: {
    "@l?": "ERR led unsupported",
    "@o?": "ERR opto unsupported",
    "@r?": "ERR ir unsupported",
    "@r slots": "ERR ir unsupported",
  } });
  await openBoardPage(page);

  // No board devices -> the whole Board section disappears and falls back.
  await expect(page.locator('[data-settings-target="board"]')).toBeHidden();
  await expect(page.locator('[data-rail-target="board"]')).toBeHidden();
  await expect(page.locator('[data-settings-target="connection"]')).toHaveAttribute("aria-selected", "true");

  // The prompt is stored in NVS, so it stays available on the AI page.
  await page.locator('[data-settings-target="ai"]').tap();
  await expect(page.locator("#boardPromptCard")).toBeVisible();
  await expect(page.locator("#agentModelCard")).toBeVisible();
});

test("an unsupported prompt hides its card on the AI page", async ({ page }) => {
  await openApp(page);
  await mockBoard(page, { replies: { "@p?": "ERR prompt unsupported" } });
  await openAiPage(page);

  await expect(page.locator("#agentModelCard")).toBeVisible();
  await expect(page.locator("#boardPromptCard")).toBeHidden();
});

test("WiFi and WebDAV panels follow the board capabilities", async ({ page }) => {
  await openApp(page);
  // Bits 0 (WiFi) and 1 (WebDAV) are absent on this board.
  await mockBoard(page, { capabilities: (1 << 4) });
  await page.locator("#panelToggle").tap();
  await expect(page.locator('[data-settings-target="network"]')).toBeHidden();
  await expect(page.locator("#wifiWebdavCard")).toBeHidden();
});

test("the desktop rail opens the board section", async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    hasTouch: false,
    isMobile: false,
  });
  const page = await context.newPage();
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      "\nwindow.__test = { state, setConnected, readBoardPrompt, setControl: fn => { sendControl = fn; } };" });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__test?.state.term);
  await page.evaluate(() => {
    const { state, setConnected } = window.__test;
    state.mode = "ble";
    state.mgmtReady = true;
    window.__test.setControl(async (command) => {
      if (command === "@l?") return "OK led rgb=001830";
      if (command === "@o?") return "OK opto=open";
      if (command === "@r?") return "OK ir rx=off frames=0 repeats=0 glitches=0 last=none learn=idle,slots=0/8";
      if (command === "@r slots") return "OK ir slots=0:0,1:0,2:0,3:0,4:0,5:0,6:0,7:0 used=0";
      return "OK";
    });
    setConnected(true);
  });

  await page.locator('[data-rail-target="board"]').click();
  await expect(page.locator('[data-settings-page="board"]:visible')).toHaveCount(3);
  await expect(page.locator('#controlsPanel [data-settings-page="connection"]:visible')).toHaveCount(0);
  await expect(page.locator('[data-rail-target="board"]')).toHaveAttribute("aria-current", "page");
  await context.close();
});

test("the board status texts follow a language switch", async ({ page }) => {
  await openApp(page);
  await mockBoard(page, { replies: {
    "@l?": "OK led rgb=001830",
    "@o?": "OK opto=closed",
    "@r?": NEC_STATUS,
    "@r slots": SLOTS,
  } });
  await openBoardPage(page);
  await expect(page.locator("#boardOptoStatus")).toContainText(/Contact closed|触点已闭合/);

  await page.locator("#drawerLangButton").tap();
  await expect(page.locator("#boardOptoStatus")).toContainText("触点已闭合");
  await expect(page.locator("#boardLedStatus")).toContainText("001830");
  await expect(page.locator('.board-swatch[data-hex="ff0000"]')).toHaveAttribute("title", "红");
});

test("a saved signal is renamed on the board and read back after a reload", async ({ page }) => {
  await openApp(page);
  await mockBoard(page, { replies: { "@r?": NEC_STATUS, "@r slots": SLOTS } });
  await openBoardPage(page);

  const row = page.locator('#boardSlotList .board-slot-row[data-slot="0"]');
  await expect(row.locator(".board-slot-name")).toHaveText(/Slot 0|槽位 0/);
  await row.locator('[data-board-action="rename"]').tap();
  await page.locator(".board-slot-input").fill("客厅 电源");
  await page.locator('[data-board-action="save-name"]').tap();
  await expect(row.locator(".board-slot-name")).toHaveText("客厅 电源");
  await expect(row.locator(".board-slot-meta")).toContainText("67");

  // The name went to the board as UTF-8 hex, not to browser storage.
  const commands = await page.evaluate(() => window.boardCommands);
  expect(commands).toContain("@r name 0 e5aea2e58e8520e794b5e6ba90");
  const stored = await page.evaluate(() => window.boardNames);
  expect(stored["0"]).toBe("客厅 电源");
  expect(await page.evaluate(() => localStorage.getItem("linkr-board-slot-names-v1")))
    .toBeNull();

  // A fresh session reads the names back from the board's NVS.
  await page.reload();
  await page.waitForFunction(() => window.__test?.state.term);
  await mockBoard(page, {
    replies: { "@r?": NEC_STATUS, "@r slots": SLOTS },
    names: { "0": "客厅 电源" },
  });
  await openBoardPage(page);
  await expect(page.locator('#boardSlotList .board-slot-row[data-slot="0"] .board-slot-name'))
    .toHaveText("客厅 电源");
});

test("the merged card neither clips nor misaligns in a narrow drawer", async ({ page }) => {
  await page.setViewportSize({ width: 338, height: 844 });
  await openApp(page);
  await mockBoard(page, { replies: { "@r?": NEC_STATUS, "@r slots": SLOTS } });
  await openBoardPage(page);

  const overflow = await page.evaluate(() => {
    const card = document.querySelector(".board-ir-card");
    return card ? card.scrollWidth - card.clientWidth : -1;
  });
  expect(overflow).toBeLessThanOrEqual(0);
  const select = await page.locator("#boardLearnSlot").boundingBox();
  expect(select.width).toBeGreaterThan(180);
  // The slot description must be visible in full, not ellipsised by a 2-column row.
  const selectText = await page.locator("#boardLearnSlot option").first().textContent();
  expect(selectText).toContain("0");
});

test("the board prompt is saved, read back for the assistant, and cleared", async ({ page }) => {
  await openApp(page);
  await mockBoard(page);
  await openAiPage(page);

  await page.locator("#boardPromptText").fill("请用中文回答");
  await page.locator("#boardPromptSave").tap();
  expect(await page.evaluate(() => window.boardPrompt)).toBe("请用中文回答");
  await expect(page.locator("#boardPromptStatus")).toContainText(/6|字节|bytes/);

  // The runtime reads this value when a conversation starts.
  expect(await page.evaluate(() => window.__test.readBoardPrompt())).toBe("请用中文回答");

  await page.locator("#boardPromptText").fill("");
  await page.locator("#boardPromptLoad").tap();
  await expect(page.locator("#boardPromptText")).toHaveValue("请用中文回答");

  await page.locator("#boardPromptClear").tap();
  expect(await page.evaluate(() => window.boardPrompt)).toBe("");
  await expect(page.locator("#boardPromptStatus")).toContainText(/Empty|空/);
});

test("cards collapse, remember the state, and expand from the keyboard", async ({ page }) => {
  await openApp(page);
  await mockBoard(page, { replies: {
    "@l?": "OK led rgb=001830",
    "@o?": "OK opto=open",
    "@r?": NEC_STATUS,
    "@r slots": SLOTS,
  } });
  await openBoardPage(page);

  const card = page.locator("#boardLedCard");
  const toggle = card.locator(".card-toggle");
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await toggle.tap();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(card.locator("#boardLedSwatches")).toBeHidden();
  await expect(card).toBeVisible();

  // The header band stays; the collapsed state survives a reload.
  await page.reload();
  await page.waitForFunction(() => window.__test?.state.term);
  await mockBoard(page, { replies: { "@l?": "OK led rgb=001830" } });
  await openBoardPage(page);
  await expect(page.locator("#boardLedCard .card-toggle")).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator("#boardLedCard #boardLedSwatches")).toBeHidden();

  // Keyboard toggles too.
  await page.locator("#boardLedCard .card-toggle").focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#boardLedCard .card-toggle")).toHaveAttribute("aria-expanded", "true");
  await expect(page.locator("#boardLedCard #boardLedSwatches")).toBeVisible();
});

test("the AI configuration can be saved to and loaded from the board", async ({ page }) => {
  await openApp(page);
  await mockBoard(page);
  await page.locator("#panelToggle").tap();
  await page.locator('[data-settings-target="ai"]').tap();

  await page.locator("#agentEndpoint").fill("https://api.example.com/v1");
  await page.locator("#agentModel").fill("demo-model");
  await page.locator("#agentApiKey").fill("sk-test-123");
  await page.locator("#agentBoardConfigSecrets").check();
  await page.locator("#agentBoardConfigSave").tap();
  await expect(page.locator("#agentBoardConfigStatus")).toContainText(/Saved to the board|已保存到板载/);
  const blob = JSON.parse(await page.evaluate(() => window.boardAiconf));
  expect(blob.endpoint).toBe("https://api.example.com/v1");
  expect(blob.model).toBe("demo-model");
  expect(blob.apiKey).toBe("sk-test-123");

  await page.locator("#agentEndpoint").fill("");
  await page.locator("#agentModel").fill("");
  await page.locator("#agentApiKey").fill("");
  await page.locator("#agentBoardConfigLoad").tap();
  await expect(page.locator("#agentEndpoint")).toHaveValue("https://api.example.com/v1");
  await expect(page.locator("#agentModel")).toHaveValue("demo-model");
  await expect(page.locator("#agentApiKey")).toHaveValue("sk-test-123");

  await page.locator("#agentBoardConfigClear").tap();
  expect(await page.evaluate(() => window.boardAiconf)).toBe("");
  await expect(page.locator("#agentBoardConfigStatus")).toContainText(/cleared|已清除/);
});

test("the rail still shows one settings section at a time", async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    hasTouch: false,
    isMobile: false,
  });
  const page = await context.newPage();
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      "\nwindow.__test = { state, setConnected, setControl: fn => { sendControl = fn; } };" });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__test?.state.term);
  await page.evaluate(() => {
    const { state, setConnected } = window.__test;
    state.mode = "ble";
    state.mgmtReady = true;
    window.__test.setControl(async () => "OK");
    setConnected(true);
  });
  await page.locator("#panelToggle").click();

  // Each rail entry filters the panel to exactly its own section.
  await page.locator('[data-rail-target="board"]').click();
  await expect(page.locator('[data-settings-page="board"]:visible')).toHaveCount(3);
  await expect(page.locator('#controlsPanel [data-settings-page="ai"]:visible')).toHaveCount(0);
  await expect(page.locator('#controlsPanel [data-settings-page="connection"]:visible')).toHaveCount(0);

  await page.locator('[data-rail-target="ai"]').click();
  await expect(page.locator('#controlsPanel [data-settings-page="ai"]:visible')).toHaveCount(1);
  await expect(page.locator('#controlsPanel [data-settings-page="board"]:visible')).toHaveCount(0);
  await expect(page.locator("#agentModelCard")).toBeVisible();
  await context.close();
});

test("the AI configuration saves itself when a field loses focus", async ({ page }) => {
  await openApp(page);
  await mockBoard(page);
  await openAiPage(page);

  await page.locator("#agentEndpoint").fill("https://api.example.com/v1");
  await page.locator("#agentModel").fill("auto-model");
  await page.locator("#agentApiKey").fill("sk-auto");
  await page.locator("#agentApiKey").press("Tab");

  await expect(page.locator("#agentSettingsStatus")).toContainText(/saved on this device|配置已保存/);
  const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("linkr-agent-model")));
  expect(stored.endpoint).toBe("https://api.example.com/v1");
  expect(stored.model).toBe("auto-model");
  expect(stored.apiKey).toBe("sk-auto");
});


test("board configuration operations stay exclusive and recover after failure", async ({ page }) => {
  await openApp(page);
  await mockBoard(page);
  await page.locator("#panelToggle").tap();
  await page.locator('[data-settings-target="ai"]').tap();
  await page.locator("#agentEndpoint").fill("https://api.example.com/v1");
  await page.locator("#agentModel").fill("demo-model");
  await page.evaluate(() => {
    window.boardScript["@y begin"] = () => {
      window.boardAiconfStage = "";
      return new Promise(resolve => { window.releaseBoardSave = () => resolve("OK aiconf begin"); });
    };
  });
  await page.locator("#agentBoardConfigSave").tap();
  for (const id of ["agentBoardConfigSave", "agentBoardConfigLoad", "agentBoardConfigClear"]) {
    await expect(page.locator(`#${id}`)).toBeDisabled();
  }
  await page.evaluate(() => {
    for (const id of ["agentBoardConfigSave", "agentBoardConfigLoad", "agentBoardConfigClear"]) {
      document.getElementById(id).dispatchEvent(new Event("click"));
    }
    window.releaseBoardSave();
  });
  await expect(page.locator("#agentBoardConfigSave")).toBeEnabled();
  expect(await page.evaluate(() => window.boardCommands.filter(c => c === "@y begin").length)).toBe(1);
  expect(await page.evaluate(() => window.boardCommands.includes("@y clear"))).toBe(false);
  expect(JSON.parse(await page.evaluate(() => window.boardAiconf)).model).toBe("demo-model");
  await page.evaluate(() => { window.boardScript["@y begin"] = "ERR storage"; });
  await page.locator("#agentBoardConfigSave").tap();
  await expect(page.locator("#agentBoardConfigStatus")).toContainText("ERR storage");
  for (const id of ["agentBoardConfigSave", "agentBoardConfigLoad", "agentBoardConfigClear"]) {
    await expect(page.locator(`#${id}`)).toBeEnabled();
  }
  await page.evaluate(() => { delete window.boardScript["@y begin"]; });
  await page.locator("#agentBoardConfigSave").tap();
  await expect(page.locator("#agentBoardConfigStatus")).toContainText(/Saved to the board|已保存到板载/);
});
