# README.md の入力フォーマット記述が実装と齟齬しユーザーを誤誘導する

- Priority: Medium
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/update-readme-input-format-mismatch
- Polished: 2026-08-02

## 目的

`README.md` の「特徴」セクションの「エンコード入力フォーマット選択 (NV12 / I420 / YV12 / BGRA / P010)」は 5 種類を列挙しているが、実装（`src/encode.rs` の `FrameFormat` enum）は `Nv12 / Yuy2 / Bgra` の 3 種類のみ。同じ README.md 内でも「サポートフォーマット」表とは矛盾する。ユーザーを誤誘導するため実装に合わせて修正する。

## 優先度根拠

Medium。以下による。

- **公開ドキュメント（README.md）の誤情報**: crates.io にも表示されるため、実装と一致しないと利用者の初期評価を誤らせる。
- **同一 README 内での自己矛盾**: 「特徴」セクションと「サポートフォーマット」表で異なる情報を出しており、どちらが正しいのか判断できない。
- **修正差分は 1 行のみ**: 修正そのものは 1 行（README 全体の再確認は検証作業）。
- **緊急性は低い**: 実装コード自体の破損はなく、README を読んだユーザーが「あれ？」となる程度。Priority を High にする理由はない。

## 現状

### 問題箇所

`README.md` の「特徴」セクション内:

```
- エンコード入力フォーマット選択 (NV12 / I420 / YV12 / BGRA / P010)
```

`I420` / `YV12` / `P010` は実装に存在しない。

### 実装

`src/encode.rs` の `FrameFormat` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFormat {
    /// Semi-Planar YUV 4:2:0 8bit [Y plane + interleaved UV plane]
    Nv12,
    /// Packed YUV 4:2:2 8bit [YUYV interleaved]
    Yuy2,
    /// Packed BGRA 8bit
    Bgra,
}
```

Nv12 / Yuy2 / Bgra の 3 種のみ。

### 同一 README 内の別セクションとの矛盾

`README.md` の「サポートフォーマット」の「エンコード入力フォーマット (`FrameFormat`)」表:

```markdown
### エンコード入力フォーマット (`FrameFormat`)

| フォーマット | `FrameFormat` | 説明 |
|---|---|---|
| NV12 | `FrameFormat::Nv12` | Semi-Planar YUV 4:2:0 8bit |
| YUY2 | `FrameFormat::Yuy2` | Packed YUV 4:2:2 8bit |
| BGRA | `FrameFormat::Bgra` | Packed BGRA 8bit |
```

こちらは実装と一致している。したがって「特徴」セクションの記述だけが古い（git 履歴を確認すると、5 フォーマット表記は初回インポートコミットから存在し、`FrameFormat` enum は常に 3 種であるため、過去にサポートを予定していた形跡ではなくインポート当初からの誤記である）。

### `docs/INTEL_VPL.md` にも同じ齟齬

`docs/INTEL_VPL.md` の入力フレームフォーマット表にも同じ誤りがあるが、そちらは全面老朽化のため別 issue（0016）で削除する（本 issue では触れない）。

## 設計方針

`README.md` の「特徴」セクションの「エンコード入力フォーマット選択」の行を実装と表に合わせて修正する。

```diff
- - エンコード入力フォーマット選択 (NV12 / I420 / YV12 / BGRA / P010)
+ - エンコード入力フォーマット選択 (NV12 / YUY2 / BGRA)
```

## 完了条件

以下すべてを満たす。

1. `README.md` の「特徴」セクションの「エンコード入力フォーマット選択」の行が「NV12 / YUY2 / BGRA」に修正される。
2. `README.md` の入力フォーマット関連の記述を再確認し、他に類似の齟齬（`FrameFormat` の列挙と実装との不一致）がないことを確認する。確認は「サポートフォーマット」表（`FrameFormat` enum と一致していることを確認済み）と「デコード出力フォーマット」表（NV12 固定。`src/decode.rs` の `MFX_FOURCC_NV12` と一致）を対象とする。確認中に類似の齟齬を発見した場合は本 issue のスコープ外とし、別 issue で対応する。
3. `CHANGES.md` には追記しない（`.rst` / `.md` ファイルの変更は変更履歴に反映しない、という `shiguredo-changelog` 規約による）。

## 影響範囲

- `README.md`（「特徴」セクションの「エンコード入力フォーマット選択」の行のみ）

## 参考

- 関連 issue: 0016（`docs/INTEL_VPL.md` の同じ齟齬。本 issue では触れず 0016 の削除に委ねる）
- 関連 issue: 0015（`README.md` の docs.rs 向けビルド記述を「Linux 上で」に限定する。同じファイルを触るが修正箇所が異なるため独立して適用可能）
