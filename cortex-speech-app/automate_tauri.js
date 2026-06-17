import fs from 'fs';
import path from 'path';
import { chromium } from 'playwright';

function screenshotPath() {
  const target = process.env.CORTEX_SCREENSHOT_PATH || path.join(process.cwd(), 'reports', 'screenshot_now.png');
  fs.mkdirSync(path.dirname(target), { recursive: true });
  return target;
}

async function main() {
  const wsUrl = 'http://127.0.0.1:9222';
  console.log('Connecting to WebView2 debugging port:', wsUrl);
  
  let browser;
  try {
    browser = await chromium.connectOverCDP(wsUrl);
  } catch (err) {
    console.error('Failed to connect to CDP endpoint. Make sure the port is active!', err.message);
    process.exit(1);
  }

  const contexts = browser.contexts();
  console.log('Contexts count:', contexts.length);
  
  let page;
  for (const context of contexts) {
    const pages = context.pages();
    console.log('Pages count in context:', pages.length);
    for (const p of pages) {
      const url = p.url();
      console.log('Checking page URL:', url);
      if (!url.startsWith('devtools://')) {
        page = p;
        break;
      }
    }
    if (page) break;
  }

  if (!page) {
    console.error('No page found in browser contexts!');
    process.exit(1);
  }

  console.log('Successfully connected to Tauri WebView!');
  console.log('Page Title:', await page.title());
  console.log('Page URL:', page.url());

  // Capture console messages
  page.on('console', msg => {
    console.log(`[Browser Console] [${msg.type()}]: ${msg.text()}`);
  });

  // Capture page errors
  page.on('pageerror', err => {
    console.log(`[Browser Uncaught Error]: ${err.message}`);
    if (err.stack) console.log(err.stack);
  });

  console.log('Reloading page to capture all console messages...');
  await page.reload().catch(err => console.log('Reload error:', err.message));

  // Let's find the segment list buttons. 
  // Selector: aside[data-testid="left-panel"] [role="list"] button
  const segmentSelector = 'aside[data-testid="left-panel"] [role="list"] button';
  
  // Wait for segments to load using robust polling
  console.log('Waiting for segments to load in UI...');
  let segmentCount = 0;
  let segments;
  for (let attempts = 0; attempts < 30; attempts++) {
    segments = page.locator(segmentSelector);
    segmentCount = await segments.count();
    if (segmentCount > 0) {
      break;
    }
    await page.waitForTimeout(1000);
  }
  console.log(`Found ${segmentCount} segments in the sidebar list.`);

  // Loop through first 5 segments
  const limit = Math.min(5, segmentCount);
  if (limit === 0) {
    console.log('Error: No segments found. Make sure audio/datasets are loaded in the application.');
    await browser.close();
    return;
  }

  for (let i = 0; i < limit; i++) {
    console.log(`\n--- Playing Segment ${i + 1}/${limit} ---`);
    const segment = segments.nth(i);
    
    // Click segment to select
    console.log('Clicking segment in UI...');
    await segment.click();
    await page.waitForTimeout(2000);

    // Get current transcript
    const rawTranscript = await page.locator('textarea#raw-ts').inputValue({ timeout: 2000 }).catch(() => '');
    const normTranscript = await page.locator('textarea#norm-ts').inputValue({ timeout: 2000 }).catch(() => '');
    console.log('Raw Transcript :', rawTranscript);
    console.log('Norm Transcript:', normTranscript);

    // Inspect AudioPlayer state
    console.log('Audio Player HTML:', await page.locator('[aria-label="Audio player controls"]').innerHTML().catch(() => 'Not found'));
    const audioSrc = await page.locator('audio').getAttribute('src').catch(() => 'Not found');
    console.log('Audio element src:', audioSrc);

    // Wait for play button to appear (wait for loading state to end)
    const playBtn = page.locator('button[aria-label="Play"]');
    let playBtnFound = false;
    for (let attempts = 0; attempts < 10; attempts++) {
      if (await playBtn.count() > 0) {
        playBtnFound = true;
        break;
      }
      await page.waitForTimeout(500);
    }

    if (playBtnFound) {
      console.log('Clicking PLAY button in UI...');
      await playBtn.click();
      
      // Let it play for 2 seconds
      console.log('Listening (playing for 2 seconds)...');
      await page.waitForTimeout(2000);
      
      // Take a screenshot of the UI playing/processing audio
      const targetScreenshot = screenshotPath();
      console.log('Taking screenshot and saving to:', targetScreenshot);
      await page.screenshot({ path: targetScreenshot });

      // Pause it
      const pauseBtn = page.locator('button[aria-label="Pause"]');
      if (await pauseBtn.count() > 0) {
        console.log('Clicking PAUSE button in UI...');
        await pauseBtn.click();
      }
    } else {
      console.log('AudioPlayer Play button not found (loading/error state active).');
      // Take a screenshot to see what error or loading state is displayed
      const targetScreenshot = screenshotPath();
      console.log('Taking error state screenshot and saving to:', targetScreenshot);
      await page.screenshot({ path: targetScreenshot });
    }
  }

  console.log('\nTauri Web UI automation completed successfully!');
  await browser.close();
}

main().catch(console.error);
