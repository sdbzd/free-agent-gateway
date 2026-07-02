import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = resolve(root, "src");
const output = resolve(root, "build");

const [html, css, app, usage] = await Promise.all([
  readFile(resolve(source, "index.html"), "utf8"),
  readFile(resolve(source, "styles.css"), "utf8"),
  readFile(resolve(source, "app.js"), "utf8"),
  readFile(resolve(source, "usage.js"), "utf8"),
]);

const bundledJs = app
  .replace(/import\s*\{[\s\S]*?\}\s*from\s*"\.\/usage\.js";/, usage.replaceAll("export ", ""))
  .replaceAll(' from "./usage.js"', "");

const document = html
  .replace('<link rel="stylesheet" href="./styles.css" />', `<style>\n${css}\n</style>`)
  .replace('<script type="module" src="./app.js"></script>', `<script type="module">\n${bundledJs}\n</script>`);

await mkdir(output, { recursive: true });
await writeFile(resolve(output, "index.html"), document, "utf8");
console.log(`built ${resolve(output, "index.html")}`);
