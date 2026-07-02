# README.md の入力フォーマット記述が実装と齟齬しユーザーを誤誘導する

- Priority: Medium
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/doc-fix-readme-input-format-mismatch
- Polished: 2026-07-02

## 目的

`README.md:31` の特徴一覧は「エンコード入力フォーマット選択 (NV12 / I420 / YV12 / BGRA / P010)」と 5 種類を列挙しているが、実装（`src/encode.rs:104-112` の `enum FrameFormat`）は `Nv12 / Yuy2 / Bgra` の 3 種類のみ。同じ README.md 内でも「サポートフォーマット」表（`README.md:194-200`）とは矛盾する。ユーザーを誤誘導するため実装に合わせて修正する。

## 優先度根拠

Medium。以下による。

- **公開ドキュメント（README.md）の誤情報**: crates.io にも表示されるため、実装と一致しないと利用者の初期評価を誤らせる。
- **同一 README 内での自己矛盾**: 「特徴」セクションと「サポートフォーマット」表で異なる情報を出しており、どちらが正しいのか判断できない。
- **修正コスト極小**: 1 行の修正のみ。
- **緊急性は低い**: 実装コード自体の破損はなく、README を読んだユーザーが「あれ？」となる程度。Priority を High にする理由はない。

## 現状

### 問題箇所

`README.md:31`（「特徴」セクション内）:

```
- エンコード入力フォーマット選択 (NV12 / I420 / YV12 / BGRA / P010)
```

`I420` / `YV12` / `P010` は実装に存在しない。

### 実装

`src/encode.rs:104-112`:

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

`README.md:194-200` の「サポートフォーマット」表:

```markdown
### エンコード入力フォーマット (`FrameFormat`)

| フォーマット | `FrameFormat` | 説明 |
|---|---|---|
| NV12 | `FrameFormat::Nv12` | Semi-Planar YUV 4:2:0 8bit |
| YUY2 | `FrameFormat::Yuy2` | Packed YUV 4:2:2 8bit |
| BGRA | `FrameFormat::Bgra` | Packed BGRA 8bit |
```

こちらは実装と一致している。したがって L31 の記述だけが古い（過去に I420 / YV12 / P010 のサポートを予定していた形跡と思われる）。

### `docs/INTEL_VPL.md` にも同じ齟齬

`docs/INTEL_VPL.md:20-25` にも同じ誤りがあるが、そちらは全面老朽化のため別 issue（0016）で削除する。

## 設計方針

`README.md:31` を実装と表に合わせて修正する。

```diff
- - エンコード入力フォーマット選択 (NV12 / I420 / YV12 / BGRA / P010)
+ - エンコード入力フォーマット選択 (NV12 / YUY2 / BGRA)
```

## 完了条件

以下すべてを満たす。

1. `README.md:31` の記述が「NV12 / YUY2 / BGRA」に修正される。
2. `README.md` 全体を再確認し、他に類似の齟齬がないことを確認する。
3. `CHANGES.md` の `## develop` に `[UPDATE]` として追記する。

## 影響範囲

- `README.md`（L31 のみ）
- `CHANGES.md`

## 参考

- 関連 issue: 0016（`docs/INTEL_VPL.md` にも同じ齟齬あり）
