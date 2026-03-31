# 変更履歴

- UPDATE
  - 後方互換がある変更
- ADD
  - 後方互換がある追加
- CHANGE
  - 後方互換のない変更
- FIX
  - バグ修正

## develop

- [CHANGE] `FrameFormat::frame_size()` の戻り値を `usize` から `Option<usize>` に変更してオーバーフロー時に `None` を返すようにする
  - @voluntas
- [FIX] デコーダの `sync_and_collect` でサーフェスサイズ計算のオーバーフローを防御する
  - @voluntas

### misc


## 2025.1.0

**リリース日**: 2026-03-31
