import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  buildReleaseBody,
  classifyAsset,
  extractVersionNotes,
  releaseVersion,
} from "./update-release-notes.mjs";

test("normalizes preview and stable release tags", () => {
  assert.equal(releaseVersion("0.8.0+build.15"), "0.8.0");
  assert.equal(releaseVersion("v0.8.0"), "0.8.0");
});

test("extracts exactly one changelog section", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "guglefs-release-notes-"));
  const file = path.join(directory, "CHANGES.md");
  fs.writeFileSync(file, "# Changes\n\n## [0.8.0]\n\n- New\n\n## [0.7.0]\n\n- Old\n");
  assert.equal(extractVersionNotes(file, "0.8.0"), "- New");
  fs.rmSync(directory, { recursive: true, force: true });
});

test("classifies current desktop artifacts and signatures", () => {
  assert.deepEqual(classifyAsset("GugleFS_0.8.0_x64-setup.exe"), {
    order: 0,
    platform: "Windows",
    architecture: "x86_64",
    format: "NSIS installer",
  });
  assert.equal(classifyAsset("GugleFS_0.8.0_aarch64.dmg.sig").format, "DMG signature");
  assert.equal(classifyAsset("checksums.txt").platform, "Other");
});

test("renders bilingual notes and a sorted asset table", () => {
  const originalRead = fs.readFileSync;
  fs.readFileSync = (file, encoding) => {
    if (file === "CHANGES.md") return "## [0.8.0]\n\n- English\n";
    if (file === "CHANGES.zh_CN.md") return "## [0.8.0]\n\n- 中文\n";
    return originalRead(file, encoding);
  };
  try {
    const body = buildReleaseBody({
      release: {
        assets: [
          { name: "GugleFS_0.8.0_aarch64.dmg", browser_download_url: "https://d/mac" },
          { name: "GugleFS_0.8.0_x64-setup.exe", browser_download_url: "https://d/win" },
        ],
      },
      tag: "0.8.0+build.15",
      previousTag: "0.7.0+build.14",
      repository: "Gu-ZT/GugleFS",
    });
    assert.match(body, /## What's Changed\n\n- English/);
    assert.match(body, /## 更新内容\n\n- 中文/);
    assert.ok(body.indexOf("x64-setup.exe") < body.indexOf("aarch64.dmg"));
    assert.match(body, /compare\/0\.7\.0\+build\.14\.\.\.0\.8\.0\+build\.15/);
  } finally {
    fs.readFileSync = originalRead;
  }
});
