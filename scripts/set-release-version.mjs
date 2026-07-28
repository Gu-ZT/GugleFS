import fs from "node:fs";

const tag = process.argv[2];
if (!tag) {
  throw new Error("release tag argument is required");
}

const version = tag.startsWith("v") ? tag.slice(1) : tag;
const path = "src-tauri/tauri.conf.json";
const config = JSON.parse(fs.readFileSync(path, "utf8"));
config.version = version;
fs.writeFileSync(path, `${JSON.stringify(config, null, 2)}\n`);
