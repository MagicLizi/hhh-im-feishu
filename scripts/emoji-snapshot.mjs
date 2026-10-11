// 生成飞书客户端专有表情与中文名的随包快照 src/feishu/emoji_extra.json（契约 hhh-im client v15 §2）。
//
// 开放平台文档只列可用于表情回复的一部分表情（没有中文名）；飞书客户端里其余的表情与全部中文名在飞书文档网页的
// 公开前端资源里。这两份资源的地址按版本与语言带指纹、找不到稳定入口，所以不在运行时拉：飞书出了新表情时，
// 从飞书文档网页（浏览器开发者工具的网络面板）找到当前的两份资源地址，重跑本脚本，随 HHH 更新。
//
// 用法：node scripts/emoji-snapshot.mjs --map <类型→图片名的 JS 地址> --names <中文名的 JS 地址> [--out <路径>] [--sampled <日期>]
//   --map    含 `THUMBSUP:"emoji_thumbsup_v2.png"` 这类对照的文件（如 …/docx/module/ud-biz-react-emoji_es6.<指纹>.chunk.js）
//   --names  含 `Emoji_Thumbsup:"赞"` 这类对照的文件（如 …/docx/module/dx_63567_es6.<指纹>.chunk.js，中文版）
// 只发不带身份的 GET；每张补上的表情图都核对在图片 CDN 上读得到。写出的快照里“panel”默认按“进面板”，
// 肤色版本与测试群实测点不上的（NOT_REACTABLE）不进面板：它们照样显示，只是面板里不给选。
// 新补上的表情先在测试群逐个点上、读回、撤掉（hhh-friend live_probe 的 live_probe_feishu_reaction_write），
// 点不上的加进 NOT_REACTABLE 再重跑（friend/im-watch v59 验收记录）。

import { writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const args = Object.fromEntries(process.argv.slice(2).reduce((pairs, arg, i, all) => (arg.startsWith('--') ? [...pairs, [arg.slice(2), all[i + 1]]] : pairs), []));
if (!args.map || !args.names) throw new Error('用法：--map <类型→图片名的 JS 地址> --names <中文名的 JS 地址> [--out <路径>] [--sampled <日期>]');
const out = resolve(args.out ?? resolve(here, '../src/feishu/emoji_extra.json'));
const sampled = args.sampled ?? new Date().toISOString().slice(0, 10);
/** 开放平台“表情文案说明”（与 src/feishu/emoji.rs 读的是同一篇）。 */
const CATALOG = 'https://open.feishu.cn/document_portal/v1/document/get_detail?fullPath=/server-docs/im-v1/message-reaction/emojis-introduce';
/** 表情图所在的 CDN 目录。 */
const IMAGE_BASE = 'https://sf3-ttcdn-tos.pstatp.com/obj/lark-reaction-cn/';
/** 肤色：飞书的写法（接口类型的前缀、中文名表里的后缀）与中文。 */
const TONES = [['MediumLight', '中等浅色'], ['MediumDark', '中等深色'], ['Light', '浅色'], ['Medium', '中等色'], ['Dark', '深色']];
/** 肤色版本的基底在中文名表里的叫法与接口不同的几个。 */
const TONE_BASE_NAMES = { Thumbsup: 'Like', Applaud: 'Clap', Shake: 'Handshake', ThumbsDown: 'Dislike' };
/** 图片名与接口类型都对不上中文名表的：中文名表里的叫法（2026-09-30 逐个核对图片）。 */
const NAME_ALIASES = { MUSCLE: 'Fighting', SiSiASYouWish: 'Snake2025', BeAtTheForefront: 'Horse2026', LIPS: 'Lips_new' };
/** 飞书不让点成表情回复的（2026-09-30 在测试群逐个点，接口回 400 + 231001）：只在文字里出现，不进面板。 */
const NOT_REACTABLE = new Set(['ATTENTION', 'WELLDONE', 'FOLLOWME', 'DETERGENT', 'AWESOME', 'GOODJOB', 'PursueUltimate', 'CustomerSuccess', 'Responsible', 'Ambitious']);

async function text(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url} → ${response.status}`);
  return response.text();
}

// 1. 开放平台目录：官方表情（顺序与图片地址），与 feishu/emoji.rs 同样按 <md-td> 单元格读。
const detail = JSON.parse(await text(CATALOG));
const cells = [...detail.data.content.matchAll(/<md-td[^>]*>([\s\S]*?)<\/md-td>/g)].map((m) => m[1].trim());
const official = [];
for (let i = 0; i + 1 < cells.length; i++) {
  const image = cells[i].match(/!\[[^\]]*\]\(([^)\s]+)/);
  if (!image || /!\[/.test(cells[i + 1])) continue;
  official.push({ type: cells[i + 1].trim(), url: image[1].split('?')[0] });
  i++;
}
if (official.length < 150) throw new Error(`开放平台目录只读到 ${official.length} 个表情，文档格式可能变了`);

// 2. 类型 → 图片名；3. 中文名。
const mapJs = await text(args.map);
const images = new Map();
// 同一文件里别的对象也有“键:"emoji_….png"”（imageKey、defaultImage、按语言的 CN / US）：表情类型大写或数字开头，
// 两个大写字母的是语言代码，都不算。
const isEmojiType = (key) => /^[A-Z0-9]/.test(key) && !/^[A-Z]{2}$/.test(key);
for (const [, type, file] of mapJs.matchAll(/"?([A-Za-z0-9_]+)"?:"(emoji_[a-z0-9_]+\.png)"/g)) if (isEmojiType(type) && !images.has(type)) images.set(type, file);
const namesJs = await text(args.names);
const names = new Map();
for (const [, key, name] of namesJs.matchAll(/Emoji_([A-Za-z0-9_]+):"([^"]+)"/g)) names.set(key.toLowerCase(), name);
if (images.size < 200 || names.size < 200) throw new Error(`对照太少：图片 ${images.size}、中文名 ${names.size}，地址可能不对`);

// 中文名：先按图片名（显示出来的就是这张图；BLUSH 的图是 grin“呲牙”、XBLUSH 的图是 blush“脸红”，飞书内部叫法互换），
// 再按接口类型，再按别名表；肤色版本按“基底 + 肤色”。
const stem = (file) => file.replace(/^emoji_/, '').replace(/\.png$/, '').replace(/_v\d+$/, '').replace(/_(zh|en)$/, '').replaceAll('_', '');
// 肤色前缀大小写不统一（MediumLightThumbsup、MediumlightYouAreTheBest 都有），不分大小写认。
const toneOf = (type) => {
  for (const [word, cn] of TONES) {
    if (type.toLowerCase().startsWith(word.toLowerCase()) && /^[A-Z]/.test(type.slice(word.length))) return { word, cn, base: type.slice(word.length) };
  }
  return null;
};
const nameOf = (type, file) => {
  const tone = toneOf(type);
  if (tone) {
    const base = TONE_BASE_NAMES[tone.base] ?? tone.base;
    const baseName = names.get(base.toLowerCase()) ?? names.get(tone.base.toLowerCase());
    return names.get(`${base}${tone.word}`.toLowerCase()) ?? (baseName ? `${baseName}（${tone.cn}）` : null);
  }
  const fromImage = file ? names.get(stem(file)) : undefined;
  return fromImage ?? names.get(type.toLowerCase()) ?? (NAME_ALIASES[type] ? names.get(NAME_ALIASES[type].toLowerCase()) : undefined) ?? null;
};

const officialTypes = new Set(official.map((e) => e.type));
const fileOfUrl = (url) => url.match(/\/(emoji_[a-z0-9_]+\.png)$/)?.[1] ?? null;
const officialNames = Object.fromEntries(official.map((e) => [e.type, nameOf(e.type, images.get(e.type) ?? fileOfUrl(e.url))]).filter(([, name]) => name));
const extras = [];
for (const [type, file] of images) {
  if (officialTypes.has(type)) continue;
  const head = await fetch(IMAGE_BASE + file, { method: 'HEAD' });
  if (!head.ok || !(head.headers.get('content-type') ?? '').startsWith('image/')) {
    console.warn(`跳过：${type} 的图 ${file} 读不到（${head.status}）`);
    continue;
  }
  const tone = toneOf(type);
  extras.push({ type, image: file, name: nameOf(type, file), panel: !tone && !NOT_REACTABLE.has(type), tone: tone?.cn ?? null });
}

const snapshot = {
  // 随包快照：运行时不改它；飞书出了新表情时按文件头的说明重新生成。
  sampled,
  sources: { catalog: CATALOG, map: args.map, names: args.names, imageBase: IMAGE_BASE },
  names: officialNames,
  extras,
};
await writeFile(out, `${JSON.stringify(snapshot, null, 1)}\n`, 'utf8');
const unnamed = [...extras.filter((e) => !e.name).map((e) => e.type), ...official.filter((e) => !officialNames[e.type]).map((e) => e.type)];
console.log(`官方 ${official.length} 个（有中文名 ${Object.keys(officialNames).length}），补上 ${extras.length} 个（进面板 ${extras.filter((e) => e.panel).length}、肤色 ${extras.filter((e) => e.tone).length}）`);
console.log(`没有中文名：${unnamed.join('、') || '无'}`);
console.log(`已写到 ${out}`);
