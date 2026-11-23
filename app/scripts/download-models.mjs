#!/usr/bin/env node
/**
 * OCR 模型下载与校验脚本（清单锁定模型版本与 sha256）
 *
 * 用法：node app/scripts/download-models.mjs
 *
 * 行为：
 *  - 读取 app/src-tauri/resources/ocr-models/models.toml
 *  - 逐个 [[model]] 下载到 dest_dir（默认 files/）
 *  - 已存在且 sha256 匹配则跳过；url 为空（待 Spike 1.1 回填）则跳过并提示
 *  - 下载后校验 sha256，不匹配则删除并报错退出
 *
 * 仅依赖 Node >= 22 内置能力（fetch / crypto），无第三方包。
 */
import { createHash } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { mkdir, stat, unlink } from 'node:fs/promises';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const MANIFEST = path.join(__dirname, '..', 'src-tauri', 'resources', 'ocr-models', 'models.toml');

/** 解析本清单固定结构：[meta] 的 dest_dir + 若干 [[model]] 块（key = "value"） */
function parseManifest(text) {
  const models = [];
  let meta = {};
  let current = null;
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    if (line === '[meta]') { current = null; continue; }
    if (line === '[[model]]') { current = {}; models.push(current); continue; }
    const m = line.match(/^(\w+)\s*=\s*"(.*)"\s*$/);
    if (!m) continue;
    if (current) current[m[1]] = m[2];
    else meta[m[1]] = m[2];
  }
  return { meta, models };
}

async function sha256Of(file) {
  const hash = createHash('sha256');
  const { createReadStream } = await import('node:fs');
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}

/** HuggingFace 端点：默认官方站；HF_ENDPOINT 环境变量可指向镜像
 * （如 https://hf-mirror.com，HF 直连受限的网络环境下使用）。 */
const HF_ENDPOINT = (process.env.HF_ENDPOINT || '').replace(/\/+$/, '');
function resolveUrl(u) {
  return HF_ENDPOINT ? u.replace(/^https:\/\/huggingface\.co/, HF_ENDPOINT) : u;
}

async function main() {
  const { readFile } = await import('node:fs/promises');
  const text = await readFile(MANIFEST, 'utf8');
  const { meta, models } = parseManifest(text);
  const destDir = path.join(path.dirname(MANIFEST), meta.dest_dir || 'files');
  await mkdir(destDir, { recursive: true });

  if (models.length === 0) {
    console.log('[models] 清单中暂无启用的模型条目（Phase 1.1 回填 models.toml 后重新运行）');
    return;
  }

  let failed = 0;
  for (const model of models) {
    const dest = path.join(destDir, model.file);
    if (!model.url) {
      console.warn(`[models] 跳过 ${model.name}：url 未填写`);
      continue;
    }
    const url = resolveUrl(model.url);
    try {
      await stat(dest);
      if (model.sha256 && (await sha256Of(dest)) === model.sha256.toLowerCase()) {
        console.log(`[models] 已存在且校验通过，跳过：${model.file}`);
        continue;
      }
      console.warn(`[models] 已存在但校验不符，重新下载：${model.file}`);
      await unlink(dest);
    } catch { /* 不存在则下载 */ }

    console.log(`[models] 下载 ${model.name} <- ${url}`);
    const res = await fetch(url);
    if (!res.ok || !res.body) {
      console.error(`[models] 下载失败 ${model.name}: HTTP ${res.status}`);
      failed++;
      continue;
    }
    await pipeline(Readable.fromWeb(res.body), createWriteStream(dest));

    if (model.sha256) {
      const actual = await sha256Of(dest);
      if (actual !== model.sha256.toLowerCase()) {
        await unlink(dest);
        console.error(`[models] sha256 校验失败 ${model.file}: 期望 ${model.sha256} 实际 ${actual}（已删除）`);
        failed++;
        continue;
      }
    }
    console.log(`[models] 完成：${model.file}`);
  }

  if (failed > 0) {
    console.error(`[models] ${failed} 个模型下载/校验失败`);
    process.exit(1);
  }
  console.log('[models] 全部就绪');
}

main().catch((err) => {
  console.error('[models] 执行失败:', err);
  process.exit(1);
});
