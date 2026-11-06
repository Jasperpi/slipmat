// Headless screenshots of the running app: node scripts/shot.mjs out.png [load-query]
import { createRequire } from 'module';
const require = createRequire(process.env.PW_FROM || '/home/jaspermeller/DPHS-Leadership/package.json');
const { chromium } = require('playwright');
const [out = 'shot.png', ...steps] = process.argv.slice(2);
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 });
const errors = [];
page.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') errors.push(m.text()); });
page.on('pageerror', (e) => errors.push('PAGE: ' + e.message));
await page.goto('http://127.0.0.1:4141/');
await page.waitForTimeout(2500);
for (const step of steps) {
  const [cmd, ...rest] = step.split(':');
  const arg = rest.join(':');
  if (cmd === 'wait') await page.waitForTimeout(+arg);
  else if (cmd === 'js') await page.evaluate(arg);
  else if (cmd === 'click') await page.click(arg);
}
await page.screenshot({ path: out });
console.log(errors.length ? 'console:\n' + errors.join('\n') : 'no console errors');
await browser.close();
