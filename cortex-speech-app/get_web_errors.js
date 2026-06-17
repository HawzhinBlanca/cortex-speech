import { chromium } from 'playwright';
import fs from 'fs';
import path from 'path';

function screenshotPath() {
  const target = process.env.CORTEX_SCREENSHOT_PATH || path.join(process.cwd(), 'reports', 'screenshot_now.png');
  fs.mkdirSync(path.dirname(target), { recursive: true });
  return target;
}

async function main() {
  console.log('Launching Playwright Chromium...');
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext();
  const page = await context.newPage();

  // Capture console messages
  page.on('console', msg => {
    console.log(`[Browser Console] [${msg.type()}]: ${msg.text()}`);
  });

  // Capture page errors
  page.on('pageerror', err => {
    console.log(`[Browser Uncaught Error]: ${err.message}`);
    if (err.stack) console.log(err.stack);
  });

  console.log('Navigating to http://localhost:1420/ ...');
  try {
    await page.goto('http://localhost:1420/', { waitUntil: 'networkidle', timeout: 10000 });
  } catch (err) {
    console.error('Failed to load page:', err.message);
  }

  console.log('Waiting 3 seconds for Svelte initialization...');
  await page.waitForTimeout(3000);

  // Take a screenshot
  const targetScreenshot = screenshotPath();
  console.log('Taking screenshot and saving to:', targetScreenshot);
  await page.screenshot({ path: targetScreenshot });

  console.log('Closing browser...');
  await browser.close();
}

main().catch(console.error);
