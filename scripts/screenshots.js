// Used by scripts/screenshots.sh: node screenshots.js BASE_URL WORK_DIR OUT_DIR
const { chromium } = require('playwright');

const [base, work, out] = process.argv.slice(2);

(async () => {
  const browser = await chromium.launch();
  const shot = async (lang, name, path, viewport, scheme, options) => {
    const context = await browser.newContext({
      viewport,
      colorScheme: scheme,
      deviceScaleFactor: 1,
      httpCredentials: { username: 'admin', password: 'demo-password' },
    });
    await context.addCookies([{ name: 'lang', value: lang, url: base }]);
    const page = await context.newPage();
    await page.goto(path.startsWith('file://') ? path : base + path);
    await page.screenshot({ path: `${out}/${lang}-${name}.png`, ...options });
    await context.close();
  };
  const desk = { width: 1100, height: 900 };
  for (const lang of ['en', 'pt']) {
    // The top of the status page: the state, the facts, the last jobs.
    await shot(lang, 'status', '/', desk, 'light', { fullPage: true, clip: { x: 0, y: 0, width: 1100, height: 870 } });
    await shot(lang, 'jobs', '/local/logs', { width: 1100, height: 520 }, 'light', {});
    await shot(lang, 'advanced', '/local/advanced', desk, 'light', {});
    await shot(lang, 'dark', '/', { width: 1100, height: 760 }, 'dark', {});
    await shot(lang, 'phone', '/', { width: 390, height: 844 }, 'light', {});
    await shot(lang, 'authorization', `file://${work}/auth-${lang}.html`, { width: 1100, height: 800 }, 'light',
      { clip: { x: 0, y: 0, width: 1100, height: 540 } });
  }
  await browser.close();
})().catch((err) => {
  console.error(err);
  process.exit(1);
});
