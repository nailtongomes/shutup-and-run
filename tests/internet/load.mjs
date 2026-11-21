// N browsers at once through one PC: each opens the same few sites, a few
// rounds, through the relay's proxy, while the relay's /status is read every
// half second. It prints the pages that loaded, how long they took, and the
// most streams the PC carried at once. Exit 1 when a page failed.
//
//   node tests/internet/load.mjs http://ID:PASSWORD@127.0.0.1:18080 [browsers] [rounds]
//
// Needs node with Playwright and its Chromium. tests/internet/local.sh runs
// it with SHUTUP_AND_RUN_LOAD=1. The numbers are in docs/INTERNET.md, "How
// many at once".
import { createRequire } from "node:module";
import http from "node:http";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const proxyUrl = new URL(process.argv[2] ?? "");
const browsers = Number(process.argv[3] ?? 10);
const rounds = Number(process.argv[4] ?? 3);
const sites = [
  "https://www.wikipedia.org/",
  "https://github.com/",
  "https://www.debian.org/",
  "https://www.iana.org/",
  "https://www.python.org/",
];
const user = decodeURIComponent(proxyUrl.username);
const pass = decodeURIComponent(proxyUrl.password);

let peak = 0;
const watch = setInterval(() => {
  const req = http.get(
    { host: proxyUrl.hostname, port: proxyUrl.port, path: "/status", auth: `${user}:${pass}` },
    (res) => {
      let body = "";
      res.on("data", (d) => (body += d));
      res.on("end", () => {
        try {
          peak = Math.max(peak, JSON.parse(body).open ?? 0);
        } catch {}
      });
    },
  );
  req.on("error", () => {});
}, 500);

const times = [];
const failed = [];
async function one(n) {
  const browser = await chromium.launch({
    proxy: { server: `http://${proxyUrl.host}`, username: user, password: pass },
  });
  try {
    const page = await browser.newPage();
    for (let round = 0; round < rounds; round++) {
      for (const site of sites) {
        const began = Date.now();
        try {
          const res = await page.goto(site, { waitUntil: "load", timeout: 60_000 });
          if (!res || res.status() >= 400) throw new Error(`HTTP ${res?.status()}`);
          times.push(Date.now() - began);
        } catch (e) {
          failed.push(`browser ${n} ${site}: ${String(e.message).split("\n")[0]}`);
        }
      }
    }
  } finally {
    await browser.close();
  }
}

const began = Date.now();
await Promise.all(Array.from({ length: browsers }, (_, n) => one(n + 1)));
clearInterval(watch);
times.sort((a, b) => a - b);
const at = (q) => times[Math.min(times.length - 1, Math.floor(q * times.length))] ?? 0;
console.log(
  `${browsers} browsers x ${rounds} rounds x ${sites.length} sites: ` +
    `${times.length} pages loaded, ${failed.length} failed, in ${Math.round((Date.now() - began) / 1000)} s`,
);
console.log(`page load: median ${at(0.5)} ms, 95% under ${at(0.95)} ms`);
console.log(`streams open at once through the PC, at most: ${peak}`);
for (const line of failed) console.log(`FAIL ${line}`);
process.exit(failed.length ? 1 : 0);
