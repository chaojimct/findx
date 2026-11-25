# 内容全文拼音检索（实现记录，2026-11-25）

GUI 全文/混合模式输入拼音即可召回中文正文。链路：GUI `content:` 改写 →
service `MergedContentSearch`（主库精确在前 + 辅库拼音补后）→ 辅库命中标 `拼` →
摘要拼音对齐标热。

## 辅库

- 独立边车：`<index>.content.pinyin-initials` / `.pinyin-full`，主库倒排 walk 回填
  （`build_aux_from_index`），增量同写（`pinyin_texts`），缺库/开关关闭自动降级。
- 回填曾按字节长度比较 token 位置（`rebuild_chars`），每篇只剩首个中文词；
  已改为字符单位 + 同位取最长，单测用中部音节（`ceshi`）回归网锁住。

## 查询语义（`search_pinyin_for_full`）

| 输入 | 语义 |
|---|---|
| `qw`（首字母串） | initials 侧 Term + 前缀正则 |
| `ma`（单音节） | 全拼侧精确 sip |
| `quanwen`（连写） | 切音节 → 笛卡尔变体（≤8）phrase 或；超限回落精确 phrase |
| `quanw`（半截尾音） | heads AND + 尾部前缀正则 |
| `quan wen`（空格） | OR（兼容"拼音+英文"混合） |
| 模糊音 | 平翘舌（zh/z、ch/c、sh/s）+ 前后鼻（in/ing、en/eng、an/ang），查询侧扩展 |
| 非 ASCII | 走主库中文分词，无拼音概念 |

## 摘要高亮

`match_spans` 字面优先；字面为空时拼音对齐 CJK 字串（单音节/连写/首字母/
模糊四口径，扫描定界正文前 1MB）。`query_terms` 过滤纯标点/空白词
（jieba search 模式会吐空格，曾报出 1/131631）。

## 性能

- 打标缓存：合并查询顺手填辅库命中集（limit 覆盖才命中），免重复两次
  TopDocs(50000)；重复查询实测 3~5ms。
- `filter_content` 求交并行化；连写改严格连续后召回量级下降两个数量级。
- 参考耗时（dev 调试构建，35 万篇内容库）：`content:machuntian` 冷查 ~360ms。

## 状态上报

`StatusResult.content_dir`（service 实际挂载路径，不再从 GUI indexPath 推导）+
`content_aux_{ini,full}_{docs,bytes}`；设置页全文 tab 展示"拼音辅库（首字母/全拼）"
行，无库明示降级。
