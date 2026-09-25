import { readFile, writeFile } from "node:fs/promises";

const [version, url, signaturePath, outputPath = "latest.json"] = process.argv.slice(2);
if (!version || !url || !signaturePath) {
  console.error(
    "Usage: node scripts/write-updater-manifest.mjs <version> <archive-url> <signature-file> [output-file]",
  );
  process.exit(2);
}

const signature = (await readFile(signaturePath, "utf8")).trim();
if (!signature) throw new Error(`Updater signature is empty: ${signaturePath}`);

const manifest = {
  version,
  notes: process.env.RELEASE_NOTES ?? "",
  pub_date: new Date().toISOString(),
  platforms: {
    "darwin-aarch64": { url, signature },
  },
};

await writeFile(outputPath, `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`Wrote ${outputPath} for ${version} (darwin-aarch64).`);
