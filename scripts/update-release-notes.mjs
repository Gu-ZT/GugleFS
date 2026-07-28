import fs from "node:fs";
import { pathToFileURL } from "node:url";

const ASSET_RULES = [
  [/_x64-setup\.exe$/, "Windows", "x86_64", "NSIS installer"],
  [/_x86-setup\.exe$/, "Windows", "x86 (32-bit)", "NSIS installer"],
  [/_arm64-setup\.exe$/, "Windows", "arm64", "NSIS installer"],
  [/_x64[^/]*\.msi$/, "Windows", "x86_64", "MSI installer"],
  [/_x86[^/]*\.msi$/, "Windows", "x86 (32-bit)", "MSI installer"],
  [/_arm64[^/]*\.msi$/, "Windows", "arm64", "MSI installer"],
  [/_amd64\.deb$/, "Linux", "x86_64", "DEB package"],
  [/_arm64\.deb$/, "Linux", "arm64", "DEB package"],
  [/\.x86_64\.rpm$/, "Linux", "x86_64", "RPM package"],
  [/\.aarch64\.rpm$/, "Linux", "arm64", "RPM package"],
  [/_amd64\.AppImage$/, "Linux", "x86_64", "AppImage"],
  [/_aarch64\.AppImage$/, "Linux", "arm64", "AppImage"],
  [/_x64\.dmg$/, "macOS", "Intel (x86_64)", "DMG"],
  [/_aarch64\.dmg$/, "macOS", "Apple Silicon (arm64)", "DMG"],
  [/_x64\.app\.tar\.gz$/, "macOS", "Intel (x86_64)", "App archive"],
  [/_aarch64\.app\.tar\.gz$/, "macOS", "Apple Silicon (arm64)", "App archive"],
];

export function releaseVersion(tag) {
  return tag.replace(/^v/, "").replace(/\+build\.\d+$/, "");
}

export function extractVersionNotes(file, version) {
  const lines = fs.readFileSync(file, "utf8").replace(/\r\n/g, "\n").split("\n");
  const heading = `## [${version}]`;
  const start = lines.findIndex((line) => line.startsWith(heading));
  if (start === -1) {
    throw new Error(`${file} does not contain a ${heading} section`);
  }
  const end = lines.findIndex(
    (line, index) => index > start && line.startsWith("## ["),
  );
  return lines.slice(start + 1, end === -1 ? lines.length : end).join("\n").trim();
}

export function classifyAsset(name) {
  const signature = name.endsWith(".sig");
  const baseName = signature ? name.slice(0, -4) : name;
  const index = ASSET_RULES.findIndex(([pattern]) => pattern.test(baseName));
  if (index === -1) {
    return {
      order: ASSET_RULES.length,
      platform: "Other",
      architecture: "-",
      format: signature ? "Signature" : "File",
    };
  }
  const [, platform, architecture, format] = ASSET_RULES[index];
  return {
    order: index,
    platform,
    architecture,
    format: signature ? `${format} signature` : format,
  };
}

function escapeCell(value) {
  return String(value).replaceAll("|", "\\|");
}

export function buildReleaseBody({ release, tag, previousTag, repository }) {
  const version = releaseVersion(tag);
  const englishNotes = extractVersionNotes("CHANGES.md", version);
  const chineseNotes = extractVersionNotes("CHANGES.zh_CN.md", version);
  const rows = (release.assets ?? [])
    .map((asset) => ({ ...classifyAsset(asset.name), asset }))
    .sort(
      (left, right) =>
        left.order - right.order || left.asset.name.localeCompare(right.asset.name),
    )
    .map(
      ({ platform, architecture, format, asset }) =>
        `| ${platform} | ${architecture} | ${format} | [${escapeCell(asset.name)}](${asset.browser_download_url}) |`,
    );
  if (rows.length === 0) {
    rows.push("| - | - | - | No build artifacts were uploaded / 暂无构建产物 |");
  }

  const comparison = previousTag
    ? `**Full Changelog / 完整变更**: https://github.com/${repository}/compare/${previousTag}...${tag}`
    : "";
  const table = [
    "| Platform / 平台 | Architecture / 架构 | Format / 格式 | File / 文件 |",
    "| --- | --- | --- | --- |",
    ...rows,
  ].join("\n");
  return [
    "## What's Changed",
    englishNotes,
    "## 更新内容",
    chineseNotes,
    comparison,
    "## Downloads / 下载",
    table,
  ]
    .filter(Boolean)
    .join("\n\n")
    .concat("\n");
}

function main() {
  const [releasePath, tag, previousTag = "", outputPath] = process.argv.slice(2);
  if (!releasePath || !tag || !outputPath) {
    throw new Error(
      "usage: node scripts/update-release-notes.mjs <release.json> <tag> <previous-tag> <output.md>",
    );
  }
  const release = JSON.parse(fs.readFileSync(releasePath, "utf8"));
  const repository = process.env.GITHUB_REPOSITORY;
  if (!repository) {
    throw new Error("GITHUB_REPOSITORY is required");
  }
  const body = buildReleaseBody({ release, tag, previousTag, repository });
  fs.writeFileSync(outputPath, body);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
