import { chromium } from 'playwright';

async function main() {
  const wsUrl = process.env.AGY_BROWSER_WS_URL;
  if (!wsUrl) {
    console.error('AGY_BROWSER_WS_URL environment variable is not set!');
    process.exit(1);
  }
  console.log('Connecting to:', wsUrl);
  const browser = await chromium.connectOverCDP(wsUrl);
  const contexts = browser.contexts();
  console.log('Contexts count:', contexts.length);
  for (const context of contexts) {
    const pages = context.pages();
    console.log('Pages count:', pages.length);
    for (const page of pages) {
      console.log('--- Page ---');
      console.log('Title:', await page.title());
      console.log('URL:', page.url());
      
      // Listen for console messages, especially errors
      page.on('console', msg => {
        console.log(`Console [${msg.type()}]:`, msg.text());
      });
      
      try {
        const text = await page.innerText('body');
        console.log('Body Text:\n', text);
      } catch (e) {
        console.error('Error reading body text:', e.message);
      }
      
      try {
        const html = await page.content();
        console.log('HTML length:', html.length);
      } catch (e) {
        console.error('Error reading HTML:', e.message);
      }
    }
  }
  // Do not call browser.close() to keep the user's browser open.
}

main().catch(console.error);
