#!/usr/bin/env node
/**
 * OCR 运行时库下载与校验脚本（ONNX Runtime + pdfium）
 *
 * 用法：node app/scripts/download-runtimes.mjs [--platform <darwin-arm64|win-x64>]
 *
 * 行为：
 *  - 按平台（默认当前平台）下载 ONNX Runtime 与 pdfium 官方归档到临时目录
 *  - 校验归档 sha256（常量在本文件维护，升级版本时同步更新）
 *  - 解出动态库放到 app/src-tauri/resources/{onnxruntime,pdfium}/
 *  - 已存在且与归档内文件一致（按哈希比对）则跳过下载
 *
 * Windows x64 映射为 CI（.github/workflows/tauri-build.yml）预留；
 * 本机（macOS）执行只处理 darwin-arm64。
 * 仅依赖 Node >= 22 内置能力与系统 tar（macOS/Linux/Windows runner 均有 bsdtar）。
 */
import { createHash } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { mkdir, mkdtemp, stat, unlink, rename, readFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import os from 'node:os';
import path from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const RESOURCES = path.join(__dirname, '..', 'src-tauri', 'resources');

// ---- 版本与哈希常量（升级时同步更新；哈希为发布归档的 sha256） ----
const ORT_VERSION = '1.28.0';
const PDFIUM_TAG = 'chromium/8035'; // bblanchon/pdfium-binaries（PDFium 154.0.8035.0）

// 备用下载源：本仓库 Release 自托管的官方归档副本（sha256 与官方一致，校验逻辑不变）。
// 官方源（github.com/microsoft/onnxruntime）在部分网络与 GitHub Actions 出口下会 404。
const MIRROR_BASE =
  'https://github.com/Shinkai-C1/QueryDoc/releases/download/resources-v1';

const RUNTIMES = {
  'darwin-arm64': {
    ort: {
      file: `onnxruntime-osx-arm64-${ORT_VERSION}.tgz`,
      urls: [
        `https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-osx-arm64-${ORT_VERSION}.tgz`,
        `${MIRROR_BASE}/onnxruntime-osx-arm64-${ORT_VERSION}.tgz`,
      ],
      sha256: '1268b359718099bde2cedb55787f182a130067bc4f31e8c88478c445b850d3d8',
      archiveMember: `onnxruntime-osx-arm64-${ORT_VERSION}/lib/libonnxruntime.${ORT_VERSION}.dylib`,
      dest: path.join(RESOURCES, 'onnxruntime', `libonnxruntime.${ORT_VERSION}.dylib`),
    },
    pdfium: {
      file: 'pdfium-mac-arm64.tgz',
      urls: [
        `https://github.com/bblanchon/pdfium-binaries/releases/download/${PDFIUM_TAG.replace('/', '%2F')}/pdfium-mac-arm64.tgz`,
        `${MIRROR_BASE}/pdfium-mac-arm64.tgz`,
      ],
      sha256: '308fd9c6eff1be5b7bde62e7a9a42f525075901314a2a50058ae0b6ea0ff30a2',
      archiveMember: 'lib/libpdfium.dylib',
      dest: path.join(RESOURCES, 'pdfium', 'libpdfium.dylib'),
    },
  },
  'win-x64': {
    ort: {
      file: `onnxruntime-win-x64-${ORT_VERSION}.zip`,
      urls: [
        `https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-win-x64-${ORT_VERSION}.zip`,
        `${MIRROR_BASE}/onnxruntime-win-x64-${ORT_VERSION}.zip`,
      ],
      sha256: 'abef733dacbe2f571547a7150b479b5cb9cc0df22f96c24983a42cadb1b4f8bc',
      archiveMember: `onnxruntime-win-x64-${ORT_VERSION}/lib/onnxruntime.dll`,
      dest: path.join(RESOURCES, 'onnxruntime', 'onnxruntime.dll'),
    },
    pdfium: {
      file: 'pdfium-win-x64.tgz',
      urls: [
        `https://github.com/bblanchon/pdfium-binaries/releases/download/${PDFIUM_TAG.replace('/', '%2F')}/pdfium-win-x64.tgz`,
        `${MIRROR_BASE}/pdfium-win-x64.tgz`,
      ],
      sha256: '61513d611ad200a383456140739be77d156f1e3a2eef22bd89f6c3bda79bdd41',
      archiveMember: 'bin/pdfium.dll',
      dest: path.join(RESOURCES, 'pdfium', 'pdfium.dll'),
    },
  },
};

function currentPlatform() {
  if (process.platform === 'darwin' && process.arch === 'arm64') return 'darwin-arm64';
  if (process.platform === 'win32' && process.arch === 'x64') return 'win-x64';
  throw new Error(`未支持的平台: ${process.platform}-${process.arch}（可 --platform 显式指定）`);
}

async function sha256Of(file) {
  const hash = createHash('sha256');
  const { createReadStream } = await import('node:fs');
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}

async function download(url, dest) {
  // 用系统 curl：遵循 http_proxy/https_proxy 环境变量（Node fetch 不读），
  // macOS / Linux / Windows runner 均自带；失败即非零退出
  execFileSync('curl', ['-fSL', '--retry', '3', '--connect-timeout', '30', '-o', dest, url], {
    stdio: 'inherit',
  });
}

/** 从归档解出单个成员（tgz 与 zip 均走系统 tar/bsdtar）。 */
function extractMember(archive, member, outDir) {
  execFileSync('tar', ['-xf', archive, '-C', outDir, member], { stdio: 'inherit' });
  return path.join(outDir, member);
}

async function ensureRuntime(name, spec, tmpDir) {
  // 已存在且来源标记（归档哈希）一致则跳过
  try {
    await stat(spec.dest);
    const marker = `${spec.dest}.sha256`;
    try {
      const recorded = (await readFile(marker, 'utf8')).trim();
      if (recorded === spec.sha256) {
        console.log(`[runtimes] ${name} 已就绪（归档哈希匹配），跳过`);
        return;
      }
      console.warn(`[runtimes] ${name} 版本/来源不同，重新下载`);
    } catch {
      console.warn(`[runtimes] ${name} 缺少来源标记，重新下载并校验`);
    }
  } catch { /* 不存在则下载 */ }

  // 先下载并校验，成功后才替换目标（失败不破坏现有安装）；
  // 按顺序尝试官方源与备用源，任一成功即可
  const archive = path.join(tmpDir, spec.file);
  let downloaded = false;
  for (const url of spec.urls) {
    console.log(`[runtimes] 下载 ${name} <- ${url}`);
    try {
      await download(url, archive);
      downloaded = true;
      break;
    } catch (e) {
      console.warn(`[runtimes] 该源失败，尝试下一个（${e.message.split('\n')[0]}）`);
    }
  }
  if (!downloaded) throw new Error(`${name} 所有下载源均失败`);
  const actual = await sha256Of(archive);
  if (actual !== spec.sha256) {
    throw new Error(`${name} 归档 sha256 校验失败: 期望 ${spec.sha256} 实际 ${actual}`);
  }
  const extracted = extractMember(archive, spec.archiveMember, tmpDir);
  await mkdir(path.dirname(spec.dest), { recursive: true });
  await rename(extracted, spec.dest);
  // 记录来源归档哈希作为安装标记
  const { writeFile } = await import('node:fs/promises');
  await writeFile(`${spec.dest}.sha256`, spec.sha256 + '\n');
  console.log(`[runtimes] 完成 ${name} -> ${spec.dest}`);
}

async function main() {
  const argv = process.argv.slice(2);
  const idx = argv.indexOf('--platform');
  const platform = idx >= 0 ? argv[idx + 1] : currentPlatform();
  const spec = RUNTIMES[platform];
  if (!spec) throw new Error(`未知平台: ${platform}（可选: ${Object.keys(RUNTIMES).join(', ')}）`);

  const tmpDir = await mkdtemp(path.join(os.tmpdir(), 'querydoc-runtimes-'));
  try {
    await ensureRuntime(`onnxruntime(${platform})`, spec.ort, tmpDir);
    await ensureRuntime(`pdfium(${platform})`, spec.pdfium, tmpDir);
  } finally {
    // 清理临时目录（大归档）
    const { rm } = await import('node:fs/promises');
    await rm(tmpDir, { recursive: true, force: true });
  }
  console.log('[runtimes] 全部就绪');
}

main().catch((err) => {
  console.error('[runtimes] 执行失败:', err);
  process.exit(1);
});
